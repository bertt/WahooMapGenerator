//! Turns OSM elements into map features for one tile.

use std::collections::HashMap;
use std::io::Read;

use anyhow::{Result, ensure};

use crate::model::{Point, Poi, Way};
use crate::osm::{Element, read_pbf};
use crate::tagmap::{Classified, Kind, TagMapping};
use crate::tile::BBox;

/// Nodes this far outside the tile (0.1 degrees) are kept so that ways crossing the tile border
/// keep their shape inside the tile.
pub const MARGIN_MICRO: i32 = 100_000;

/// The features found for a tile.
#[derive(Debug, Default)]
pub struct Extraction {
    pub pois: Vec<Poi>,
    pub ways: Vec<Way>,
}

fn tag<'a>(tags: &'a [(String, String)], key: &str) -> Option<&'a str> {
    tags.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn layer(tags: &[(String, String)]) -> i8 {
    tag(tags, "layer").and_then(|v| v.parse::<i32>().ok()).map_or(0, |v| v.clamp(-5, 5) as i8)
}

fn elevation(tags: &[(String, String)]) -> Option<i32> {
    let value: f64 = tag(tags, "ele")?.trim().trim_end_matches('m').trim().parse().ok()?;
    value.is_finite().then(|| value.round() as i32)
}

/// A multipolygon relation with a known tag; members are way ids.
struct Multipolygon {
    tags: Vec<(String, String)>,
    outer: Vec<i64>,
    inner: Vec<i64>,
}

/// A way that is a member of a multipolygon: end node ids plus the points inside the read area.
struct MemberWay {
    first: i64,
    last: i64,
    points: Vec<Point>,
}

/// First pass: finds the multipolygon relations that carry a mapped tag.
fn collect_multipolygons<R: Read>(pbf: R, mapping: &TagMapping) -> Result<Vec<Multipolygon>> {
    let nothing = BBox { min_lat: 1, min_lon: 1, max_lat: 0, max_lon: 0 };
    let mut found = Vec::new();
    read_pbf(pbf, &nothing, |element| {
        if let Element::Relation(rel) = element
            && tag(&rel.tags, "type") == Some("multipolygon")
            && mapping.classify(Kind::Way, &rel.tags).is_some()
        {
            let (inner, outer): (Vec<_>, Vec<_>) = rel.ways.iter().partition(|(_, role)| role == "inner");
            found.push(Multipolygon {
                tags: rel.tags,
                outer: outer.into_iter().map(|(id, _)| *id).collect(),
                inner: inner.into_iter().map(|(id, _)| *id).collect(),
            });
        }
        Ok(())
    })?;
    Ok(found)
}

/// Chains way members by shared end nodes into closed rings. Chains that do not close are dropped.
fn join_rings(ids: &[i64], members: &HashMap<i64, MemberWay>) -> Vec<Vec<Point>> {
    let mut seen = std::collections::HashSet::new();
    let mut pending: Vec<&MemberWay> = ids.iter().filter(|id| seen.insert(**id)).filter_map(|id| members.get(id)).collect();
    let mut rings = Vec::new();
    while let Some(start) = pending.pop() {
        let (first, mut last, mut points) = (start.first, start.last, start.points.clone());
        while first != last {
            let Some(i) = pending.iter().position(|w| w.first == last || w.last == last) else { break };
            let next = pending.swap_remove(i);
            if next.first == last {
                points.extend(next.points.iter().copied());
                last = next.last;
            } else {
                points.extend(next.points.iter().rev().copied());
                last = next.first;
            }
        }
        if first != last {
            continue;
        }
        points.dedup();
        if points.first() != points.last()
            && let Some(&p) = points.first()
        {
            points.push(p);
        }
        if points.len() >= 4 {
            rings.push(points);
        }
    }
    rings
}

/// Ray casting test; `ring` may or may not repeat its first point.
fn ring_contains(ring: &[Point], p: Point) -> bool {
    let mut inside = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[j]);
        if (a.lat > p.lat) != (b.lat > p.lat) {
            let t = f64::from(p.lat - a.lat) / f64::from(b.lat - a.lat);
            if f64::from(p.lon) < f64::from(a.lon) + t * f64::from(b.lon - a.lon) {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Builds the features of a multipolygon: one way per outer ring, holding the inner rings it contains.
fn multipolygon_rings(mp: &Multipolygon, members: &HashMap<i64, MemberWay>) -> Vec<Vec<Vec<Point>>> {
    let mut groups: Vec<Vec<Vec<Point>>> = join_rings(&mp.outer, members).into_iter().map(|r| vec![r]).collect();
    for inner in join_rings(&mp.inner, members) {
        let owner = if groups.len() == 1 {
            Some(0)
        } else {
            groups.iter().position(|g| ring_contains(&g[0], inner[0]))
        };
        if let Some(i) = owner {
            groups[i].push(inner);
        }
    }
    groups
}

/// Applies the area size filter and the tile test to a feature built from `rings`.
fn finish(
    tags: &[(String, String)],
    class: Classified,
    rings: Vec<Vec<Point>>,
    mapping: &TagMapping,
    tile: &BBox,
) -> Option<Way> {
    let mut feature = Way {
        layer: layer(tags),
        tags: class.tags,
        name: tag(tags, "name").map(str::to_string),
        house_number: None,
        reference: tag(tags, "ref").map(str::to_string),
        zoom: class.zoom,
        rings,
    };
    let bounds = feature.bounds()?;
    let size = (bounds.max_lat - bounds.min_lat).max(bounds.max_lon - bounds.min_lon);
    let class = mapping.classify_way(tags, size)?;
    (feature.tags, feature.zoom) = (class.tags, class.zoom);
    bounds.intersects(tile).then_some(feature)
}

/// Reads a sorted PBF (nodes, ways, relations) and collects every feature of the tile.
///
/// `open` must return a fresh reader of the same file each time: the file is read twice, first to
/// find the multipolygon relations (which hold e.g. the large lakes), then for the features.
///
/// All nodes with a known POI tag inside the tile become POIs, without thinning (see
/// WahooMapStyles issue #1). Multipolygon relations become ways with their outer ring first and
/// the inner rings (holes) after it; the tags are those of the relation.
pub fn extract<R: Read>(open: impl Fn() -> Result<R>, tile: &BBox, mapping: &TagMapping) -> Result<Extraction> {
    let multipolygons = collect_multipolygons(open()?, mapping)?;
    let member_ids: std::collections::HashSet<i64> =
        multipolygons.iter().flat_map(|m| m.outer.iter().chain(&m.inner)).copied().collect();
    let mut members: HashMap<i64, MemberWay> = HashMap::new();
    let area = tile.expanded(MARGIN_MICRO);
    let mut nodes: HashMap<i64, Point> = HashMap::new();
    let mut result = Extraction::default();
    let mut seen_way = false;
    read_pbf(open()?, &area, |element| {
        match element {
            Element::Node(node) => {
                ensure!(!seen_way, "the PBF file must be sorted (nodes before ways)");
                let point = Point { lat: node.lat, lon: node.lon };
                nodes.insert(node.id, point);
                if tile.contains(node.lat, node.lon)
                    && let Some(class) = mapping.classify(Kind::Poi, &node.tags)
                {
                    result.pois.push(Poi {
                        point,
                        layer: layer(&node.tags),
                        tags: class.tags,
                        name: tag(&node.tags, "name").map(str::to_string),
                        house_number: tag(&node.tags, "addr:housenumber").map(str::to_string),
                        elevation: elevation(&node.tags),
                        zoom: class.zoom,
                    });
                }
            }
            Element::Way(way) => {
                seen_way = true;
                if member_ids.contains(&way.id)
                    && let (Some(&first), Some(&last)) = (way.refs.first(), way.refs.last())
                {
                    let points = way.refs.iter().filter_map(|r| nodes.get(r).copied()).collect();
                    members.insert(way.id, MemberWay { first, last, points });
                }
                let Some(class) = mapping.classify(Kind::Way, &way.tags) else {
                    return Ok(());
                };
                let ring: Vec<Point> = way.refs.iter().filter_map(|r| nodes.get(r).copied()).collect();
                if ring.len() < 2 {
                    return Ok(());
                }
                result.ways.extend(finish(&way.tags, class, vec![ring], mapping, tile));
            }
            Element::Relation(_) => {}
        }
        Ok(())
    })?;
    for mp in &multipolygons {
        for rings in multipolygon_rings(mp, &members) {
            let Some(class) = mapping.classify(Kind::Way, &mp.tags) else { continue };
            result.ways.extend(finish(&mp.tags, class, rings, mapping, tile));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osm::testutil::{TestNode, TestRelation, TestWay, build_pbf, build_pbf_with_relations};

    const TILE: BBox = BBox { min_lat: 52_000_000, min_lon: 5_000_000, max_lat: 53_000_000, max_lon: 6_000_000 };

    fn extract(data: &[u8], tile: &BBox, mapping: &TagMapping) -> Result<Extraction> {
        super::extract(|| Ok(data), tile, mapping)
    }

    #[test]
    fn multipolygon_relations_become_ways_with_holes() {
        let mapping = TagMapping::parse("way,natural=water,natural=water,8,true\n").unwrap();
        let n = |id, lat, lon| node(id, lat, lon, vec![]);
        let pbf = build_pbf_with_relations(
            &[
                n(1, 52_100_000, 5_100_000),
                n(2, 52_100_000, 5_900_000),
                n(3, 52_900_000, 5_900_000),
                n(4, 52_900_000, 5_100_000),
                n(5, 52_400_000, 5_400_000),
                n(6, 52_400_000, 5_600_000),
                n(7, 52_600_000, 5_600_000),
                n(8, 52_600_000, 5_400_000),
            ],
            &[
                // The outer ring is split in two ways, one of them reversed.
                TestWay { id: 10, refs: vec![1, 2, 3], tags: vec![] },
                TestWay { id: 11, refs: vec![1, 4, 3], tags: vec![] },
                TestWay { id: 12, refs: vec![5, 6, 7, 8, 5], tags: vec![] },
                TestWay { id: 13, refs: vec![1, 2], tags: vec![] },
            ],
            &[
                TestRelation {
                    id: 100,
                    members: vec![(10, "outer"), (11, "outer"), (12, "inner")],
                    tags: vec![("type", "multipolygon"), ("natural", "water"), ("name", "Plas")],
                },
                // Unclosed outer ring, not a multipolygon and untagged relations are ignored.
                TestRelation { id: 101, members: vec![(13, "outer")], tags: vec![("type", "multipolygon"), ("natural", "water")] },
                TestRelation { id: 102, members: vec![(10, "outer"), (11, "outer")], tags: vec![("type", "route"), ("natural", "water")] },
                TestRelation { id: 103, members: vec![(10, "outer"), (11, "outer")], tags: vec![("type", "multipolygon")] },
            ],
            true,
        );
        let result = extract(&pbf, &TILE, &mapping).unwrap();
        assert_eq!(result.ways.len(), 1);
        let way = &result.ways[0];
        assert_eq!(way.name.as_deref(), Some("Plas"));
        assert_eq!(way.tags, vec![0]);
        assert_eq!(way.rings.len(), 2);
        assert_eq!(way.rings[0].len(), 5);
        assert_eq!(way.rings[0].first(), way.rings[0].last());
        assert_eq!(way.rings[1].len(), 5);
    }

    #[test]
    fn holes_go_to_the_outer_ring_that_contains_them() {
        let mapping = TagMapping::parse("way,natural=water,natural=water,8,true\n").unwrap();
        let square = |first: i64, lat: i32, lon: i32, size: i32| {
            let nodes = [(lat, lon), (lat, lon + size), (lat + size, lon + size), (lat + size, lon)];
            let n: Vec<TestNode> = nodes.iter().zip(first..).map(|(p, id)| node(id, p.0, p.1, vec![])).collect();
            (n, TestWay { id: first, refs: vec![first, first + 1, first + 2, first + 3, first], tags: vec![] })
        };
        let (n1, outer_a) = square(1, 52_100_000, 5_100_000, 300_000);
        let (n2, outer_b) = square(11, 52_100_000, 5_600_000, 300_000);
        let (n3, hole) = square(21, 52_200_000, 5_700_000, 100_000);
        let nodes: Vec<TestNode> = [n1, n2, n3].into_iter().flatten().collect();
        let pbf = build_pbf_with_relations(
            &nodes,
            &[outer_a, outer_b, hole],
            &[TestRelation {
                id: 100,
                members: vec![(1, "outer"), (11, "outer"), (21, "inner")],
                tags: vec![("type", "multipolygon"), ("natural", "water")],
            }],
            true,
        );
        let result = extract(&pbf, &TILE, &mapping).unwrap();
        let mut counts: Vec<usize> = result.ways.iter().map(|w| w.rings.len()).collect();
        counts.sort();
        assert_eq!(counts, [1, 2]);
    }

    fn mapping() -> TagMapping {
        TagMapping::parse(
            "poi,amenity=drinking_water,amenity=drinking_water,10,true\n\
             poi,tourism=picnic_site,tourism=picnic_site,10,true\n\
             way,highway=path,highway=path,10,true\n",
        )
        .unwrap()
    }

    fn node(id: i64, lat: i32, lon: i32, tags: Vec<(&'static str, &'static str)>) -> TestNode {
        TestNode { id, lat, lon, tags }
    }

    #[test]
    fn keeps_every_poi_with_a_known_tag() {
        // Regression for WahooMapStyles issue #1: no thinning of POIs.
        let nodes: Vec<TestNode> = (0..500)
            .map(|i| node(i + 1, 52_100_000 + i as i32 * 100, 5_100_000, vec![("amenity", "drinking_water")]))
            .collect();
        let pbf = build_pbf(&nodes, &[], true);
        let result = extract(pbf.as_slice(), &TILE, &mapping()).unwrap();
        assert_eq!(result.pois.len(), 500);
    }

    #[test]
    fn poi_attributes() {
        let pbf = build_pbf(
            &[
                node(1, 52_500_000, 5_500_000, vec![("tourism", "picnic_site"), ("name", "Rust"), ("addr:housenumber", "3"), ("layer", "9"), ("ele", "12.6 m")]),
                node(2, 52_500_001, 5_500_000, vec![("tourism", "picnic_site"), ("layer", "x"), ("ele", "high")]),
                node(3, 52_500_002, 5_500_000, vec![("tourism", "picnic_site"), ("ele", "NaN")]),
                node(4, 40_000_000, 5_500_000, vec![("tourism", "picnic_site")]),
                node(5, 52_500_003, 5_500_000, vec![("shop", "none")]),
            ],
            &[],
            false,
        );
        let result = extract(pbf.as_slice(), &TILE, &mapping()).unwrap();
        assert_eq!(result.pois.len(), 3);
        let p = &result.pois[0];
        assert_eq!(p.name.as_deref(), Some("Rust"));
        assert_eq!(p.house_number.as_deref(), Some("3"));
        assert_eq!(p.layer, 5);
        assert_eq!(p.elevation, Some(13));
        assert_eq!(p.zoom, 10);
        assert_eq!((result.pois[1].layer, result.pois[1].elevation), (0, None));
        assert_eq!(result.pois[2].elevation, None);
    }

    #[test]
    fn ways_are_built_from_known_nodes() {
        let pbf = build_pbf(
            &[
                node(1, 52_500_000, 5_500_000, vec![]),
                node(2, 52_500_100, 5_500_100, vec![]),
                node(3, 52_500_200, 5_500_200, vec![]),
                node(4, 30_000_000, 5_500_000, vec![]),
                node(5, 52_900_000, 5_900_000, vec![]),
            ],
            &[
                TestWay { id: 10, refs: vec![1, 2, 3, 99, 1], tags: vec![("highway", "path"), ("name", "Pad"), ("ref", "P1"), ("layer", "-1")] },
                TestWay { id: 11, refs: vec![1], tags: vec![("highway", "path")] },
                TestWay { id: 12, refs: vec![1, 2], tags: vec![("building", "yes")] },
                TestWay { id: 13, refs: vec![4, 4], tags: vec![("highway", "path")] },
                TestWay { id: 14, refs: vec![5, 1], tags: vec![("highway", "path")] },
            ],
            true,
        );
        let result = extract(pbf.as_slice(), &TILE, &mapping()).unwrap();
        assert_eq!(result.ways.len(), 2);
        let way = &result.ways[0];
        assert_eq!(way.rings[0].len(), 4);
        assert_eq!(way.name.as_deref(), Some("Pad"));
        assert_eq!(way.reference.as_deref(), Some("P1"));
        assert_eq!(way.layer, -1);
        assert_eq!(way.tags, vec![0]);
    }

    #[test]
    fn nodes_in_the_margin_are_kept_for_ways_but_not_as_pois() {
        let pbf = build_pbf(
            &[
                node(1, 51_950_000, 5_500_000, vec![("amenity", "drinking_water")]),
                node(2, 52_100_000, 5_500_000, vec![]),
            ],
            &[TestWay { id: 10, refs: vec![1, 2], tags: vec![("highway", "path")] }],
            true,
        );
        let result = extract(pbf.as_slice(), &TILE, &mapping()).unwrap();
        assert!(result.pois.is_empty());
        assert_eq!(result.ways.len(), 1);
        assert_eq!(result.ways[0].rings[0].len(), 2);
    }

    #[test]
    fn small_minor_areas_are_dropped() {
        let mapping = TagMapping::parse(
            "way,highway=path,highway=path,10,true\nway,landuse=grass,landuse=grass,10,true\n",
        )
        .unwrap()
        .with_min_sizes("landuse=grass,1000\n")
        .unwrap();
        let pbf = build_pbf(
            &[node(1, 52_500_000, 5_500_000, vec![]), node(2, 52_500_500, 5_500_000, vec![]), node(3, 52_502_000, 5_500_000, vec![])],
            &[
                TestWay { id: 10, refs: vec![1, 2], tags: vec![("landuse", "grass")] },
                TestWay { id: 11, refs: vec![1, 3], tags: vec![("landuse", "grass")] },
                TestWay { id: 12, refs: vec![1, 2], tags: vec![("landuse", "grass"), ("highway", "path")] },
            ],
            true,
        );
        let result = extract(pbf.as_slice(), &TILE, &mapping).unwrap();
        let tags: Vec<&[u32]> = result.ways.iter().map(|w| w.tags.as_slice()).collect();
        assert_eq!(tags, [&[1][..], &[0][..]]);
    }

    #[test]
    fn unsorted_files_are_rejected() {
        let mut data = build_pbf(&[], &[TestWay { id: 1, refs: vec![], tags: vec![] }], true);
        data.extend(build_pbf(&[node(1, 52_100_000, 5_100_000, vec![])], &[], true));
        let err = extract(data.as_slice(), &TILE, &mapping()).unwrap_err();
        assert!(err.to_string().contains("sorted"));
    }
}
