//! A streaming reader for OpenStreetMap PBF files (nodes and ways only).

use std::io::Read;

use anyhow::{Context, Result, bail, ensure};
use flate2::read::ZlibDecoder;
use prost::Message;

use crate::tile::BBox;

const MAX_HEADER_SIZE: usize = 64 * 1024;
const MAX_BLOB_SIZE: usize = 64 * 1024 * 1024;

#[derive(Clone, PartialEq, Message)]
pub struct BlobHeader {
    #[prost(string, tag = "1")]
    pub r#type: String,
    #[prost(int32, tag = "3")]
    pub datasize: i32,
}

#[derive(Clone, PartialEq, Message)]
pub struct Blob {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub raw: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub zlib_data: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct StringTable {
    #[prost(bytes = "vec", repeated, tag = "1")]
    pub s: Vec<Vec<u8>>,
}

#[derive(Clone, PartialEq, Message)]
pub struct PbfNode {
    #[prost(sint64, tag = "1")]
    pub id: i64,
    #[prost(uint32, repeated, tag = "2")]
    pub keys: Vec<u32>,
    #[prost(uint32, repeated, tag = "3")]
    pub vals: Vec<u32>,
    #[prost(sint64, tag = "8")]
    pub lat: i64,
    #[prost(sint64, tag = "9")]
    pub lon: i64,
}

#[derive(Clone, PartialEq, Message)]
pub struct DenseNodes {
    #[prost(sint64, repeated, tag = "1")]
    pub id: Vec<i64>,
    #[prost(sint64, repeated, tag = "8")]
    pub lat: Vec<i64>,
    #[prost(sint64, repeated, tag = "9")]
    pub lon: Vec<i64>,
    #[prost(int32, repeated, tag = "10")]
    pub keys_vals: Vec<i32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct PbfWay {
    #[prost(int64, tag = "1")]
    pub id: i64,
    #[prost(uint32, repeated, tag = "2")]
    pub keys: Vec<u32>,
    #[prost(uint32, repeated, tag = "3")]
    pub vals: Vec<u32>,
    #[prost(sint64, repeated, tag = "8")]
    pub refs: Vec<i64>,
}

#[derive(Clone, PartialEq, Message)]
pub struct PbfRelation {
    #[prost(int64, tag = "1")]
    pub id: i64,
    #[prost(uint32, repeated, tag = "2")]
    pub keys: Vec<u32>,
    #[prost(uint32, repeated, tag = "3")]
    pub vals: Vec<u32>,
    #[prost(int32, repeated, tag = "8")]
    pub roles_sid: Vec<i32>,
    #[prost(sint64, repeated, tag = "9")]
    pub memids: Vec<i64>,
    /// 0 = node, 1 = way, 2 = relation.
    #[prost(int32, repeated, tag = "10")]
    pub types: Vec<i32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct PrimitiveGroup {
    #[prost(message, repeated, tag = "1")]
    pub nodes: Vec<PbfNode>,
    #[prost(message, optional, tag = "2")]
    pub dense: Option<DenseNodes>,
    #[prost(message, repeated, tag = "3")]
    pub ways: Vec<PbfWay>,
    #[prost(message, repeated, tag = "4")]
    pub relations: Vec<PbfRelation>,
}

#[derive(Clone, PartialEq, Message)]
pub struct PrimitiveBlock {
    #[prost(message, optional, tag = "1")]
    pub stringtable: Option<StringTable>,
    #[prost(message, repeated, tag = "2")]
    pub primitivegroup: Vec<PrimitiveGroup>,
    #[prost(int32, optional, tag = "17")]
    pub granularity: Option<i32>,
    #[prost(int64, optional, tag = "19")]
    pub lat_offset: Option<i64>,
    #[prost(int64, optional, tag = "20")]
    pub lon_offset: Option<i64>,
}

/// An OSM node with its position in microdegrees.
#[derive(Clone, Debug, PartialEq)]
pub struct OsmNode {
    pub id: i64,
    pub lat: i32,
    pub lon: i32,
    pub tags: Vec<(String, String)>,
}

/// An OSM way.
#[derive(Clone, Debug, PartialEq)]
pub struct OsmWay {
    pub id: i64,
    pub refs: Vec<i64>,
    pub tags: Vec<(String, String)>,
}

/// An OSM relation; only its way members are kept.
#[derive(Clone, Debug, PartialEq)]
pub struct OsmRelation {
    pub id: i64,
    /// `(way id, role)` of every way member.
    pub ways: Vec<(i64, String)>,
    pub tags: Vec<(String, String)>,
}

/// A node, a way or a relation.
#[derive(Clone, Debug, PartialEq)]
pub enum Element {
    Node(OsmNode),
    Way(OsmWay),
    Relation(OsmRelation),
}

/// Reads all blocks and calls `f` for every way and for every node inside `node_area`.
///
/// Nodes outside the area are skipped before their tags are decoded, which keeps reading a
/// country-sized file fast.
pub fn read_pbf<R: Read>(mut reader: R, node_area: &BBox, mut f: impl FnMut(Element) -> Result<()>) -> Result<()> {
    loop {
        let mut len_bytes = [0u8; 4];
        match reader.read(&mut len_bytes[..1])? {
            0 => return Ok(()),
            _ => reader.read_exact(&mut len_bytes[1..])?,
        }
        let header_len = u32::from_be_bytes(len_bytes) as usize;
        ensure!(header_len <= MAX_HEADER_SIZE, "blob header too large");
        let mut header_buf = vec![0u8; header_len];
        reader.read_exact(&mut header_buf)?;
        let header = BlobHeader::decode(header_buf.as_slice()).context("invalid blob header")?;
        let size = usize::try_from(header.datasize).context("negative blob size")?;
        ensure!(size <= MAX_BLOB_SIZE, "blob too large");
        let mut blob_buf = vec![0u8; size];
        reader.read_exact(&mut blob_buf)?;
        if header.r#type != "OSMData" {
            continue;
        }
        let data = blob_data(&Blob::decode(blob_buf.as_slice()).context("invalid blob")?)?;
        let block = PrimitiveBlock::decode(data.as_slice()).context("invalid primitive block")?;
        decode_block(&block, node_area, &mut f)?;
    }
}

fn blob_data(blob: &Blob) -> Result<Vec<u8>> {
    if let Some(raw) = &blob.raw {
        return Ok(raw.clone());
    }
    let Some(zlib) = &blob.zlib_data else {
        bail!("blob has no supported data");
    };
    let mut out = Vec::new();
    ZlibDecoder::new(zlib.as_slice()).read_to_end(&mut out).context("invalid zlib data")?;
    Ok(out)
}

struct Units {
    granularity: i64,
    lat_offset: i64,
    lon_offset: i64,
}

impl Units {
    fn micro(&self, raw: i64, offset: i64) -> i32 {
        ((offset + self.granularity * raw) as f64 / 1000.0).round() as i32
    }
}

fn string(table: &[Vec<u8>], index: usize) -> Result<String> {
    let bytes = table.get(index).with_context(|| format!("string index {index} out of range"))?;
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

fn pairs(table: &[Vec<u8>], keys: &[u32], vals: &[u32]) -> Result<Vec<(String, String)>> {
    ensure!(keys.len() == vals.len(), "key/value count mismatch");
    keys.iter().zip(vals).map(|(k, v)| Ok((string(table, *k as usize)?, string(table, *v as usize)?))).collect()
}

fn decode_block(block: &PrimitiveBlock, area: &BBox, f: &mut impl FnMut(Element) -> Result<()>) -> Result<()> {
    let units = Units {
        granularity: i64::from(block.granularity.unwrap_or(100)),
        lat_offset: block.lat_offset.unwrap_or(0),
        lon_offset: block.lon_offset.unwrap_or(0),
    };
    let table: &[Vec<u8>] = block.stringtable.as_ref().map_or(&[], |t| t.s.as_slice());
    for group in &block.primitivegroup {
        for node in &group.nodes {
            let lat = units.micro(node.lat, units.lat_offset);
            let lon = units.micro(node.lon, units.lon_offset);
            if area.contains(lat, lon) {
                f(Element::Node(OsmNode { id: node.id, lat, lon, tags: pairs(table, &node.keys, &node.vals)? }))?;
            }
        }
        if let Some(dense) = &group.dense {
            decode_dense(dense, &units, table, area, f)?;
        }
        for way in &group.ways {
            f(Element::Way(OsmWay { id: way.id, refs: delta_decode(&way.refs), tags: pairs(table, &way.keys, &way.vals)? }))?;
        }
                    for relation in &group.relations {
                        f(Element::Relation(decode_relation(relation, table)?))?;
                    }
                }
                Ok(())
            }

            fn decode_relation(relation: &PbfRelation, table: &[Vec<u8>]) -> Result<OsmRelation> {
                ensure!(
                    relation.roles_sid.len() == relation.memids.len() && relation.types.len() == relation.memids.len(),
                    "relation member arrays differ in length"
                );
                let mut ways = Vec::new();
                let mut id = 0i64;
                for i in 0..relation.memids.len() {
                    id += relation.memids[i];
                    if relation.types[i] == 1 {
                        let role = usize::try_from(relation.roles_sid[i]).context("negative role index")?;
                        ways.push((id, string(table, role)?));
                    }
                }
                Ok(OsmRelation { id: relation.id, ways, tags: pairs(table, &relation.keys, &relation.vals)? })
            }
fn delta_decode(values: &[i64]) -> Vec<i64> {
    values
        .iter()
        .scan(0i64, |acc, d| {
            *acc += d;
            Some(*acc)
        })
        .collect()
}

fn decode_dense(
    dense: &DenseNodes,
    units: &Units,
    table: &[Vec<u8>],
    area: &BBox,
    f: &mut impl FnMut(Element) -> Result<()>,
) -> Result<()> {
    ensure!(dense.id.len() == dense.lat.len() && dense.id.len() == dense.lon.len(), "dense node arrays differ in length");
    let (mut id, mut lat, mut lon) = (0i64, 0i64, 0i64);
    let mut kv = 0usize;
    for i in 0..dense.id.len() {
        id += dense.id[i];
        lat += dense.lat[i];
        lon += dense.lon[i];
        let mut tags = Vec::new();
        let inside = {
            let (la, lo) = (units.micro(lat, units.lat_offset), units.micro(lon, units.lon_offset));
            area.contains(la, lo).then_some((la, lo))
        };
        while kv < dense.keys_vals.len() && dense.keys_vals[kv] != 0 {
            let key = dense.keys_vals[kv] as usize;
            let val = *dense.keys_vals.get(kv + 1).context("dangling dense key")? as usize;
            if inside.is_some() {
                tags.push((string(table, key)?, string(table, val)?));
            }
            kv += 2;
        }
        // Skip the 0 delimiter; nodes without tags have none when keys_vals is empty.
        if kv < dense.keys_vals.len() {
            kv += 1;
        }
        if let Some((la, lo)) = inside {
            f(Element::Node(OsmNode { id, lat: la, lon: lo, tags }))?;
        }
    }
    Ok(())
}

#[cfg(test)]
pub mod testutil {
    //! Builds small PBF files for tests.

    use std::io::Write;

    use flate2::Compression;
    use flate2::write::ZlibEncoder;

    use super::*;

    pub struct TestNode {
        pub id: i64,
        pub lat: i32,
        pub lon: i32,
        pub tags: Vec<(&'static str, &'static str)>,
    }

    pub struct TestWay {
        pub id: i64,
        pub refs: Vec<i64>,
        pub tags: Vec<(&'static str, &'static str)>,
    }

    pub struct TestRelation {
        pub id: i64,
        /// `(way id, role)`.
        pub members: Vec<(i64, &'static str)>,
        pub tags: Vec<(&'static str, &'static str)>,
    }

    fn file_block(kind: &str, blob: &Blob) -> Vec<u8> {
        let body = blob.encode_to_vec();
        let header = BlobHeader { r#type: kind.to_string(), datasize: body.len() as i32 };
        let header_bytes = header.encode_to_vec();
        let mut out = (header_bytes.len() as u32).to_be_bytes().to_vec();
        out.extend(header_bytes);
        out.extend(body);
        out
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    struct Strings {
        table: Vec<Vec<u8>>,
    }

    impl Strings {
        fn new() -> Self {
            Strings { table: vec![Vec::new()] }
        }
        fn id(&mut self, s: &str) -> u32 {
            if let Some(i) = self.table.iter().position(|t| t == s.as_bytes()) {
                return i as u32;
            }
            self.table.push(s.as_bytes().to_vec());
            (self.table.len() - 1) as u32
        }
    }

    /// Builds a PBF with a header block, nodes (dense or plain, with differing units) and ways.
    pub fn build_pbf(nodes: &[TestNode], ways: &[TestWay], dense: bool) -> Vec<u8> {
        build_pbf_with_relations(nodes, ways, &[], dense)
    }

    /// Like [`build_pbf`], with relations (way members only) after the ways.
    pub fn build_pbf_with_relations(nodes: &[TestNode], ways: &[TestWay], relations: &[TestRelation], dense: bool) -> Vec<u8> {
        let mut out = file_block("OSMHeader", &Blob { raw: Some(vec![]), zlib_data: None });

        let mut st = Strings::new();
        let node_block = if dense {
            let (mut pid, mut plat, mut plon) = (0i64, 0i64, 0i64);
            let mut d = DenseNodes::default();
            for n in nodes {
                d.id.push(n.id - pid);
                d.lat.push(i64::from(n.lat) * 10 - plat);
                d.lon.push(i64::from(n.lon) * 10 - plon);
                (pid, plat, plon) = (n.id, i64::from(n.lat) * 10, i64::from(n.lon) * 10);
                for (k, v) in &n.tags {
                    d.keys_vals.push(st.id(k) as i32);
                    d.keys_vals.push(st.id(v) as i32);
                }
                d.keys_vals.push(0);
            }
            PrimitiveBlock {
                stringtable: Some(StringTable { s: st.table.clone() }),
                primitivegroup: vec![PrimitiveGroup { dense: Some(d), ..Default::default() }],
                ..Default::default()
            }
        } else {
            let mut group = PrimitiveGroup::default();
            for n in nodes {
                group.nodes.push(PbfNode {
                    id: n.id,
                    keys: n.tags.iter().map(|(k, _)| st.id(k)).collect(),
                    vals: n.tags.iter().map(|(_, v)| st.id(v)).collect(),
                    lat: i64::from(n.lat) - 2,
                    lon: i64::from(n.lon) - 2,
                });
            }
            PrimitiveBlock {
                stringtable: Some(StringTable { s: st.table.clone() }),
                primitivegroup: vec![group],
                granularity: Some(1000),
                lat_offset: Some(2000),
                lon_offset: Some(2000),
            }
        };
        out.extend(file_block("OSMData", &Blob { raw: None, zlib_data: Some(zlib(&node_block.encode_to_vec())) }));

        let mut st = Strings::new();
        let mut group = PrimitiveGroup::default();
        for w in ways {
            let mut prev = 0;
            group.ways.push(PbfWay {
                id: w.id,
                keys: w.tags.iter().map(|(k, _)| st.id(k)).collect(),
                vals: w.tags.iter().map(|(_, v)| st.id(v)).collect(),
                refs: w
                    .refs
                    .iter()
                    .map(|r| {
                        let d = r - prev;
                        prev = *r;
                        d
                    })
                    .collect(),
            });
        }
        let mut prev_member = 0;
        for r in relations {
            let mut rel = PbfRelation {
                id: r.id,
                keys: r.tags.iter().map(|(k, _)| st.id(k)).collect(),
                vals: r.tags.iter().map(|(_, v)| st.id(v)).collect(),
                ..Default::default()
            };
            for (way, role) in &r.members {
                rel.roles_sid.push(st.id(role) as i32);
                rel.memids.push(way - prev_member);
                rel.types.push(1);
                prev_member = *way;
            }
            group.relations.push(rel);
            prev_member = 0;
        }
        let way_block = PrimitiveBlock {
            stringtable: Some(StringTable { s: st.table }),
            primitivegroup: vec![group],
            ..Default::default()
        };
        out.extend(file_block("OSMData", &Blob { raw: Some(way_block.encode_to_vec()), zlib_data: None }));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    const WORLD: BBox = BBox { min_lat: -90_000_000, min_lon: -180_000_000, max_lat: 90_000_000, max_lon: 180_000_000 };

    fn collect(data: &[u8], area: &BBox) -> Result<Vec<Element>> {
        let mut out = Vec::new();
        read_pbf(data, area, |e| {
            out.push(e);
            Ok(())
        })?;
        Ok(out)
    }

    fn sample(dense: bool) -> Vec<u8> {
        build_pbf(
            &[
                TestNode { id: 10, lat: 52_000_000, lon: 5_000_000, tags: vec![("amenity", "bench"), ("name", "A")] },
                TestNode { id: 11, lat: 52_000_100, lon: 5_000_200, tags: vec![] },
                TestNode { id: 12, lat: 10_000_000, lon: 10_000_000, tags: vec![("a", "b")] },
                TestNode { id: 13, lat: 52_000_300, lon: 5_000_300, tags: vec![("c", "d")] },
            ],
            &[TestWay { id: 100, refs: vec![10, 11, 13, 10], tags: vec![("highway", "path")] }],
            dense,
        )
    }

    #[test]
    fn reads_dense_and_plain_nodes_and_ways() {
        for dense in [true, false] {
            let elements = collect(&sample(dense), &WORLD).unwrap();
            assert_eq!(elements.len(), 5);
            assert_eq!(
                elements[0],
                Element::Node(OsmNode {
                    id: 10,
                    lat: 52_000_000,
                    lon: 5_000_000,
                    tags: vec![("amenity".into(), "bench".into()), ("name".into(), "A".into())]
                })
            );
            let Element::Node(n) = &elements[1] else { panic!() };
            assert!(n.tags.is_empty());
            assert_eq!((n.id, n.lat, n.lon), (11, 52_000_100, 5_000_200));
            let Element::Way(w) = &elements[4] else { panic!() };
            assert_eq!(w.refs, [10, 11, 13, 10]);
            assert_eq!(w.tags, [("highway".to_string(), "path".to_string())]);
        }
    }

    #[test]
    fn nodes_outside_the_area_are_skipped() {
        let area = BBox { min_lat: 51_000_000, min_lon: 4_000_000, max_lat: 53_000_000, max_lon: 6_000_000 };
        for dense in [true, false] {
            let elements = collect(&sample(dense), &area).unwrap();
            let ids: Vec<i64> = elements
                .iter()
                .map(|e| match e {
                    Element::Node(n) => n.id,
                    Element::Way(w) => w.id,
                    Element::Relation(r) => r.id,
                })
                .collect();
            assert_eq!(ids, [10, 11, 13, 100]);
            let Element::Node(last) = &elements[2] else { panic!() };
            assert_eq!(last.tags, [("c".to_string(), "d".to_string())]);
        }
    }

    #[test]
    fn reads_relations_with_way_members_only() {
        let pbf = build_pbf_with_relations(
            &[],
            &[],
            &[TestRelation { id: 7, members: vec![(5, "outer"), (9, "inner")], tags: vec![("type", "multipolygon")] }],
            true,
        );
        let elements = collect(&pbf, &WORLD).unwrap();
        assert_eq!(
            elements,
            [Element::Relation(OsmRelation {
                id: 7,
                ways: vec![(5, "outer".into()), (9, "inner".into())],
                tags: vec![("type".into(), "multipolygon".into())],
            })]
        );

        let table = [vec![], b"k".to_vec()];
        let mixed = PbfRelation { memids: vec![1, 1], roles_sid: vec![0, 0], types: vec![0, 1], ..Default::default() };
        assert_eq!(decode_relation(&mixed, &table).unwrap().ways, [(2, String::new())]);
        let uneven = PbfRelation { memids: vec![1], ..Default::default() };
        assert!(decode_relation(&uneven, &table).is_err());
        let negative = PbfRelation { memids: vec![1], roles_sid: vec![-1], types: vec![1], ..Default::default() };
        assert!(decode_relation(&negative, &table).is_err());
    }

    #[test]
    fn callback_errors_stop_reading() {
        let err = read_pbf(sample(true).as_slice(), &WORLD, |_| bail!("stop")).unwrap_err();
        assert_eq!(err.to_string(), "stop");
    }

    #[test]
    fn empty_input_is_fine() {
        assert!(collect(&[], &WORLD).unwrap().is_empty());
    }

    #[test]
    fn malformed_files_are_rejected() {
        let good = sample(true);
        assert!(collect(&good[..3], &WORLD).is_err());
        assert!(collect(&good[..good.len() - 5], &WORLD).is_err());
        assert!(collect(&[0xff, 0xff, 0xff, 0xff], &WORLD).is_err());
        assert!(collect(&[0, 0, 0, 3, 0xff, 0xff, 0xff], &WORLD).is_err());

        let header = BlobHeader { r#type: "OSMData".into(), datasize: -1 }.encode_to_vec();
        let mut negative = (header.len() as u32).to_be_bytes().to_vec();
        negative.extend(header);
        assert!(collect(&negative, &WORLD).is_err());

        let header = BlobHeader { r#type: "OSMData".into(), datasize: i32::MAX }.encode_to_vec();
        let mut huge = (header.len() as u32).to_be_bytes().to_vec();
        huge.extend(header);
        assert!(collect(&huge, &WORLD).is_err());
    }

    fn data_file(blob: &Blob) -> Vec<u8> {
        let body = blob.encode_to_vec();
        let header = BlobHeader { r#type: "OSMData".into(), datasize: body.len() as i32 }.encode_to_vec();
        let mut out = (header.len() as u32).to_be_bytes().to_vec();
        out.extend(header);
        out.extend(body);
        out
    }

    #[test]
    fn invalid_blobs_are_rejected() {
        assert!(collect(&data_file(&Blob::default()), &WORLD).is_err());
        assert!(collect(&data_file(&Blob { raw: None, zlib_data: Some(b"nope".to_vec()) }), &WORLD).is_err());
        assert!(collect(&data_file(&Blob { raw: Some(vec![0xff, 0xff]), zlib_data: None }), &WORLD).is_err());
        // Block without a string table and without groups is valid and empty.
        assert!(collect(&data_file(&Blob { raw: Some(vec![]), zlib_data: None }), &WORLD).unwrap().is_empty());
    }

    fn block_file(block: PrimitiveBlock) -> Vec<u8> {
        data_file(&Blob { raw: Some(block.encode_to_vec()), zlib_data: None })
    }

    #[test]
    fn inconsistent_blocks_are_rejected() {
        let table = Some(StringTable { s: vec![vec![], b"k".to_vec()] });
        let group = |g: PrimitiveGroup| PrimitiveBlock { stringtable: table.clone(), primitivegroup: vec![g], ..Default::default() };

        let bad_index = PbfNode { keys: vec![9], vals: vec![1], ..Default::default() };
        assert!(collect(&block_file(group(PrimitiveGroup { nodes: vec![bad_index], ..Default::default() })), &WORLD).is_err());

        let mismatch = PbfWay { keys: vec![1], vals: vec![], ..Default::default() };
        assert!(collect(&block_file(group(PrimitiveGroup { ways: vec![mismatch], ..Default::default() })), &WORLD).is_err());

        let uneven = DenseNodes { id: vec![1], lat: vec![], lon: vec![], keys_vals: vec![] };
        assert!(collect(&block_file(group(PrimitiveGroup { dense: Some(uneven), ..Default::default() })), &WORLD).is_err());

        let dangling = DenseNodes { id: vec![1], lat: vec![0], lon: vec![0], keys_vals: vec![1] };
        assert!(collect(&block_file(group(PrimitiveGroup { dense: Some(dangling), ..Default::default() })), &WORLD).is_err());

        let untagged = DenseNodes { id: vec![1], lat: vec![0], lon: vec![0], keys_vals: vec![] };
        let elements = collect(&block_file(group(PrimitiveGroup { dense: Some(untagged), ..Default::default() })), &WORLD).unwrap();
        assert_eq!(elements.len(), 1);
    }
}
