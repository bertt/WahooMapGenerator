//! WebAssembly bindings used by the web site: generate, compress, decompress and serve map tiles.

use std::io::Read;

use anyhow::{Result, anyhow};
use js_sys::{Function, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;

use crate::extract::extract;
use crate::lzma;
use crate::mvt::MapFile;
use crate::tagmap::{Kind, TagMapping};
use crate::tile::Tile;
use crate::writer::{MapSpec, build_map};

const CHUNK: usize = 4 << 20;

fn js_err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&format!("{e:#}"))
}

fn call(target: &JsValue, name: &str, args: &[JsValue]) -> Result<JsValue> {
    let f: Function = Reflect::get(target, &JsValue::from_str(name))
        .map_err(|_| anyhow!("source has no `{name}`"))?
        .dyn_into()
        .map_err(|_| anyhow!("`{name}` is not a function"))?;
    let array = js_sys::Array::new();
    args.iter().for_each(|a| {
        array.push(a);
    });
    f.apply(target, &array).map_err(|e| anyhow!("{}", e.as_string().unwrap_or_else(|| format!("{e:?}"))))
}

/// Reads a JS object `{ size, read(offset, length) -> Uint8Array }` sequentially, reporting progress.
struct JsReader<'a> {
    source: &'a JsValue,
    size: u64,
    pos: u64,
    buf: Vec<u8>,
    buf_pos: usize,
    pass: u32,
    progress: &'a Function,
}

impl Read for JsReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.buf_pos == self.buf.len() {
            if self.pos >= self.size {
                return Ok(0);
            }
            let len = (self.size - self.pos).min(CHUNK as u64);
            let chunk = call(self.source, "read", &[JsValue::from_f64(self.pos as f64), JsValue::from_f64(len as f64)])
                .map_err(std::io::Error::other)?;
            self.buf = Uint8Array::new(&chunk).to_vec();
            self.buf_pos = 0;
            if self.buf.is_empty() {
                return Ok(0);
            }
            self.pos += self.buf.len() as u64;
            let _ = self.progress.call3(
                &JsValue::NULL,
                &JsValue::from(self.pass),
                &JsValue::from_f64(self.pos as f64),
                &JsValue::from_f64(self.size as f64),
            );
        }
        let n = out.len().min(self.buf.len() - self.buf_pos);
        out[..n].copy_from_slice(&self.buf[self.buf_pos..self.buf_pos + n]);
        self.buf_pos += n;
        Ok(n)
    }
}

/// The outcome of [`generate`].
#[wasm_bindgen]
pub struct Generated {
    map: Vec<u8>,
    pois: u32,
    ways: u32,
}

#[wasm_bindgen]
impl Generated {
    /// The `.map` file bytes (taken once; afterwards empty).
    pub fn take_map(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.map)
    }
    #[wasm_bindgen(getter)]
    pub fn pois(&self) -> u32 {
        self.pois
    }
    #[wasm_bindgen(getter)]
    pub fn ways(&self) -> u32 {
        self.ways
    }
}

/// Builds the map of tile `zoom/col/row` from an OSM PBF. `source` is `{ size, read(offset, length) }`;
/// `progress(pass, bytesRead, totalBytes)` is called while reading (the file is read twice).
#[wasm_bindgen]
pub fn generate(source: JsValue, zoom: u8, col: u32, row: u32, progress: Function) -> Result<Generated, JsError> {
    let tile = Tile { zoom, x: col, y: row };
    if !tile.is_valid() {
        return Err(js_err(format!("tile {zoom}/{col}/{row} does not exist")));
    }
    let size = Reflect::get(&source, &JsValue::from_str("size")).ok().and_then(|v| v.as_f64()).ok_or_else(|| js_err("source has no size"))? as u64;
    let mapping = TagMapping::embedded();
    let pass = std::cell::Cell::new(0u32);
    let features = extract(
        || {
            pass.set(pass.get() + 1);
            Ok(std::io::BufReader::new(JsReader {
                source: &source,
                size,
                pos: 0,
                buf: Vec::new(),
                buf_pos: 0,
                pass: pass.get(),
                progress: &progress,
            }))
        },
        &tile.bbox(),
        &mapping,
    )
    .map_err(js_err)?;
    let created_by = format!("wahoo-map-generator {} (web)", env!("CARGO_PKG_VERSION"));
    let spec = MapSpec {
        tile,
        poi_tags: mapping.tags(Kind::Poi),
        way_tags: mapping.tags(Kind::Way),
        creation_ms: js_sys::Date::now() as i64,
        created_by: &created_by,
    };
    let map = build_map(&spec, &features.pois, &features.ways);
    Ok(Generated { map, pois: features.pois.len() as u32, ways: features.ways.len() as u32 })
}

/// LZMA-compresses data; `progress(bytesDone, bytesTotal)` is called per chunk.
#[wasm_bindgen]
pub fn compress_lzma(data: &[u8], progress: Function) -> Result<Vec<u8>, JsError> {
    let total = data.len();
    let mut done = 0usize;
    lzma::compress_with_progress(data, |n| {
        done += n;
        let _ = progress.call2(&JsValue::NULL, &JsValue::from_f64(done as f64), &JsValue::from_f64(total as f64));
    })
    .map_err(js_err)
}

/// Decompresses LZMA data (`.map.lzma`).
#[wasm_bindgen]
pub fn decompress_lzma(data: &[u8]) -> Result<Vec<u8>, JsError> {
    lzma::decompress(data).map_err(js_err)
}

/// True when the bytes start with a mapsforge magic string.
#[wasm_bindgen]
pub fn is_mapsforge(data: &[u8]) -> bool {
    data.starts_with(b"mapsforge binary OSM")
}

/// A loaded map that serves vector tiles.
#[wasm_bindgen]
pub struct MapViewer {
    map: MapFile,
}

#[wasm_bindgen]
impl MapViewer {
    #[wasm_bindgen(constructor)]
    pub fn new(data: Vec<u8>) -> Result<MapViewer, JsError> {
        Ok(MapViewer { map: MapFile::from_bytes(data).map_err(js_err)? })
    }

    /// JSON: `{"bounds":[w,s,e,n],"minzoom":..,"maxzoom":..,"createdBy":..}`.
    pub fn info(&self) -> String {
        let h = &self.map.header;
        let b = &h.bbox;
        let min = h.subfiles.iter().map(|s| s.min_zoom).min().unwrap_or(0);
        let max = h.subfiles.iter().map(|s| s.max_zoom).max().unwrap_or(0);
        let by = h.created_by.as_deref().unwrap_or("").replace('\\', "\\\\").replace('"', "\\\"");
        format!(
            "{{\"bounds\":[{},{},{},{}],\"minzoom\":{min},\"maxzoom\":{max},\"createdBy\":\"{by}\"}}",
            f64::from(b.min_lon) / 1e6,
            f64::from(b.min_lat) / 1e6,
            f64::from(b.max_lon) / 1e6,
            f64::from(b.max_lat) / 1e6
        )
    }

    /// The Mapbox Vector Tile for `z/x/y`.
    pub fn tile(&mut self, z: u8, x: u32, y: u32) -> Result<Vec<u8>, JsError> {
        self.map.tile(z, x, y).map_err(js_err)
    }
}
