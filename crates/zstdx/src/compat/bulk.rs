//! Buffer-to-buffer compression mirroring `zstd::bulk`.

use std::{io, vec::Vec};

use super::map_level;
use crate::decoding::frame::read_frame_header;

/// Compresses a single block of data to a Vec.
pub fn compress(data: &[u8], level: i32) -> io::Result<Vec<u8>> {
    Ok(crate::bulk::compress(data, map_level(level)))
}

/// Compresses a single block of data to a writable buffer.
///
/// Fails if the buffer does not have enough space for the compressed frame.
pub fn compress_to_buffer(data: &[u8], destination: &mut [u8], level: i32) -> io::Result<usize> {
    let compressed = crate::bulk::compress(data, map_level(level));
    if destination.len() < compressed.len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "destination buffer too small for the compressed frame",
        ));
    }
    destination[..compressed.len()].copy_from_slice(&compressed);
    Ok(compressed.len())
}

/// Decompresses a frame to the given Vec capacity; the buffer grows if the
/// capacity hint was too small.
pub fn decompress(source: &[u8], capacity: usize) -> io::Result<Vec<u8>> {
    crate::bulk::decompress(source, capacity).map_err(io::Error::from)
}

/// Decompresses a frame into the caller's buffer, returning the decoded
/// length.
pub fn decompress_to_buffer(source: &[u8], destination: &mut [u8]) -> io::Result<usize> {
    crate::bulk::decompress_to_buffer(source, destination).map_err(io::Error::from)
}

/// A decoder that can be used to decompress multiple frames, mirroring
/// `zstd::bulk::Decompressor`.
pub struct Decompressor {
    inner: crate::decoding::FrameDecoder,
}

impl Decompressor {
    /// Creates a new decompressor.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            inner: crate::decoding::FrameDecoder::new(),
        })
    }

    /// Creates a new decompressor bound to a dictionary.
    pub fn with_dictionary(dictionary: &[u8]) -> io::Result<Self> {
        let mut inner = crate::decoding::FrameDecoder::new();
        let dict = crate::decoding::Dictionary::decode_dict(dictionary)
            .map_err(|e| io::Error::from(crate::Error::Dictionary(e)))?;
        inner
            .add_dict(dict)
            .map_err(|e| io::Error::from(crate::Error::Frame(e)))?;
        Ok(Self { inner })
    }

    /// Decompresses into the Vec, growing it if the capacity is too small.
    pub fn decompress(&mut self, source: &[u8], capacity: usize) -> io::Result<Vec<u8>> {
        crate::bulk::decompress(source, capacity).map_err(io::Error::from)
    }

    /// Decompresses into the caller's buffer, returning the decoded length.
    pub fn decompress_to_buffer(
        &mut self,
        source: &[u8],
        destination: &mut [u8],
    ) -> io::Result<usize> {
        self.inner
            .decode_all(source, destination)
            .map_err(|e| io::Error::from(crate::Error::Frame(e)))
    }

    /// Frame content size from the leading frame header, when present.
    pub fn upper_bound(data: &[u8]) -> Option<usize> {
        read_frame_header(data)
            .ok()
            .map(|(header, _)| header.frame_content_size() as usize)
    }
}

impl Default for Decompressor {
    fn default() -> Self {
        Self::new().expect("frame decoder construction is infallible")
    }
}

/// An encoder that can be used to compress multiple frames, mirroring
/// `zstd::bulk::Compressor`.
#[derive(Debug)]
pub struct Compressor {
    level: i32,
    dict: Option<Vec<u8>>,
}

impl Compressor {
    /// Creates a new compressor.
    pub fn new(level: i32) -> io::Result<Self> {
        Ok(Self { level, dict: None })
    }

    /// Creates a compressor bound to a dictionary.
    pub fn with_dictionary(level: i32, dictionary: &[u8]) -> io::Result<Self> {
        Ok(Self {
            level,
            dict: Some(dictionary.to_vec()),
        })
    }

    /// Changes the level used by subsequent `compress` calls.
    pub fn set_compression_level(&mut self, level: i32) -> io::Result<()> {
        self.level = level;
        Ok(())
    }

    /// Sets a dictionary for subsequent compression (an empty slice
    /// detaches it).
    pub fn set_dictionary(&mut self, dictionary: &[u8]) -> io::Result<()> {
        self.dict = (!dictionary.is_empty()).then(|| dictionary.to_vec());
        Ok(())
    }

    /// Compresses a single block of data.
    pub fn compress(&mut self, data: &[u8]) -> io::Result<Vec<u8>> {
        self.compress_inner(data).map(|(v, _)| v)
    }

    /// Compresses a single block into the caller's buffer.
    pub fn compress_to_buffer(&mut self, data: &[u8], destination: &mut [u8]) -> io::Result<usize> {
        let (compressed, _) = self.compress_inner(data)?;
        if destination.len() < compressed.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "destination buffer too small for the compressed frame",
            ));
        }
        destination[..compressed.len()].copy_from_slice(&compressed);
        Ok(compressed.len())
    }

    fn compress_inner(&mut self, data: &[u8]) -> io::Result<(Vec<u8>, i32)> {
        let level = self.level;
        match &self.dict {
            Some(dict) => {
                let opts = crate::EncoderOptions::new(map_level(level)).dictionary(dict);
                Ok((crate::bulk::compress_with(data, &opts)?, level))
            },
            None => Ok((crate::bulk::compress(data, map_level(level)), level)),
        }
    }
}
