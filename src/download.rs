//! Automatic download of OpenStreetMap extracts from Geofabrik.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::tile::BBox;

pub const GEOFABRIK_INDEX: &str = "https://download.geofabrik.de/index-v1.json";
const USER_AGENT: &str = concat!("wahoo-map-generator/", env!("CARGO_PKG_VERSION"));
const SAMPLES_PER_AXIS: usize = 5;

/// Opens a URL for streaming. Abstracted so tests do not need a network.
pub trait Fetcher {
    fn open(&self, url: &str) -> Result<Box<dyn Read>>;
}

/// Fetches over HTTP(S).
pub struct HttpFetcher;

impl Fetcher for HttpFetcher {
    fn open(&self, url: &str) -> Result<Box<dyn Read>> {
        let response = ureq::get(url)
            .header("User-Agent", USER_AGENT)
            .call()
            .with_context(|| format!("request to {url} failed"))?;
        Ok(Box::new(response.into_body().into_reader()))
    }
}

/// A Geofabrik region with its outline.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub id: String,
    pub pbf_url: String,
    /// Outer rings as `[lon, lat]` pairs.
    pub rings: Vec<Vec<[f64; 2]>>,
}

impl Region {
    fn contains(&self, lon: f64, lat: f64) -> bool {
        self.rings.iter().any(|ring| ring_contains(ring, lon, lat))
    }

    fn area(&self) -> f64 {
        self.rings
            .iter()
            .map(|ring| {
                let doubled: f64 = ring
                    .iter()
                    .zip(ring.iter().cycle().skip(1))
                    .map(|(a, b)| a[0] * b[1] - b[0] * a[1])
                    .sum();
                doubled.abs() / 2.0
            })
            .sum()
    }
}

fn ring_contains(ring: &[[f64; 2]], x: f64, y: f64) -> bool {
    let mut inside = false;
    let mut j = ring.len().saturating_sub(1);
    for i in 0..ring.len() {
        let (a, b) = (ring[i], ring[j]);
        if (a[1] > y) != (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0] {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn outer_rings(geometry: &Value) -> Vec<Vec<[f64; 2]>> {
    let ring = |v: &Value| -> Option<Vec<[f64; 2]>> {
        v.as_array()?.iter().map(|p| Some([p.get(0)?.as_f64()?, p.get(1)?.as_f64()?])).collect()
    };
    let coordinates = &geometry["coordinates"];
    match geometry["type"].as_str() {
        Some("Polygon") => coordinates.get(0).and_then(ring).into_iter().collect(),
        Some("MultiPolygon") => {
            coordinates.as_array().into_iter().flatten().filter_map(|polygon| polygon.get(0).and_then(ring)).collect()
        }
        _ => Vec::new(),
    }
}

/// Parses the Geofabrik `index-v1.json` (GeoJSON). Regions without a PBF download are dropped.
pub fn parse_index(json: &str) -> Result<Vec<Region>> {
    let value: Value = serde_json::from_str(json).context("invalid Geofabrik index")?;
    let features = value["features"].as_array().context("index has no features")?;
    let mut regions = Vec::new();
    for feature in features {
        let (Some(id), Some(url)) = (
            feature["properties"]["id"].as_str(),
            feature["properties"]["urls"]["pbf"].as_str(),
        ) else {
            continue;
        };
        let rings = outer_rings(&feature["geometry"]);
        if !rings.is_empty() {
            regions.push(Region { id: id.to_string(), pbf_url: url.to_string(), rings });
        }
    }
    Ok(regions)
}

/// Picks the smallest region that covers the whole tile (checked on a 5x5 grid of sample points).
pub fn select_region<'a>(regions: &'a [Region], tile: &BBox) -> Option<&'a Region> {
    let lon = |m: i32| f64::from(m) / 1e6;
    let (w, e) = (lon(tile.min_lon), lon(tile.max_lon));
    let (s, n) = (lon(tile.min_lat), lon(tile.max_lat));
    let steps = (SAMPLES_PER_AXIS - 1) as f64;
    let samples: Vec<(f64, f64)> = (0..SAMPLES_PER_AXIS)
        .flat_map(|i| (0..SAMPLES_PER_AXIS).map(move |j| (i as f64 / steps, j as f64 / steps)))
        .map(|(fx, fy)| (w + (e - w) * fx, s + (n - s) * fy))
        .collect();
    regions
        .iter()
        .filter(|r| samples.iter().all(|(x, y)| r.contains(*x, *y)))
        .min_by(|a, b| a.area().total_cmp(&b.area()))
}

/// Finds a region by its Geofabrik id (for example `europe/netherlands`).
pub fn find_region<'a>(regions: &'a [Region], id: &str) -> Option<&'a Region> {
    regions.iter().find(|r| r.id == id)
}

/// Returns the path of a cached PBF for the tile, downloading the smallest covering region first if needed.
pub fn ensure_pbf(
    fetcher: &dyn Fetcher,
    index_url: &str,
    tile: &BBox,
    region_id: Option<&str>,
    cache_dir: &Path,
) -> Result<PathBuf> {
    let mut json = String::new();
    fetcher.open(index_url)?.read_to_string(&mut json).context("reading the Geofabrik index failed")?;
    let regions = parse_index(&json)?;
    let region = match region_id {
        Some(id) => find_region(&regions, id).with_context(|| format!("unknown region '{id}'"))?,
        None => match select_region(&regions, tile) {
            Some(region) => region,
            None => bail!("no Geofabrik region covers the tile; pass --input or --region"),
        },
    };
    let target = cache_dir.join(format!("{}.osm.pbf", region.id.replace('/', "-")));
    if target.exists() {
        eprintln!("Using cached {}", target.display());
        return Ok(target);
    }
    fs::create_dir_all(cache_dir)?;
    eprintln!("Downloading {} (this can take a while)", region.pbf_url);
    let partial = target.with_extension("part");
    let mut source = fetcher.open(&region.pbf_url)?;
    let mut file = BufWriter::new(File::create(&partial)?);
    io::copy(&mut source, &mut file).context("download interrupted")?;
    drop(file);
    fs::rename(&partial, &target)?;
    Ok(target)
}

/// Bounding box of an XYZ tile in degrees, for messages.
pub fn describe(tile: &BBox) -> String {
    format!(
        "{:.3},{:.3} .. {:.3},{:.3}",
        f64::from(tile.min_lat) / 1e6,
        f64::from(tile.min_lon) / 1e6,
        f64::from(tile.max_lat) / 1e6,
        f64::from(tile.max_lon) / 1e6
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use std::net::TcpListener;

    pub const INDEX: &str = r#"{"features":[
        {"properties":{"id":"europe","urls":{"pbf":"http://x/europe.pbf"}},
         "geometry":{"type":"Polygon","coordinates":[[[-20,30],[40,30],[40,70],[-20,70],[-20,30]]]}},
        {"properties":{"id":"europe/netherlands","urls":{"pbf":"http://x/nl.pbf"}},
         "geometry":{"type":"MultiPolygon","coordinates":[[[[3,50],[8,50],[8,54],[3,54],[3,50]]],[[[100,0],[101,0],[101,1],[100,0]]]]}},
        {"properties":{"id":"europe/nopbf","urls":{}},"geometry":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}},
        {"properties":{"id":"nogeometry","urls":{"pbf":"http://x/n.pbf"}},"geometry":null},
        {"properties":{"id":"point","urls":{"pbf":"http://x/p.pbf"}},"geometry":{"type":"Point","coordinates":[1,2]}},
        {"properties":{"id":"broken","urls":{"pbf":"http://x/b.pbf"}},"geometry":{"type":"Polygon","coordinates":[[["a"]]]}}
    ]}"#;

    const TILE: BBox = BBox { min_lat: 51_618_017, min_lon: 4_218_750, max_lat: 52_482_780, max_lon: 5_625_000 };

    #[test]
    fn parses_index_and_skips_unusable_regions() {
        let regions = parse_index(INDEX).unwrap();
        let ids: Vec<_> = regions.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["europe", "europe/netherlands"]);
        assert_eq!(regions[1].rings.len(), 2);
        assert!(parse_index("{").is_err());
        assert!(parse_index("{}").is_err());
    }

    #[test]
    fn selects_the_smallest_covering_region() {
        let regions = parse_index(INDEX).unwrap();
        assert_eq!(select_region(&regions, &TILE).unwrap().id, "europe/netherlands");
        let far = BBox { min_lat: 60_000_000, min_lon: 10_000_000, max_lat: 61_000_000, max_lon: 11_000_000 };
        assert_eq!(select_region(&regions, &far).unwrap().id, "europe");
        let nowhere = BBox { min_lat: -10_000_000, min_lon: 0, max_lat: -9_000_000, max_lon: 1_000_000 };
        assert!(select_region(&regions, &nowhere).is_none());
        assert_eq!(find_region(&regions, "europe").unwrap().id, "europe");
        assert!(find_region(&regions, "mars").is_none());
    }

    struct FakeFetcher;

    impl Fetcher for FakeFetcher {
        fn open(&self, url: &str) -> Result<Box<dyn Read>> {
            match url {
                "index" => Ok(Box::new(Cursor::new(INDEX.as_bytes().to_vec()))),
                "http://x/nl.pbf" => Ok(Box::new(Cursor::new(b"pbf-bytes".to_vec()))),
                "http://x/europe.pbf" => Ok(Box::new(FailingReader)),
                other => bail!("unexpected url {other}"),
            }
        }
    }

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("boom"))
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wtg-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn downloads_once_and_reuses_the_cache() {
        let dir = temp_dir("download");
        let path = ensure_pbf(&FakeFetcher, "index", &TILE, None, &dir).unwrap();
        assert_eq!(path.file_name().unwrap(), "europe-netherlands.osm.pbf");
        assert_eq!(fs::read(&path).unwrap(), b"pbf-bytes");
        // Second call must not fetch the region again (the fake would still succeed, so alter the file).
        fs::write(&path, b"cached").unwrap();
        let again = ensure_pbf(&FakeFetcher, "index", &TILE, Some("europe/netherlands"), &dir).unwrap();
        assert_eq!(fs::read(again).unwrap(), b"cached");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn download_errors_are_reported() {
        let dir = temp_dir("errors");
        let far = BBox { min_lat: 60_000_000, min_lon: 10_000_000, max_lat: 61_000_000, max_lon: 11_000_000 };
        assert!(ensure_pbf(&FakeFetcher, "index", &far, None, &dir).is_err());
        assert!(ensure_pbf(&FakeFetcher, "index", &TILE, Some("mars"), &dir).is_err());
        let nowhere = BBox { min_lat: -10_000_000, min_lon: 0, max_lat: -9_000_000, max_lon: 1_000_000 };
        assert!(ensure_pbf(&FakeFetcher, "index", &nowhere, None, &dir).is_err());
        assert!(ensure_pbf(&FakeFetcher, "missing", &TILE, None, &dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn describes_a_box() {
        assert_eq!(describe(&TILE), "51.618,4.219 .. 52.483,5.625");
    }

    fn serve_once(status: &'static str, body: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
        });
        format!("http://{addr}/file")
    }

    #[test]
    fn http_fetcher_streams_bodies_and_reports_errors() {
        let mut text = String::new();
        HttpFetcher.open(&serve_once("200 OK", b"hello")).unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "hello");
        assert!(HttpFetcher.open(&serve_once("404 Not Found", b"no")).is_err());
    }
}
