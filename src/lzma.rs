//! LZMA ("lzma alone", `.lzma`) compression helpers.

use std::io::Read;

use anyhow::Result;
use xz2::read::XzDecoder;
use xz2::stream::{LzmaOptions, Stream};

/// Compresses data into the legacy `.lzma` container used for Wahoo tiles.
pub fn compress(data: &[u8]) -> Result<Vec<u8>> {
    let options = LzmaOptions::new_preset(6)?;
    let stream = Stream::new_lzma_encoder(&options)?;
    let mut encoder = xz2::read::XzEncoder::new_stream(data, stream);
    let mut out = Vec::new();
    encoder.read_to_end(&mut out)?;
    Ok(out)
}

/// Decompresses a `.lzma` container.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    let stream = Stream::new_lzma_decoder(u64::MAX)?;
    let mut decoder = XzDecoder::new_stream(data, stream);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
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
