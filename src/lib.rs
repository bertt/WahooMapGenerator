//! WahooMapGenerator: builds Wahoo-compatible mapsforge map tiles from OpenStreetMap data.

#[cfg(feature = "native")]
pub mod cli;
pub mod clip;
#[cfg(feature = "native")]
pub mod download;
pub mod extract;
pub mod inspect;
pub mod lzma;
pub mod model;
pub mod mvt;
pub mod osm;
pub mod reader;
pub mod tagmap;
pub mod tile;
pub mod varint;
#[cfg(target_arch = "wasm32")]
pub mod wasm;
pub mod writer;
