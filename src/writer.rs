//! Writes a single-sub-file mapsforge (version 3) map for one XYZ tile.
//!
//! Layout follows the Wahoo tiles: tile size 256, Mercator, one sub file with base zoom
//! `tile zoom + 2` (16 tiles) covering zoom levels 0-17, no debug signatures.

use crate::clip::{clip_way, enlarge};
use crate::model::{Poi, Way};
use crate::reader::MAGIC;
use crate::tile::{BBox, Grid, Tile, tile_x_of, tile_y_of};
use crate::varint::{put_string, put_svar, put_uvar};

pub const MIN_ZOOM: u8 = 0;
pub const MAX_ZOOM: u8 = 17;
/// Difference between the requested tile zoom and the base zoom of the sub file.
pub const BASE_ZOOM_OFFSET: u8 = 2;
const TILE_SIZE: u16 = 256;
const FORMAT_VERSION: i32 = 3;
const WAYS_SUBTILE_GRID: i64 = 4;

/// Static properties of the map to write.
pub struct MapSpec<'a> {
    pub tile: Tile,
    pub poi_tags: &'a [String],
    pub way_tags: &'a [String],
    pub creation_ms: i64,
    pub created_by: &'a str,
}

/// Serialises the features into the bytes of a `.map` file.
pub fn build_map(spec: &MapSpec, pois: &[Poi], ways: &[Way]) -> Vec<u8> {
    let bbox = spec.tile.bbox();
    let grid = Grid::from_bbox(&bbox, spec.tile.zoom + BASE_ZOOM_OFFSET);
    let mut tile_pois: Vec<Vec<&Poi>> = vec![Vec::new(); grid.len()];
    let mut tile_ways: Vec<Vec<&Way>> = vec![Vec::new(); grid.len()];
    for poi in pois {
        let x = tile_x_of(poi.point.lon, grid.zoom);
        let y = tile_y_of(poi.point.lat, grid.zoom);
        if let Some(i) = grid.index_of(x, y) {
            tile_pois[i].push(poi);
        }
    }
    for way in ways {
        let Some(b) = way.bounds() else { continue };
        let (x0, x1) = (tile_x_of(b.min_lon, grid.zoom), tile_x_of(b.max_lon, grid.zoom));
        let (y0, y1) = (tile_y_of(b.max_lat, grid.zoom), tile_y_of(b.min_lat, grid.zoom));
        for y in y0..=y1 {
            for x in x0..=x1 {
                if let Some(i) = grid.index_of(x, y) {
                    tile_ways[i].push(way);
                }
            }
        }
    }

    let index_len = grid.len() * 5;
    let mut index = Vec::with_capacity(index_len);
    let mut body = Vec::new();
    for i in 0..grid.len() {
        let offset = (index_len + body.len()) as u64;
        index.extend_from_slice(&offset.to_be_bytes()[3..]);
        let tile = grid.tile_at(i);
        body.extend(encode_tile(tile, &tile_pois[i], &tile_ways[i]));
    }
    let mut subfile = index;
    subfile.extend(body);

    let header_len = encode_header(spec, &bbox, grid.zoom, 0, 0, 0).len();
    let file_size = (header_len + subfile.len()) as i64;
    let mut out = encode_header(spec, &bbox, grid.zoom, header_len as u64, subfile.len() as u64, file_size);
    out.extend(subfile);
    out
}

fn put_tag_list(buf: &mut Vec<u8>, tags: &[String]) {
    buf.extend_from_slice(&(tags.len() as u16).to_be_bytes());
    for tag in tags {
        put_string(buf, tag);
    }
}

fn encode_header(spec: &MapSpec, bbox: &BBox, base_zoom: u8, start: u64, size: u64, file_size: i64) -> Vec<u8> {
    let mut rest = Vec::new();
    rest.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
    rest.extend_from_slice(&file_size.to_be_bytes());
    rest.extend_from_slice(&spec.creation_ms.to_be_bytes());
    for v in [bbox.min_lat, bbox.min_lon, bbox.max_lat, bbox.max_lon] {
        rest.extend_from_slice(&v.to_be_bytes());
    }
    rest.extend_from_slice(&TILE_SIZE.to_be_bytes());
    put_string(&mut rest, "Mercator");
    rest.push(0x04); // flags: "created by" present
    put_string(&mut rest, spec.created_by);
    put_tag_list(&mut rest, spec.poi_tags);
    put_tag_list(&mut rest, spec.way_tags);
    rest.push(1);
    rest.extend_from_slice(&[base_zoom, MIN_ZOOM, MAX_ZOOM]);
    rest.extend_from_slice(&start.to_be_bytes());
    rest.extend_from_slice(&size.to_be_bytes());

    let mut out = Vec::with_capacity(rest.len() + 24);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(rest.len() as i32).to_be_bytes());
    out.extend(rest);
    out
}

fn bucket(zoom: u8) -> usize {
    usize::from(zoom.clamp(MIN_ZOOM, MAX_ZOOM) - MIN_ZOOM)
}

fn layer_byte(layer: i8, tag_count: usize) -> u8 {
    let stored = (i16::from(layer) + 5).clamp(0, 10) as u8;
    (stored << 4) | tag_count as u8
}

fn encode_tile(tile: Tile, pois: &[&Poi], ways: &[&Way]) -> Vec<u8> {
    let origin = tile.origin();
    let levels = usize::from(MAX_ZOOM - MIN_ZOOM) + 1;
    let clip_box = enlarge(&tile.bbox());
    let mut pois: Vec<&Poi> = pois.to_vec();
    let mut ways: Vec<Way> = ways.iter().filter_map(|w| clip_way(w, &clip_box)).collect();
    pois.sort_by_key(|p| bucket(p.zoom));
    ways.sort_by_key(|w| bucket(w.zoom));

    let mut poi_counts = vec![0u64; levels];
    let mut way_counts = vec![0u64; levels];
    let mut poi_data = Vec::new();
    for poi in &pois {
        poi_counts[bucket(poi.zoom)] += 1;
        encode_poi(&mut poi_data, poi, origin);
    }
    let mut way_data = Vec::new();
    for way in &ways {
        way_counts[bucket(way.zoom)] += 1;
        encode_way(&mut way_data, way, tile, origin);
    }

    let mut out = Vec::new();
    for level in 0..levels {
        put_uvar(&mut out, poi_counts[level]);
        put_uvar(&mut out, way_counts[level]);
    }
    put_uvar(&mut out, poi_data.len() as u64);
    out.extend(poi_data);
    out.extend(way_data);
    out
}

fn put_tags(buf: &mut Vec<u8>, tags: &[u32]) -> usize {
    let tags = &tags[..tags.len().min(crate::tagmap::MAX_TAGS)];
    for t in tags {
        put_uvar(buf, u64::from(*t));
    }
    tags.len()
}

fn encode_poi(out: &mut Vec<u8>, poi: &Poi, origin: (i32, i32)) {
    put_svar(out, i64::from(poi.point.lat) - i64::from(origin.0));
    put_svar(out, i64::from(poi.point.lon) - i64::from(origin.1));
    let mut tags = Vec::new();
    let count = put_tags(&mut tags, &poi.tags);
    out.push(layer_byte(poi.layer, count));
    out.extend(tags);
    let flags = u8::from(poi.name.is_some()) << 7
        | u8::from(poi.house_number.is_some()) << 6
        | u8::from(poi.elevation.is_some()) << 5;
    out.push(flags);
    if let Some(name) = &poi.name {
        put_string(out, name);
    }
    if let Some(number) = &poi.house_number {
        put_string(out, number);
    }
    if let Some(elevation) = poi.elevation {
        put_svar(out, i64::from(elevation));
    }
}

/// Bit mask of the 4x4 sub tiles (top-left is the most significant bit) that the way's box touches.
fn subtile_mask(way_box: &BBox, tile: Tile) -> u16 {
    let zoom = tile.zoom + 2;
    let (base_x, base_y) = (i64::from(tile.x) * WAYS_SUBTILE_GRID, i64::from(tile.y) * WAYS_SUBTILE_GRID);
    let (x0, x1) = (tile_x_of(way_box.min_lon, zoom) - base_x, tile_x_of(way_box.max_lon, zoom) - base_x);
    let (y0, y1) = (tile_y_of(way_box.max_lat, zoom) - base_y, tile_y_of(way_box.min_lat, zoom) - base_y);
    let mut mask = 0u16;
    for row in y0.max(0)..=y1.min(3) {
        for col in x0.max(0)..=x1.min(3) {
            mask |= 0x8000 >> (row * 4 + col);
        }
    }
    if mask == 0 { 0xffff } else { mask }
}

fn encode_way(out: &mut Vec<u8>, way: &Way, tile: Tile, origin: (i32, i32)) {
    let mut data = Vec::new();
    let mask = way.bounds().map_or(0xffff, |b| subtile_mask(&b, tile));
    data.extend_from_slice(&mask.to_be_bytes());
    let mut tags = Vec::new();
    let count = put_tags(&mut tags, &way.tags);
    data.push(layer_byte(way.layer, count));
    data.extend(tags);
    let flags = u8::from(way.name.is_some()) << 7
        | u8::from(way.house_number.is_some()) << 6
        | u8::from(way.reference.is_some()) << 5;
    data.push(flags);
    for text in [&way.name, &way.house_number, &way.reference].into_iter().flatten() {
        put_string(&mut data, text);
    }
    put_uvar(&mut data, way.rings.len() as u64);
    for ring in &way.rings {
        put_uvar(&mut data, ring.len() as u64);
        let mut prev = (origin.0, origin.1);
        for p in ring {
            put_svar(&mut data, i64::from(p.lat) - i64::from(prev.0));
            put_svar(&mut data, i64::from(p.lon) - i64::from(prev.1));
            prev = (p.lat, p.lon);
        }
    }
    put_uvar(out, data.len() as u64);
    out.extend(data);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Point;
    use crate::reader::{read_header, read_map};

    fn spec<'a>(poi_tags: &'a [String], way_tags: &'a [String]) -> MapSpec<'a> {
        MapSpec {
            tile: Tile { zoom: 8, x: 131, y: 84 },
            poi_tags,
            way_tags,
            creation_ms: 1_700_000_000_000,
            created_by: "test",
        }
    }

    fn pt(lat: i32, lon: i32) -> Point {
        Point { lat, lon }
    }

    #[test]
    fn pois_in_one_tile_are_ordered_by_zoom() {
        let tags = vec!["a=b".to_string()];
        let b = Tile { zoom: 8, x: 131, y: 84 }.bbox();
        let (lat, lon) = ((b.min_lat + b.max_lat) / 2, (b.min_lon + b.max_lon) / 2);
        let late = Poi { point: pt(lat, lon), zoom: 15, tags: vec![0], ..Poi::default() };
        let early = Poi { point: pt(lat + 1, lon), zoom: 11, tags: vec![0], ..Poi::default() };
        let bytes = build_map(&spec(&tags, &tags), &[late, early], &[]);
        let zooms: Vec<u8> = read_map(&bytes).unwrap().pois.iter().map(|p| p.zoom).collect();
        assert_eq!(zooms, [11, 15]);
    }

    #[test]
    fn truncated_tiles_are_reported_with_their_coordinates() {
        let tags = vec!["a=b".to_string()];
        let bytes = build_map(&spec(&tags, &tags), &[], &[]);
        let err = read_map(&bytes[..bytes.len() - 3]).unwrap_err();
        assert!(err.to_string().starts_with("tile 10/"));
    }

    #[test]
    fn empty_map_has_valid_header_and_tiles() {
        let poi_tags = vec!["a=b".to_string()];
        let way_tags = vec!["c=d".to_string(), "e=f".to_string()];
        let bytes = build_map(&spec(&poi_tags, &way_tags), &[], &[]);
        let header = read_header(&bytes).unwrap();
        assert_eq!(header.version, 3);
        assert_eq!(header.file_size as usize, bytes.len());
        assert_eq!(header.created_by.as_deref(), Some("test"));
        assert_eq!(header.poi_tags, poi_tags);
        assert_eq!(header.way_tags, way_tags);
        assert_eq!(header.bbox, Tile { zoom: 8, x: 131, y: 84 }.bbox());
        assert_eq!(header.subfiles.len(), 1);
        let sub = &header.subfiles[0];
        assert_eq!((sub.base_zoom, sub.min_zoom, sub.max_zoom), (10, 0, 17));
        assert_eq!(sub.start as usize + sub.size as usize, bytes.len());
        let map = read_map(&bytes).unwrap();
        assert!(map.pois.is_empty() && map.ways.is_empty());
    }

    #[test]
    fn features_round_trip() {
        let tags = vec!["a=b".to_string(), "c=d".to_string()];
        let b = Tile { zoom: 8, x: 131, y: 84 }.bbox();
        let (lat, lon) = ((b.min_lat + b.max_lat) / 2, (b.min_lon + b.max_lon) / 2);
        let poi = Poi {
            point: pt(lat, lon),
            layer: -2,
            tags: vec![1, 0],
            name: Some("Fontein".into()),
            house_number: Some("12".into()),
            elevation: Some(-5),
            zoom: 12,
        };
        let plain = Poi { point: pt(lat + 10, lon - 10), zoom: 30, tags: vec![0], ..Poi::default() };
        let way = Way {
            rings: vec![vec![pt(lat, lon), pt(lat + 1000, lon + 2000), pt(lat - 5, lon + 3)]],
            layer: 3,
            tags: vec![1],
            name: Some("Pad".into()),
            house_number: Some("1".into()),
            reference: Some("N1".into()),
            zoom: 9,
        };
        let multi = Way { rings: vec![vec![pt(lat, lon), pt(lat, lon + 1)], vec![pt(lat + 5, lon), pt(lat + 6, lon)]], tags: vec![0], zoom: 99, ..Way::default() };
        let degenerate = Way::default();
        let outside = Poi { point: pt(0, 0), ..Poi::default() };

        let bytes = build_map(&spec(&tags, &tags), &[poi.clone(), plain.clone(), outside], &[way.clone(), multi.clone(), degenerate]);
        let mut map = read_map(&bytes).unwrap();
        map.pois.sort_by_key(|p| p.zoom);
        map.ways.sort_by_key(|w| w.zoom);

        assert_eq!(map.pois.len(), 2);
        assert_eq!(map.pois[0], poi);
        assert_eq!(map.pois[1].zoom, 17);
        assert_eq!(map.pois[1].point, plain.point);
        assert_eq!(map.ways.len(), 2);
        assert_eq!(map.ways[0], way);
        assert_eq!(map.ways[1].rings, multi.rings);
        assert_eq!(map.ways[1].zoom, 17);
    }

    #[test]
    fn way_is_stored_in_every_tile_it_touches() {
        let tags = vec!["a=b".to_string()];
        let b = Tile { zoom: 8, x: 131, y: 84 }.bbox();
        let way = Way {
            rings: vec![vec![pt(b.min_lat + 100, b.min_lon + 100), pt(b.max_lat - 100, b.max_lon - 100)]],
            tags: vec![0],
            ..Way::default()
        };
        let bytes = build_map(&spec(&tags, &tags), &[], &[way]);
        let map = read_map(&bytes).unwrap();
        assert!(map.ways.len() >= 4);
    }

    #[test]
    fn ways_are_clipped_to_each_tile() {
        let tags = vec!["a=b".to_string()];
        let b = Tile { zoom: 8, x: 131, y: 84 }.bbox();
        let mid = (b.min_lat + b.max_lat) / 2;
        let way = Way { rings: vec![vec![pt(mid, b.min_lon + 100), pt(mid, b.max_lon - 100)]], tags: vec![0], ..Way::default() };
        let map = read_map(&build_map(&spec(&tags, &tags), &[], &[way])).unwrap();
        assert!(map.ways.len() >= 4);
        let span = |w: &Way| w.bounds().map(|x| x.max_lon - x.min_lon).unwrap();
        let total = (b.max_lon - b.min_lon) / 4;
        assert!(map.ways.iter().all(|w| span(w) < total + 2_000));
    }

    #[test]
    fn subtile_masks() {
        let tile = Tile { zoom: 8, x: 131, y: 84 };
        let b = tile.bbox();
        let whole = subtile_mask(&b, tile);
        assert_eq!(whole, 0xffff);
        let corner = BBox { min_lat: b.max_lat - 10, min_lon: b.min_lon + 10, max_lat: b.max_lat - 5, max_lon: b.min_lon + 20 };
        assert_eq!(subtile_mask(&corner, tile), 0x8000);
        let outside = BBox { min_lat: 0, min_lon: 0, max_lat: 1, max_lon: 1 };
        assert_eq!(subtile_mask(&outside, tile), 0xffff);
    }

    #[test]
    fn layer_and_bucket_clamping() {
        assert_eq!(layer_byte(0, 3), 0x53);
        assert_eq!(layer_byte(-9, 0), 0x00);
        assert_eq!(layer_byte(9, 15), 0xaf);
        assert_eq!(bucket(200), 17);
        assert_eq!(bucket(0), 0);
    }

    #[test]
    fn tag_lists_are_truncated_to_the_format_limit() {
        let mut buf = Vec::new();
        let tags: Vec<u32> = (0..20).collect();
        assert_eq!(put_tags(&mut buf, &tags), 15);
        assert_eq!(buf.len(), 15);
    }
}
