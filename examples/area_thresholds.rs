//! Derives `data/area-min-size.csv`: for every way tag that the generated tile contains much more
//! often than the reference tile, the minimum size (largest bounding-box side, in microdegrees)
//! that keeps roughly as many ways as the reference has.
//!
//! Usage: `cargo run --release --example area_thresholds -- <reference.map> <generated.map> [ratio]`

use std::collections::BTreeMap;
use wahoo_tile_generator::reader;

fn sizes(path: &str) -> BTreeMap<String, Vec<i32>> {
    let map = reader::read_map(&std::fs::read(path).unwrap()).unwrap();
    let names = &map.header.way_tags;
    let mut out: BTreeMap<String, Vec<i32>> = BTreeMap::new();
    for way in &map.ways {
        let Some(b) = way.bounds() else { continue };
        let size = (b.max_lat - b.min_lat).max(b.max_lon - b.min_lon);
        for t in &way.tags {
            out.entry(names[*t as usize].clone()).or_default().push(size);
        }
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ratio: usize = args.get(2).map_or(3, |r| r.parse().unwrap());
    let reference = sizes(&args[0]);
    let generated = sizes(&args[1]);
    println!("osm_tag,min_size");
    for (tag, mut v) in generated {
        let keep = reference.get(&tag).map_or(0, Vec::len);
        if v.len() < 1000 || v.len() < keep * ratio || tag.starts_with("highway=") {
            continue;
        }
        v.sort_unstable_by(|a, b| b.cmp(a));
        let min_size = if keep == 0 { i32::MAX } else { v[keep - 1] };
        println!("{tag},{min_size}");
    }
}
