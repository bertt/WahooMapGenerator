//! Command line interface.

use std::fs::{self, File};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

use crate::download::{Fetcher, GEOFABRIK_INDEX, describe, ensure_pbf};
use crate::extract::extract;
use crate::inspect;
use crate::lzma;
use crate::reader::{MapContents, read_map};
use crate::tagmap::{Kind, TagMapping};
use crate::tile::Tile;
use crate::writer::{MapSpec, build_map};

/// WahooTileGenerator: builds Wahoo-compatible mapsforge map tiles from OpenStreetMap data.
#[derive(Parser, Debug)]
#[command(name = "wahoo-tile-generator", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Generate the tile `<zoom>/<col>/<row>` (default zoom 8) from OpenStreetMap data.
    Generate(GenerateArgs),
    /// Print the tag table (kind, tag, feature count, lowest zoom) of a `.map` or `.map.lzma` file.
    Inspect {
        file: PathBuf,
    },
    /// Print a Markdown report comparing a reference map with a generated map.
    Compare {
        reference: PathBuf,
        generated: PathBuf,
    },
}

#[derive(Args, Debug)]
pub struct GenerateArgs {
    /// Tile column (XYZ x).
    #[arg(long, alias = "x")]
    pub col: u32,
    /// Tile row (XYZ y).
    #[arg(long, alias = "y")]
    pub row: u32,
    /// Zoom level of the tile.
    #[arg(long, default_value_t = 8)]
    pub zoom: u8,
    /// Local OSM PBF file (sorted). When omitted, a Geofabrik extract is downloaded.
    #[arg(long)]
    pub input: Option<PathBuf>,
    /// Geofabrik region id such as `europe/netherlands` (default: smallest region covering the tile).
    #[arg(long)]
    pub region: Option<String>,
    /// Directory for downloaded extracts.
    #[arg(long, default_value = "cache")]
    pub cache_dir: PathBuf,
    /// Output file (default: `tiles/<zoom>/<col>/<row>.map.lzma`).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Write the plain `.map` instead of the LZMA-compressed `.map.lzma`.
    #[arg(long)]
    pub no_compress: bool,
}

/// Runs a parsed command; human-readable results are written to `out`.
pub fn run(cli: Cli, fetcher: &dyn Fetcher, out: &mut dyn Write) -> Result<()> {
    match cli.command {
        Command::Generate(args) => generate(&args, fetcher, out),
        Command::Inspect { file } => {
            write!(out, "{}", inspect::tag_table(&load(&file)?))?;
            Ok(())
        }
        Command::Compare { reference, generated } => {
            let (r, g) = (load_sized(&reference)?, load_sized(&generated)?);
            let sizes = inspect::size_report((r.1, r.0.header.file_size as u64), (g.1, g.0.header.file_size as u64));
            write!(out, "{sizes}{}", inspect::compare(&r.0, &g.0))?;
            Ok(())
        }
    }
}

/// Reads a `.map` file, decompressing it first when it has the `.lzma` extension.
pub fn load(path: &Path) -> Result<MapContents> {
    Ok(load_sized(path)?.0)
}

/// Like [`load`], also returning the size of the file on disk.
pub fn load_sized(path: &Path) -> Result<(MapContents, u64)> {
    let mut data = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let disk_size = data.len() as u64;
    if path.extension().is_some_and(|e| e == "lzma") {
        data = lzma::decompress(&data)?;
    }
    Ok((read_map(&data)?, disk_size))
}

fn generate(args: &GenerateArgs, fetcher: &dyn Fetcher, out: &mut dyn Write) -> Result<()> {
    let tile = Tile { zoom: args.zoom, x: args.col, y: args.row };
    if !tile.is_valid() {
        bail!("tile {}/{}/{} does not exist", args.zoom, args.col, args.row);
    }
    let bbox = tile.bbox();
    writeln!(out, "Tile {}/{}/{}: {}", tile.zoom, tile.x, tile.y, describe(&bbox))?;

    let input = match &args.input {
        Some(path) => path.clone(),
        None => ensure_pbf(fetcher, GEOFABRIK_INDEX, &bbox, args.region.as_deref(), &args.cache_dir)?,
    };
    let mapping = TagMapping::embedded();
    let file = File::open(&input).with_context(|| format!("cannot open {}", input.display()))?;
    let features = extract(BufReader::new(file), &bbox, &mapping)?;
    writeln!(out, "Found {} POIs and {} ways", features.pois.len(), features.ways.len())?;

    let created_by = format!("wahoo-tile-generator {}", env!("CARGO_PKG_VERSION"));
    let spec = MapSpec {
        tile,
        poi_tags: mapping.tags(Kind::Poi),
        way_tags: mapping.tags(Kind::Way),
        creation_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64,
        created_by: &created_by,
    };
    let mut bytes = build_map(&spec, &features.pois, &features.ways);
    let default_name = if args.no_compress { "map" } else { "map.lzma" };
    if !args.no_compress {
        bytes = lzma::compress(&bytes)?;
    }
    let output = args.output.clone().unwrap_or_else(|| {
        PathBuf::from(format!("tiles/{}/{}/{}.{default_name}", tile.zoom, tile.x, tile.y))
    });
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output, &bytes).with_context(|| format!("cannot write {}", output.display()))?;
    writeln!(out, "Wrote {} ({} bytes)", output.display(), bytes.len())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osm::testutil::{TestNode, TestWay, build_pbf};
    use std::io::Read;

    struct FakePbf(Vec<u8>);

    impl Fetcher for FakePbf {
        fn open(&self, url: &str) -> Result<Box<dyn Read>> {
            if url == GEOFABRIK_INDEX {
                let index = r#"{"features":[{"properties":{"id":"world","urls":{"pbf":"http://x/world.pbf"}},
                    "geometry":{"type":"Polygon","coordinates":[[[-180,-80],[180,-80],[180,80],[-180,80],[-180,-80]]]}}]}"#;
                return Ok(Box::new(std::io::Cursor::new(index.as_bytes().to_vec())));
            }
            Ok(Box::new(std::io::Cursor::new(self.0.clone())))
        }
    }

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wtg-cli-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn pbf() -> Vec<u8> {
        build_pbf(
            &[
                TestNode { id: 1, lat: 52_100_000, lon: 5_000_000, tags: vec![("amenity", "drinking_water")] },
                TestNode { id: 2, lat: 52_100_500, lon: 5_000_500, tags: vec![] },
            ],
            &[TestWay { id: 9, refs: vec![1, 2], tags: vec![("highway", "path")] }],
            true,
        )
    }

    fn run_args(args: &[&str], fetcher: &dyn Fetcher) -> Result<String> {
        let cli = Cli::try_parse_from(std::iter::once("wahoo-tile-generator").chain(args.iter().copied())).unwrap();
        let mut out = Vec::new();
        run(cli, fetcher, &mut out)?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn generate_inspect_and_compare() {
        let d = dir("full");
        let input = d.join("in.pbf");
        fs::write(&input, pbf()).unwrap();
        let packed = d.join("out.map.lzma");
        let plain = d.join("sub").join("out.map");
        let fetcher = FakePbf(Vec::new());

        let text = run_args(
            &["generate", "--x", "131", "--y", "84", "--input", input.to_str().unwrap(), "-o", packed.to_str().unwrap()],
            &fetcher,
        )
        .unwrap();
        assert!(text.contains("Found 1 POIs and 1 ways"));
        run_args(
            &["generate", "--col", "131", "--row", "84", "--input", input.to_str().unwrap(), "-o", plain.to_str().unwrap(), "--no-compress"],
            &fetcher,
        )
        .unwrap();

        let table = run_args(&["inspect", packed.to_str().unwrap()], &fetcher).unwrap();
        assert!(table.contains("poi\tamenity=drinking_water\t1\t10"));
        assert!(table.contains("way\thighway=path\t"));
        let report = run_args(&["compare", packed.to_str().unwrap(), plain.to_str().unwrap()], &fetcher).unwrap();
        assert!(report.contains("| POIs | 1 | 1 |"));
        assert!(report.contains("| On disk |"));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn generate_downloads_when_no_input_is_given() {
        let d = dir("download");
        let out = d.join("t.map");
        let text = run_args(
            &["generate", "--col", "131", "--row", "84", "--cache-dir", d.join("cache").to_str().unwrap(), "-o", out.to_str().unwrap(), "--no-compress"],
            &FakePbf(pbf()),
        )
        .unwrap();
        assert!(text.contains("Found 1 POIs"));
        assert!(d.join("cache").join("world.osm.pbf").exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn default_output_path_is_relative_to_the_working_directory() {
        let d = dir("default");
        let input = d.join("in.pbf");
        fs::write(&input, pbf()).unwrap();
        let before = std::env::current_dir().unwrap();
        std::env::set_current_dir(&d).unwrap();
        let result = run_args(&["generate", "--col", "131", "--row", "84", "--input", "in.pbf"], &FakePbf(Vec::new()));
        let bare = run_args(&["generate", "--col", "131", "--row", "84", "--input", "in.pbf", "-o", "bare.map", "--no-compress"], &FakePbf(Vec::new()));
        std::env::set_current_dir(before).unwrap();
        assert!(result.unwrap().contains("tiles"));
        bare.unwrap();
        assert!(d.join("tiles").join("8").join("131").join("84.map.lzma").exists());
        assert!(d.join("bare.map").exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn errors_are_reported() {
        let d = dir("errors");
        let fetcher = FakePbf(Vec::new());
        assert!(run_args(&["generate", "--col", "999", "--row", "0"], &fetcher).is_err());
        assert!(run_args(&["generate", "--col", "1", "--row", "1", "--input", d.join("missing.pbf").to_str().unwrap()], &fetcher).is_err());
        assert!(run_args(&["inspect", d.join("missing.map").to_str().unwrap()], &fetcher).is_err());
        fs::write(d.join("bad.map"), b"junk").unwrap();
        assert!(run_args(&["inspect", d.join("bad.map").to_str().unwrap()], &fetcher).is_err());
        let blocked = d.join("blocked");
        fs::create_dir_all(&blocked).unwrap();
        fs::write(d.join("in.pbf"), pbf()).unwrap();
        let input = d.join("in.pbf");
        assert!(run_args(&["generate", "--col", "131", "--row", "84", "--input", input.to_str().unwrap(), "-o", blocked.to_str().unwrap(), "--no-compress"], &fetcher).is_err());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn rejects_unsupported_map_versions() {
        let d = dir("version");
        let packed = d.join("x.map");
        let mut bytes = crate::writer::build_map(
            &MapSpec { tile: Tile { zoom: 8, x: 131, y: 84 }, poi_tags: &[], way_tags: &[], creation_ms: 0, created_by: "t" },
            &[],
            &[],
        );
        bytes[27] = 4;
        fs::write(&packed, bytes).unwrap();
        assert!(load(&packed).is_err());
        fs::remove_dir_all(&d).unwrap();
    }
}
