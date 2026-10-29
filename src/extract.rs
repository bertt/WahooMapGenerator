//! Turns OSM elements into map features for one tile.

use std::collections::HashMap;
use std::io::Read;

use anyhow::{Result, ensure};

use crate::model::{Point, Poi, Way};
use crate::osm::{Element, read_pbf};
use crate::tagmap::{Kind, TagMapping};
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

/// Reads a PBF stream (sorted nodes, then ways) and collects every feature of the tile.
///
/// All nodes with a known POI tag inside the tile become POIs, without thinning (see
/// WahooMapStyles issue #1). Relations are not processed.
pub fn extract<R: Read>(pbf: R, tile: &BBox, mapping: &TagMapping) -> Result<Extraction> {
    let area = tile.expanded(MARGIN_MICRO);
    let mut nodes: HashMap<i64, Point> = HashMap::new();
    let mut result = Extraction::default();
    let mut seen_way = false;
    read_pbf(pbf, &area, |element| {
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
                let Some(class) = mapping.classify(Kind::Way, &way.tags) else {
                    return Ok(());
                };
                let ring: Vec<Point> = way.refs.iter().filter_map(|r| nodes.get(r).copied()).collect();
                if ring.len() < 2 {
                    return Ok(());
                }
                let mut feature = Way {
                    layer: layer(&way.tags),
                    tags: class.tags,
                    name: tag(&way.tags, "name").map(str::to_string),
                    house_number: None,
                    reference: tag(&way.tags, "ref").map(str::to_string),
                    zoom: class.zoom,
                    rings: vec![ring],
                };
                let bounds = feature.bounds().expect("a ring has at least two points");
                let size = (bounds.max_lat - bounds.min_lat).max(bounds.max_lon - bounds.min_lon);
                let Some(class) = mapping.classify_way(&way.tags, size) else {
                    return Ok(());
                };
                (feature.tags, feature.zoom) = (class.tags, class.zoom);
                if bounds.intersects(tile) {
                    result.ways.push(feature);
                }
            }
        }
        Ok(())
    })?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osm::testutil::{TestNode, TestWay, build_pbf};

    const TILE: BBox = BBox { min_lat: 52_000_000, min_lon: 5_000_000, max_lat: 53_000_000, max_lon: 6_000_000 };

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
