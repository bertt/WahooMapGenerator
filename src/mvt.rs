//! Decodes mapsforge tiles on demand and encodes them as Mapbox Vector Tiles for the web viewer.
//!
//! Two layers are produced: `poi` (points) and `way` (lines and polygons). Every OSM tag of a
//! feature becomes a string property, so styles can match on `["get", "highway"]` and so on.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use anyhow::{Result, ensure};

use crate::clip::clip_way;
use crate::model::{Point, Poi, Way};
use crate::reader::{Header, SubFile, SubIndex, read_header, read_index, read_one_tile};
use crate::tile::{BBox, Tile, lat_to_y, lon_to_x};

/// Tile coordinate range of the encoded geometry.
pub const EXTENT: i32 = 4096;
const CACHED_TILES: usize = 8;
/// Geometry is clipped to the tile plus this fraction of its size on every side.
const BUFFER: f64 = 1.0 / 16.0;

type FileTile = Rc<(Vec<Poi>, Vec<Way>)>;

/// A mapsforge file held in memory; tiles are decoded when requested.
pub struct MapFile {
    data: Vec<u8>,
    pub header: Header,
    indexes: Vec<SubIndex>,
    poi_tags: Vec<(String, String)>,
    way_tags: Vec<(String, String)>,
    cache: VecDeque<((usize, usize), FileTile)>,
}

fn split_tag(tag: &str) -> (String, String) {
    let (k, v) = tag.split_once('=').unwrap_or((tag, ""));
    (k.to_string(), v.to_string())
}

impl MapFile {
    pub fn from_bytes(data: Vec<u8>) -> Result<MapFile> {
        let header = read_header(&data)?;
        ensure!(header.version == 3, "unsupported map version {}", header.version);
        ensure!(!header.debug, "map files with debug signatures are not supported");
        ensure!(!header.subfiles.is_empty(), "map has no sub files");
        let indexes = header.subfiles.iter().map(|s| read_index(&data, &header, s)).collect::<Result<_>>()?;
        let poi_tags = header.poi_tags.iter().map(|t| split_tag(t)).collect();
        let way_tags = header.way_tags.iter().map(|t| split_tag(t)).collect();
        Ok(MapFile { data, header, indexes, poi_tags, way_tags, cache: VecDeque::new() })
    }

    fn sub_for_zoom(&self, zoom: u8) -> usize {
        self.header.subfiles.iter().position(|s| (s.min_zoom..=s.max_zoom).contains(&zoom)).unwrap_or_else(|| {
            let last = self.header.subfiles.len() - 1;
            if zoom < self.header.subfiles[0].min_zoom { 0 } else { last }
        })
    }

    fn file_tile(&mut self, sub: usize, i: usize) -> Result<FileTile> {
        if let Some((_, tile)) = self.cache.iter().find(|(key, _)| *key == (sub, i)) {
            return Ok(Rc::clone(tile));
        }
        let decoded = Rc::new(read_one_tile(&self.data, &self.header.subfiles[sub], &self.indexes[sub], i)?);
        if self.cache.len() == CACHED_TILES {
            self.cache.pop_front();
        }
        self.cache.push_back(((sub, i), Rc::clone(&decoded)));
        Ok(decoded)
    }

    /// Encodes XYZ tile `z/x/y` as a vector tile; empty when the map has no data there.
    pub fn tile(&mut self, z: u8, x: u32, y: u32) -> Result<Vec<u8>> {
        ensure!(Tile { zoom: z, x, y }.is_valid(), "tile {z}/{x}/{y} does not exist");
        let si = self.sub_for_zoom(z);
        let sub: SubFile = self.header.subfiles[si].clone();
        let grid = self.indexes[si].grid;
        let (xs, ys) = if z >= sub.base_zoom {
            let shift = z - sub.base_zoom;
            ((x >> shift)..=(x >> shift), (y >> shift)..=(y >> shift))
        } else {
            let shift = sub.base_zoom - z;
            ((x << shift)..=(((x + 1) << shift) - 1), (y << shift)..=(((y + 1) << shift) - 1))
        };

        let area = Tile { zoom: z, x, y }.bbox();
        let (dlat, dlon) = (
            (f64::from(area.max_lat - area.min_lat) * BUFFER) as i32,
            (f64::from(area.max_lon - area.min_lon) * BUFFER) as i32,
        );
        let clip_box = BBox {
            min_lat: area.min_lat - dlat,
            min_lon: area.min_lon - dlon,
            max_lat: area.max_lat + dlat,
            max_lon: area.max_lon + dlon,
        };

        let mut pois = LayerBuilder::new("poi");
        let mut ways = LayerBuilder::new("way");
        for fy in ys {
            for fx in xs.clone() {
                let Some(i) = grid.index_of(i64::from(fx), i64::from(fy)) else { continue };
                let contents = self.file_tile(si, i)?;
                for poi in contents.0.iter().filter(|p| p.zoom <= z && clip_box.contains(p.point.lat, p.point.lon)) {
                    let mut props = tag_props(&poi.tags, &self.poi_tags);
                    props.extend(optional(&[("name", &poi.name), ("addr:housenumber", &poi.house_number)]));
                    let mut ints = vec![];
                    ints.extend(poi.elevation.map(|e| ("ele", i64::from(e))));
                    ints.push(("layer", i64::from(poi.layer)));
                    let (px, py) = project(poi.point, z, x, y);
                    pois.add(1, point_geometry(px, py), &props, &ints);
                }
                for way in contents.1.iter().filter(|w| w.zoom <= z) {
                    let Some(bounds) = way.bounds() else { continue };
                    if !bounds.intersects(&clip_box) {
                        continue;
                    }
                    let Some(clipped) = clip_way(way, &clip_box) else { continue };
                    let mut props = tag_props(&way.tags, &self.way_tags);
                    props.extend(optional(&[("name", &way.name), ("ref", &way.reference), ("addr:housenumber", &way.house_number)]));
                    let ints = [("layer", i64::from(way.layer))];
                    let (polygons, lines) = split_rings(&clipped, z, x, y);
                    if let Some(geometry) = polygon_geometry(&polygons) {
                        ways.add(3, geometry, &props, &ints);
                    }
                    if let Some(geometry) = line_geometry(&lines) {
                        ways.add(2, geometry, &props, &ints);
                    }
                }
            }
        }
        let mut out = Vec::new();
        for layer in [pois, ways] {
            layer.write(&mut out);
        }
        Ok(out)
    }
}

fn tag_props(ids: &[u32], table: &[(String, String)]) -> Vec<(String, String)> {
    ids.iter().filter_map(|id| table.get(*id as usize).cloned()).collect()
}

fn optional(pairs: &[(&str, &Option<String>)]) -> Vec<(String, String)> {
    pairs.iter().filter_map(|(k, v)| v.as_ref().map(|v| (k.to_string(), v.clone()))).collect()
}

fn project(p: Point, z: u8, x: u32, y: u32) -> (i32, i32) {
    let px = (lon_to_x(f64::from(p.lon) / 1e6, z) - f64::from(x)) * f64::from(EXTENT);
    let py = (lat_to_y(f64::from(p.lat) / 1e6, z) - f64::from(y)) * f64::from(EXTENT);
    (px.round() as i32, py.round() as i32)
}

type Ring = Vec<(i32, i32)>;

/// Projects the rings; closed rings (at least four points) become polygon rings, the rest lines.
fn split_rings(way: &Way, z: u8, x: u32, y: u32) -> (Vec<Ring>, Vec<Ring>) {
    let (mut polygons, mut lines) = (Vec::new(), Vec::new());
    for ring in &way.rings {
        let closed = ring.len() >= 4 && ring.first() == ring.last();
        let mut points: Ring = ring.iter().map(|p| project(*p, z, x, y)).collect();
        points.dedup();
        if closed {
            if points.first() == points.last() {
                points.pop();
            }
            if points.len() >= 3 {
                polygons.push(points);
            }
        } else if points.len() >= 2 {
            lines.push(points);
        }
    }
    (polygons, lines)
}

fn area2(ring: &[(i32, i32)]) -> i64 {
    (0..ring.len())
        .map(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
            i64::from(a.0) * i64::from(b.1) - i64::from(b.0) * i64::from(a.1)
        })
        .sum()
}

fn command(id: u32, count: u32) -> u32 {
    (count << 3) | id
}

fn zigzag(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}

fn point_geometry(x: i32, y: i32) -> Vec<u32> {
    vec![command(1, 1), zigzag(x), zigzag(y)]
}

fn path_geometry(rings: &[Ring], close: bool) -> Vec<u32> {
    let mut out = Vec::new();
    let mut cursor = (0, 0);
    for ring in rings {
        out.push(command(1, 1));
        out.extend([zigzag(ring[0].0 - cursor.0), zigzag(ring[0].1 - cursor.1)]);
        cursor = ring[0];
        out.push(command(2, ring.len() as u32 - 1));
        for p in &ring[1..] {
            out.extend([zigzag(p.0 - cursor.0), zigzag(p.1 - cursor.1)]);
            cursor = *p;
        }
        if close {
            out.push(command(7, 1));
        }
    }
    out
}

fn line_geometry(lines: &[Ring]) -> Option<Vec<u32>> {
    (!lines.is_empty()).then(|| path_geometry(lines, false))
}

/// The first ring is the exterior (positive area), the others are holes (negative area).
fn polygon_geometry(rings: &[Ring]) -> Option<Vec<u32>> {
    let mut oriented = Vec::new();
    for (i, ring) in rings.iter().enumerate() {
        let area = area2(ring);
        if area == 0 {
            if i == 0 {
                return None;
            }
            continue;
        }
        let mut ring = ring.clone();
        if (area > 0) != (i == 0) {
            ring.reverse();
        }
        oriented.push(ring);
    }
    (!oriented.is_empty()).then(|| path_geometry(&oriented, true))
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_field(out: &mut Vec<u8>, field: u32, payload: &[u8]) {
    put_varint(out, u64::from(field << 3 | 2));
    put_varint(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

fn packed(values: impl IntoIterator<Item = u32>) -> Vec<u8> {
    let mut out = Vec::new();
    for v in values {
        put_varint(&mut out, u64::from(v));
    }
    out
}

struct LayerBuilder {
    name: &'static str,
    keys: Vec<String>,
    key_ids: HashMap<String, u32>,
    values: Vec<Vec<u8>>,
    value_ids: HashMap<Vec<u8>, u32>,
    features: Vec<Vec<u8>>,
}

impl LayerBuilder {
    fn new(name: &'static str) -> Self {
        LayerBuilder { name, keys: vec![], key_ids: HashMap::new(), values: vec![], value_ids: HashMap::new(), features: vec![] }
    }

    fn key(&mut self, key: &str) -> u32 {
        if let Some(id) = self.key_ids.get(key) {
            return *id;
        }
        self.keys.push(key.to_string());
        self.key_ids.insert(key.to_string(), self.keys.len() as u32 - 1);
        self.keys.len() as u32 - 1
    }

    fn value(&mut self, encoded: Vec<u8>) -> u32 {
        if let Some(id) = self.value_ids.get(&encoded) {
            return *id;
        }
        self.values.push(encoded.clone());
        self.value_ids.insert(encoded, self.values.len() as u32 - 1);
        self.values.len() as u32 - 1
    }

    fn add(&mut self, kind: u64, geometry: Vec<u32>, props: &[(String, String)], ints: &[(&str, i64)]) {
        let mut tags = Vec::new();
        // A later duplicate key (e.g. two tags with one key) replaces the earlier one.
        let mut seen: HashMap<&str, usize> = HashMap::new();
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        for (k, v) in props {
            let (key, mut value) = (self.key(k), Vec::new());
            put_field(&mut value, 1, v.as_bytes());
            let value = self.value(value);
            match seen.get(k.as_str()) {
                Some(i) => pairs[*i] = (key, value),
                None => {
                    seen.insert(k, pairs.len());
                    pairs.push((key, value));
                }
            }
        }
        for (k, v) in ints {
            let key = self.key(k);
            let mut value = Vec::new();
            put_varint(&mut value, 4 << 3);
            put_varint(&mut value, *v as u64);
            pairs.push((key, self.value(value)));
        }
        for (k, v) in pairs {
            tags.extend([k, v]);
        }
        let mut feature = Vec::new();
        put_field(&mut feature, 2, &packed(tags));
        put_varint(&mut feature, 3 << 3);
        put_varint(&mut feature, kind);
        put_field(&mut feature, 4, &packed(geometry));
        self.features.push(feature);
    }

    fn write(self, out: &mut Vec<u8>) {
        if self.features.is_empty() {
            return;
        }
        let mut layer = Vec::new();
        put_varint(&mut layer, 15 << 3);
        put_varint(&mut layer, 2);
        put_field(&mut layer, 1, self.name.as_bytes());
        for f in &self.features {
            put_field(&mut layer, 2, f);
        }
        for k in &self.keys {
            put_field(&mut layer, 3, k.as_bytes());
        }
        for v in &self.values {
            put_field(&mut layer, 4, v);
        }
        put_varint(&mut layer, 5 << 3);
        put_varint(&mut layer, EXTENT as u64);
        put_field(out, 3, &layer);
    }
}
