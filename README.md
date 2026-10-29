# WahooMapGenerator

A Rust tool that builds a Wahoo map tile (mapsforge v3 `.map`, LZMA-compressed) from
OpenStreetMap data. It produces files shaped like
`https://cdn.wahooligan.com/wahoo-maps/23/tiles/8/131/84.map.lzma`, where `8` is the zoom,
`131` the tile column (x) and `84` the tile row (y).

It fixes [WahooMapStyles#1](https://github.com/bertt/WahooMapStyles/issues/1): official Wahoo tiles
contain only a tiny fraction of the OSM POIs (e.g. 4 of 1548 `amenity=drinking_water` in tile 8/131/84).
This generator emits **every** OSM node carrying a known tag, with no thinning.

## Build

```
cargo build --release
```

## Usage

```
# Generate tile 8/131/84 (downloads the smallest covering Geofabrik extract automatically)
wahoo-map-generator generate --col 131 --row 84
# -> tiles/8/131/84.map.lzma

wahoo-map-generator generate --col 131 --row 84 --input netherlands.osm.pbf --no-compress -o 84.map
wahoo-map-generator inspect 84.map.lzma          # TSV: kind, tag, count, min zoom
wahoo-map-generator compare reference.map.lzma generated.map.lzma   # Markdown report
```

`generate` options: `--col/--x`, `--row/--y`, `--zoom` (default 8), `--input <pbf>`, `--region <geofabrik id>`,
`--cache-dir` (default `cache`), `-o`, `--no-compress`. The input PBF must be sorted (Geofabrik extracts are).

## Tag mapping

`data/tag-mapping.csv` (`kind,osm_tag,wahoo_tag,min_zoom,primary`) maps OSM tags to the Wahoo tag
vocabulary (e.g. `bicycle=yes` → `bicycle=bic_yes`). It was derived from the reference tile; regenerate with:

```
uv run python tools/gen_tag_mapping.py data/reference-tags.tsv data/tag-mapping.csv
```

Tag ids follow first-seen order, matching the reference header (118 POI tags, 195 way tags).

## Architecture

`osm` (PBF reader) → `extract` (POIs/ways via `tagmap`) → `writer` (mapsforge) → `lzma`.
`reader`, `inspect` and `cli compare` read tiles back for validation. `download` handles Geofabrik.
`tile` holds the Web-Mercator maths and the mapsforge tile grid.

## Tests and coverage

```
cargo test
cargo llvm-cov --ignore-filename-regex 'main\.rs'
```

Coverage is 100% of lines and functions (`main.rs` is a thin wrapper and excluded).

## Performance

Measured for tile 8/131/84 with the Netherlands extract (1.4 GB PBF) already cached, so without
download time, on a 13th Gen Intel Core i7-1360P with 16 GB RAM (release build, single thread):

| Step | Time |
|---|---:|
| Read PBF, extract, clip and write `.map` | about 84 s |
| LZMA compression (`.map.lzma`) | about 50 s |
| **Total** | **about 134 s** |

Peak memory is about 3.3 GB (all nodes near the tile are kept in memory to build ways). The download of
the extract is a one-off cost per region and is cached in `--cache-dir`; it took several minutes here.

## Differences vs. the reference tile 8/131/84

Output is semantically equivalent (same header vocabulary, same tags), not byte-identical.
Measured with `compare` (Netherlands extract from Geofabrik, generated in about 6 minutes):

| Size | Reference | Generated | Generated / reference |
|---|---:|---:|---:|
| On disk (`.map.lzma`) | 33,051,189 | 47,913,121 | 145.0% |
| Uncompressed `.map` | 52,311,606 | 73,514,267 | 140.5% |

| | Reference | Generated |
|---|---:|---:|
| POIs | 116,071 | 749,996 |
| Ways (per tile entry) | 1,483,333 | 1,441,482 |
| POI tags in header | 118 | 118 |
| Way tags in header | 195 | 195 |

Selected POI counts (the issue #1 effect):

| Tag | Reference | Generated |
|---|---:|---:|
| `amenity=drinking_water` | 4 | 1548 |
| `amenity=water_point` | 1 | 51 |
| `amenity=bench` | 101 | 45,938 |
| `amenity=recycling` | 1 | 21,747 |

The remaining size difference comes almost entirely from keeping all POIs (6.5× more POIs).
The full per-tag table comes from `compare`.

Size history of the generated `.map.lzma`: 76.0 MB (no clipping) → 75.4 MB (ways clipped) →
47.9 MB (minor areas filtered).

Way clipping: the reference clips ways to their base-zoom tile plus a margin of about 20 m (measured: nearly
all way bounding boxes overshoot the tile by 0.0001–0.0003°). The generator does the same: closed rings are
clipped as polygons (Sutherland–Hodgman), open rings as polylines (Liang–Barsky, possibly split in pieces).

Minor areas: the reference holds only a small fraction of areas that do not matter for cycling
(`landuse=grass` 3,500 vs 286,042 in plain OSM, `amenity=parking` 29 vs 106,458, `barrier=fence`,
`landuse=meadow`, `leisure=garden`, ...). `data/area-min-size.csv` lists a minimum size per OSM tag; smaller
ways lose that tag and are dropped if no other primary tag remains. The thresholds were calibrated so that the
counts approximate the reference and can be regenerated with:

```
cargo run --release --example area_thresholds -- reference.map generated.map
```

Large forests, water, parks and all highways are not filtered.

Known limitations:

- No relations/multipolygons; no sea/coastline (`natural=sea|nosea`) or water flag.
- Only nodes within 0.1° of the tile are read, so ways that extend further are cut at that margin.
- No area-centre label POIs (the reference has e.g. `landuse=forest` POIs).
- No `hike_node`, `cycle_node` or `cycleway_*_oneway` tags (no OSM source).
- Way coordinates use single-delta instead of double-delta encoding.
- Per-tag min zooms are copied from the reference.
- Compatibility with the Wahoo head units is untested.
