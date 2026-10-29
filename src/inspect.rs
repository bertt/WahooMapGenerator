//! Per-tag statistics of map files and the comparison report between two maps.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::reader::MapContents;

/// Feature count and lowest zoom level for one tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TagStat {
    pub count: usize,
    pub min_zoom: u8,
}

/// Statistics of a map, keyed by tag text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub pois: BTreeMap<String, TagStat>,
    pub ways: BTreeMap<String, TagStat>,
}

fn add(map: &mut BTreeMap<String, TagStat>, tag: &str, zoom: u8) {
    let stat = map.entry(tag.to_string()).or_insert(TagStat { count: 0, min_zoom: zoom });
    stat.count += 1;
    stat.min_zoom = stat.min_zoom.min(zoom);
}

fn tag_text(tags: &[String], id: u32) -> String {
    tags.get(id as usize).cloned().unwrap_or_else(|| format!("<unknown tag {id}>"))
}

/// Counts features per tag.
pub fn summarize(map: &MapContents) -> Summary {
    let mut summary = Summary::default();
    for poi in &map.pois {
        for id in &poi.tags {
            add(&mut summary.pois, &tag_text(&map.header.poi_tags, *id), poi.zoom);
        }
    }
    for way in &map.ways {
        for id in &way.tags {
            add(&mut summary.ways, &tag_text(&map.header.way_tags, *id), way.zoom);
        }
    }
    summary
}

/// Lists the header tags in id order with count and lowest zoom (tab separated), one line per tag.
pub fn tag_table(map: &MapContents) -> String {
    let summary = summarize(map);
    let mut out = String::new();
    for (kind, tags, stats) in [
        ("poi", &map.header.poi_tags, &summary.pois),
        ("way", &map.header.way_tags, &summary.ways),
    ] {
        for tag in tags {
            match stats.get(tag) {
                Some(s) => writeln!(out, "{kind}\t{tag}\t{}\t{}", s.count, s.min_zoom),
                None => writeln!(out, "{kind}\t{tag}\t0\t-"),
            }
            .expect("writing to a String cannot fail");
        }
    }
    out
}

fn compare_section(out: &mut String, title: &str, reference: &BTreeMap<String, TagStat>, generated: &BTreeMap<String, TagStat>) {
    let mut tags: Vec<&String> = reference.keys().chain(generated.keys()).collect();
    tags.sort();
    tags.dedup();
    writeln!(out, "### {title}\n").unwrap();
    writeln!(out, "| Tag | Reference | Generated | Ref. min zoom | Gen. min zoom |").unwrap();
    writeln!(out, "|---|---:|---:|---:|---:|").unwrap();
    for tag in tags {
        let r = reference.get(tag);
        let g = generated.get(tag);
        let zoom = |s: Option<&TagStat>| s.map_or("-".to_string(), |s| s.min_zoom.to_string());
        writeln!(
            out,
            "| `{tag}` | {} | {} | {} | {} |",
            r.map_or(0, |s| s.count),
            g.map_or(0, |s| s.count),
            zoom(r),
            zoom(g)
        )
        .unwrap();
    }
    out.push('\n');
}

/// Renders a Markdown table with the size of a reference and a generated tile.
///
/// Each size is `(bytes on disk, bytes after decompression)`.
pub fn size_report(reference: (u64, u64), generated: (u64, u64)) -> String {
    let ratio = |g: u64, r: u64| if r == 0 { "n/a".to_string() } else { format!("{:.1}%", g as f64 * 100.0 / r as f64) };
    let mut out = String::new();
    writeln!(out, "| Size | Reference | Generated | Generated / reference |").unwrap();
    writeln!(out, "|---|---:|---:|---:|").unwrap();
    writeln!(out, "| On disk | {} | {} | {} |", reference.0, generated.0, ratio(generated.0, reference.0)).unwrap();
    writeln!(out, "| Uncompressed `.map` | {} | {} | {} |\n", reference.1, generated.1, ratio(generated.1, reference.1)).unwrap();
    out
}

/// Renders a Markdown report comparing a reference map with a generated map.
pub fn compare(reference: &MapContents, generated: &MapContents) -> String {
    let r = summarize(reference);
    let g = summarize(generated);
    let mut out = String::new();
    writeln!(out, "| | Reference | Generated |").unwrap();
    writeln!(out, "|---|---:|---:|").unwrap();
    writeln!(out, "| POIs | {} | {} |", reference.pois.len(), generated.pois.len()).unwrap();
    writeln!(out, "| Ways (per tile entry) | {} | {} |", reference.ways.len(), generated.ways.len()).unwrap();
    writeln!(
        out,
        "| POI tags in header | {} | {} |",
        reference.header.poi_tags.len(),
        generated.header.poi_tags.len()
    )
    .unwrap();
    writeln!(
        out,
        "| Way tags in header | {} | {} |\n",
        reference.header.way_tags.len(),
        generated.header.way_tags.len()
    )
    .unwrap();
    compare_section(&mut out, "POI tags", &r.pois, &g.pois);
    compare_section(&mut out, "Way tags", &r.ways, &g.ways);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Poi, Way};
    use crate::reader::Header;
    use crate::tile::BBox;

    fn map(poi_zoom: u8) -> MapContents {
        MapContents {
            header: Header {
                version: 3,
                file_size: 0,
                creation_ms: 0,
                bbox: BBox { min_lat: 0, min_lon: 0, max_lat: 0, max_lon: 0 },
                tile_size: 256,
                projection: "Mercator".into(),
                debug: false,
                created_by: None,
                poi_tags: vec!["a=b".into(), "c=d".into()],
                way_tags: vec!["x=y".into()],
                subfiles: vec![],
            },
            pois: vec![
                Poi { tags: vec![0, 7], zoom: poi_zoom + 2, ..Poi::default() },
                Poi { tags: vec![0], zoom: poi_zoom, ..Poi::default() },
            ],
            ways: vec![Way { tags: vec![0], zoom: 5, ..Way::default() }],
        }
    }

    #[test]
    fn counts_and_min_zoom() {
        let s = summarize(&map(3));
        assert_eq!(s.pois["a=b"], TagStat { count: 2, min_zoom: 3 });
        assert_eq!(s.pois["<unknown tag 7>"].count, 1);
        assert_eq!(s.ways["x=y"].min_zoom, 5);
    }

    #[test]
    fn tag_table_lists_unused_tags() {
        let table = tag_table(&map(3));
        assert!(table.contains("poi\ta=b\t2\t3\n"));
        assert!(table.contains("poi\tc=d\t0\t-\n"));
        assert!(table.contains("way\tx=y\t1\t5\n"));
    }

    #[test]
    fn size_table() {
        let report = size_report((1000, 4000), (500, 0));
        assert!(report.contains("| On disk | 1000 | 500 | 50.0% |"));
        assert!(report.contains("| Uncompressed `.map` | 4000 | 0 | 0.0% |"));
        assert!(size_report((0, 0), (1, 1)).contains("n/a"));
    }

    #[test]
    fn comparison_report() {
        let mut generated = map(4);
        generated.pois.pop();
        let report = compare(&map(3), &generated);
        assert!(report.contains("| POIs | 2 | 1 |"));
        assert!(report.contains("| `a=b` | 2 | 1 | 3 | 6 |"));
        assert!(report.contains("| `<unknown tag 7>` | 1 | 1 | 5 | 6 |"));
        assert!(!report.contains("`c=d`"));
        assert!(report.contains("### Way tags"));
    }
}
