//! A reader for the subset of the mapsforge binary format (version 3) that is needed to
//! validate generated tiles and to analyse reference tiles.

use anyhow::{Context, Result, bail, ensure};

use crate::model::{Point, Poi, Way};
use crate::tile::{BBox, Grid};
use crate::varint::Cursor;

pub const MAGIC: &[u8; 20] = b"mapsforge binary OSM";

/// Description of one sub file (a block of tiles at one base zoom level).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubFile {
    pub base_zoom: u8,
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub start: u64,
    pub size: u64,
}

/// The parsed file header.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub version: i32,
    pub file_size: i64,
    pub creation_ms: i64,
    pub bbox: BBox,
    pub tile_size: u16,
    pub projection: String,
    pub debug: bool,
    pub created_by: Option<String>,
    pub poi_tags: Vec<String>,
    pub way_tags: Vec<String>,
    pub subfiles: Vec<SubFile>,
}

/// Everything stored in a map file, with coordinates made absolute.
#[derive(Clone, Debug)]
pub struct MapContents {
    pub header: Header,
    /// Entries per tile: ways that cross tile borders appear once per tile.
    pub pois: Vec<Poi>,
    pub ways: Vec<Way>,
}

/// Parses the file header.
pub fn read_header(data: &[u8]) -> Result<Header> {
    let mut c = Cursor::new(data, 0);
    ensure!(c.take(MAGIC.len())? == MAGIC, "not a mapsforge file");
    let _header_size = c.i32()?;
    let version = c.i32()?;
    let file_size = c.i64()?;
    let creation_ms = c.i64()?;
    let bbox = BBox {
        min_lat: c.i32()?,
        min_lon: c.i32()?,
        max_lat: c.i32()?,
        max_lon: c.i32()?,
    };
    let tile_size = c.u16()?;
    let projection = c.string()?;
    let flags = c.u8()?;
    if flags & 0x40 != 0 {
        c.take(8)?;
    }
    if flags & 0x20 != 0 {
        c.take(1)?;
    }
    if flags & 0x10 != 0 {
        c.string()?;
    }
    if flags & 0x08 != 0 {
        c.string()?;
    }
    let created_by = if flags & 0x04 != 0 { Some(c.string()?) } else { None };
    let poi_tags = read_tag_list(&mut c)?;
    let way_tags = read_tag_list(&mut c)?;
    let count = c.u8()?;
    let mut subfiles = Vec::new();
    for _ in 0..count {
        subfiles.push(SubFile {
            base_zoom: c.u8()?,
            min_zoom: c.u8()?,
            max_zoom: c.u8()?,
            start: c.i64()? as u64,
            size: c.i64()? as u64,
        });
    }
    Ok(Header {
        version,
        file_size,
        creation_ms,
        bbox,
        tile_size,
        projection,
        debug: flags & 0x80 != 0,
        created_by,
        poi_tags,
        way_tags,
        subfiles,
    })
}

fn read_tag_list(c: &mut Cursor) -> Result<Vec<String>> {
    let n = c.u16()?;
    (0..n).map(|_| c.string()).collect()
}

/// Parses the header and decodes every tile of every sub file.
pub fn read_map(data: &[u8]) -> Result<MapContents> {
    let header = read_header(data)?;
    ensure!(header.version == 3, "unsupported map version {}", header.version);
    ensure!(!header.debug, "map files with debug signatures are not supported");
    let mut pois = Vec::new();
    let mut ways = Vec::new();
    for sub in &header.subfiles {
        read_subfile(data, &header, sub, &mut pois, &mut ways)?;
    }
    Ok(MapContents { header, pois, ways })
}

fn read_subfile(
    data: &[u8],
    header: &Header,
    sub: &SubFile,
    pois: &mut Vec<Poi>,
    ways: &mut Vec<Way>,
) -> Result<()> {
    let index = read_index(data, header, sub)?;
    for i in 0..index.grid.len() {
        let (p, w) = read_one_tile(data, sub, &index, i)?;
        pois.extend(p);
        ways.extend(w);
    }
    Ok(())
}

/// The tile grid of a sub file and the byte offset of every tile (relative to the sub file start).
#[derive(Clone, Debug)]
pub struct SubIndex {
    pub grid: Grid,
    offsets: Vec<usize>,
}

/// Reads the tile index of a sub file.
pub fn read_index(data: &[u8], header: &Header, sub: &SubFile) -> Result<SubIndex> {
    let grid = Grid::from_bbox(&header.bbox, sub.base_zoom);
    let mut index = Cursor::new(data, sub.start as usize);
    let mut offsets = Vec::with_capacity(grid.len());
    for _ in 0..grid.len() {
        let raw = index.take(5)?.iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
        offsets.push((raw & 0x7f_ffff_ffff) as usize);
    }
    Ok(SubIndex { grid, offsets })
}

/// Decodes one tile of a sub file, given its row-major index in the grid.
pub fn read_one_tile(data: &[u8], sub: &SubFile, index: &SubIndex, i: usize) -> Result<(Vec<Poi>, Vec<Way>)> {
    let tile = index.grid.tile_at(i);
    let offset = *index.offsets.get(i).context("tile index out of range")?;
    let mut c = Cursor::new(data, sub.start as usize + offset);
    let (mut pois, mut ways) = (Vec::new(), Vec::new());
    read_tile(&mut c, sub, tile.origin(), &mut pois, &mut ways)
        .with_context(|| format!("tile {}/{}/{}", tile.zoom, tile.x, tile.y))?;
    Ok((pois, ways))
}


fn read_tile(
    c: &mut Cursor,
    sub: &SubFile,
    origin: (i32, i32),
    pois: &mut Vec<Poi>,
    ways: &mut Vec<Way>,
) -> Result<()> {
    ensure!(sub.max_zoom >= sub.min_zoom, "invalid zoom range");
    let rows = usize::from(sub.max_zoom - sub.min_zoom) + 1;
    let mut table = Vec::with_capacity(rows);
    for _ in 0..rows {
        table.push((c.uvar()? as usize, c.uvar()? as usize));
    }
    let first_way = c.uvar()? as usize;
    let way_start = c.pos() + first_way;
    for (i, (poi_count, _)) in table.iter().enumerate() {
        for _ in 0..*poi_count {
            let mut poi = read_poi(c, origin)?;
            poi.zoom = sub.min_zoom + i as u8;
            pois.push(poi);
        }
    }
    c.seek(way_start);
    for (i, (_, way_count)) in table.iter().enumerate() {
        for _ in 0..*way_count {
            let mut way = read_way(c, origin)?;
            way.zoom = sub.min_zoom + i as u8;
            ways.push(way);
        }
    }
    Ok(())
}

fn read_tags(c: &mut Cursor, count: usize) -> Result<Vec<u32>> {
    (0..count).map(|_| Ok(c.uvar()? as u32)).collect()
}

fn read_poi(c: &mut Cursor, origin: (i32, i32)) -> Result<Poi> {
    let lat = origin.0.wrapping_add(c.svar()? as i32);
    let lon = origin.1.wrapping_add(c.svar()? as i32);
    let special = c.u8()?;
    let tags = read_tags(c, usize::from(special & 0x0f))?;
    let flags = c.u8()?;
    let name = if flags & 0x80 != 0 { Some(c.string()?) } else { None };
    let house_number = if flags & 0x40 != 0 { Some(c.string()?) } else { None };
    let elevation = if flags & 0x20 != 0 { Some(c.svar()? as i32) } else { None };
    Ok(Poi {
        point: Point { lat, lon },
        layer: (special >> 4) as i8 - 5,
        tags,
        name,
        house_number,
        elevation,
        zoom: 0,
    })
}

fn read_way(c: &mut Cursor, origin: (i32, i32)) -> Result<Way> {
    let size = c.uvar()? as usize;
    let end = c.pos() + size;
    c.take(2)?; // sub tile bitmask
    let special = c.u8()?;
    let tags = read_tags(c, usize::from(special & 0x0f))?;
    let flags = c.u8()?;
    let name = if flags & 0x80 != 0 { Some(c.string()?) } else { None };
    let house_number = if flags & 0x40 != 0 { Some(c.string()?) } else { None };
    let reference = if flags & 0x20 != 0 { Some(c.string()?) } else { None };
    if flags & 0x10 != 0 {
        c.svar()?;
        c.svar()?;
    }
    let blocks = if flags & 0x08 != 0 { c.uvar()? } else { 1 };
    let double_delta = flags & 0x04 != 0;
    let mut rings = Vec::new();
    for _ in 0..blocks {
        let ring_count = c.uvar()?;
        for _ in 0..ring_count {
            let nodes = c.uvar()? as usize;
            rings.push(read_ring(c, nodes, origin, double_delta)?);
        }
    }
    ensure!(c.pos() <= end, "way data overruns its declared size");
    c.seek(end);
    Ok(Way {
        rings,
        layer: (special >> 4) as i8 - 5,
        tags,
        name,
        house_number,
        reference,
        zoom: 0,
    })
}

fn read_ring(c: &mut Cursor, nodes: usize, origin: (i32, i32), double_delta: bool) -> Result<Vec<Point>> {
    if nodes == 0 {
        bail!("way with zero nodes");
    }
    let mut lat = origin.0.wrapping_add(c.svar()? as i32);
    let mut lon = origin.1.wrapping_add(c.svar()? as i32);
    let mut points = vec![Point { lat, lon }];
    let (mut prev_dlat, mut prev_dlon) = (0i32, 0i32);
    for _ in 1..nodes {
        let mut dlat = c.svar()? as i32;
        let mut dlon = c.svar()? as i32;
        if double_delta {
            dlat = dlat.wrapping_add(prev_dlat);
            dlon = dlon.wrapping_add(prev_dlon);
            prev_dlat = dlat;
            prev_dlon = dlon;
        }
        lat = lat.wrapping_add(dlat);
        lon = lon.wrapping_add(dlon);
        points.push(Point { lat, lon });
    }
    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_foreign_data() {
        assert!(read_header(b"not a map file at all....").is_err());
        assert!(read_header(&MAGIC[..10]).is_err());
    }

    #[test]
    fn optional_header_fields_are_skipped() {
        use crate::varint::put_string;
        let mut rest = Vec::new();
        rest.extend_from_slice(&3i32.to_be_bytes());
        rest.extend_from_slice(&0i64.to_be_bytes());
        rest.extend_from_slice(&0i64.to_be_bytes());
        rest.extend_from_slice(&[0u8; 16]);
        rest.extend_from_slice(&256u16.to_be_bytes());
        put_string(&mut rest, "Mercator");
        rest.push(0x40 | 0x20 | 0x10 | 0x08 | 0x04);
        rest.extend_from_slice(&[0u8; 8]); // start position
        rest.push(12); // start zoom
        put_string(&mut rest, "en");
        put_string(&mut rest, "comment");
        put_string(&mut rest, "writer");
        rest.extend_from_slice(&[0, 0, 0, 0, 0]); // no tags, no sub files
        let mut data = MAGIC.to_vec();
        data.extend_from_slice(&(rest.len() as i32).to_be_bytes());
        data.extend(rest);
        let header = read_header(&data).unwrap();
        assert_eq!(header.created_by.as_deref(), Some("writer"));
        assert!(header.subfiles.is_empty());
    }

    #[test]
    fn way_with_label_position_and_multiple_blocks() {
        use crate::varint::{put_string, put_svar, put_uvar};
        let mut data = vec![0xff, 0xff, 0x52 /* layer 0, 2 tags */, 1, 2, 0x80 | 0x10 | 0x08];
        put_string(&mut data, "name");
        put_svar(&mut data, 5);
        put_svar(&mut data, 6);
        put_uvar(&mut data, 2); // two blocks
        for _ in 0..2 {
            put_uvar(&mut data, 1); // one ring
            put_uvar(&mut data, 2); // two nodes
            for v in [1i64, 1, 1, 1] {
                put_svar(&mut data, v);
            }
        }
        let mut buf = Vec::new();
        put_uvar(&mut buf, data.len() as u64);
        buf.extend(data);
        let way = read_way(&mut Cursor::new(&buf, 0), (10, 20)).unwrap();
        assert_eq!(way.name.as_deref(), Some("name"));
        assert_eq!(way.tags, [1, 2]);
        assert_eq!(way.rings.len(), 2);
        assert_eq!(way.rings[0][1], Point { lat: 12, lon: 22 });
    }

    #[test]
    fn double_delta_rings_decode() {
        // origin (100, 200); first node (+1,+2); then double deltas (1,1) and (0,0) -> steps 1,1 / 1,1.
        let mut buf = Vec::new();
        for v in [1i64, 2, 1, 1, 0, 0] {
            crate::varint::put_svar(&mut buf, v);
        }
        let ring = read_ring(&mut Cursor::new(&buf, 0), 3, (100, 200), true).unwrap();
        let pts: Vec<_> = ring.iter().map(|p| (p.lat, p.lon)).collect();
        assert_eq!(pts, [(101, 202), (102, 203), (103, 204)]);
        assert!(read_ring(&mut Cursor::new(&buf, 0), 0, (0, 0), false).is_err());
    }
}
