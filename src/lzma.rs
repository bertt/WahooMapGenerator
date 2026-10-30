//! LZMA ("lzma alone", `.lzma`) compression helpers, pure Rust so they also run as WebAssembly.

use std::io::{Read, Write};

use anyhow::{Result, anyhow};
use lzma_rust2::{LzmaOptions, LzmaReader, LzmaWriter};

const PRESET: u32 = 6;
const CHUNK: usize = 1 << 20;

/// Compresses data into the legacy `.lzma` container used for Wahoo tiles.
pub fn compress(data: &[u8]) -> Result<Vec<u8>> {
    compress_with_progress(data, |_| {})
}

/// Like [`compress`], calling `progress(bytes_done)` after every megabyte of input.
pub fn compress_with_progress(data: &[u8], mut progress: impl FnMut(usize)) -> Result<Vec<u8>> {
    let mut writer = LzmaWriter::new_use_header(Vec::new(), &LzmaOptions::with_preset(PRESET), None)?;
    let mut done = 0;
    for chunk in data.chunks(CHUNK) {
        writer.write_all(chunk)?;
        done += chunk.len();
        progress(done);
    }
    Ok(writer.finish()?)
}

/// Decompresses a `.lzma` container.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    let mut reader = LzmaReader::new_mem_limit(data, u32::MAX, None).map_err(|e| anyhow!("invalid lzma data: {e}"))?;
    let mut out = Vec::new();
    reader.read_to_end(&mut out).map_err(|e| anyhow!("invalid lzma data: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let packed = compress(&data).unwrap();
        assert!(packed.len() < data.len());
        assert_eq!(decompress(&packed).unwrap(), data);
    }

    #[test]
    fn corrupt_input_is_an_error() {
        assert!(decompress(b"definitely not lzma data").is_err());
    }
}
