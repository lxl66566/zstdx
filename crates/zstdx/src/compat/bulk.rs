//! Buffer-to-buffer compression mirroring `zstd::bulk`.

use std::{io, vec::Vec};

use super::{
    dict::{DecoderDictionary, EncoderDictionary},
    map_level,
};
use crate::decoding::frame::read_frame_header;

/// Compresses a single block of data to a Vec.
///
/// Like the `zstd` crate's bulk functions, the frame header declares the
/// content size.
pub fn compress(data: &[u8], level: i32) -> io::Result<Vec<u8>> {
    Compressor::new(level)?.compress(data)
}

/// Compresses a single block of data to a writable buffer.
///
/// Fails if the buffer does not have enough space for the compressed frame.
pub fn compress_to_buffer(data: &[u8], destination: &mut [u8], level: i32) -> io::Result<usize> {
    Compressor::new(level)?.compress_to_buffer(data, destination)
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
        // `load` (not `decode_dict`): headerless raw content is a valid
        // dictionary here, as on every other decode entry point.
        let dict = alloc::sync::Arc::new(
            crate::decoding::Dictionary::load(dictionary)
                .map_err(|e| io::Error::from(crate::Error::Dictionary(e)))?,
        );
        inner
            .add_options_dict(dict)
            .map_err(|e| io::Error::from(crate::Error::Frame(e)))?;
        Ok(Self { inner })
    }

    /// Creates a new decompressor using an existing prepared
    /// [`DecoderDictionary`].
    pub fn with_prepared_dictionary(dictionary: &DecoderDictionary) -> io::Result<Self> {
        let mut inner = crate::decoding::FrameDecoder::new();
        let dict = dictionary.shared()?;
        inner
            .add_options_dict(dict)
            .map_err(|e| io::Error::from(crate::Error::Frame(e)))?;
        Ok(Self { inner })
    }

    /// Changes the dictionary used by this decompressor. Affects future
    /// decompressions; dictionaries attached earlier stay available to the
    /// frames that want them (this crate keys dictionaries by id, where
    /// the zstd crate's context holds one — an empty slice detaches a raw
    /// id-less dictionary, named ones stay reachable by their id).
    pub fn set_dictionary(&mut self, dictionary: &[u8]) -> io::Result<()> {
        // `load` (not `decode_dict`): headerless raw content is a valid
        // dictionary here, as on every other decode entry point.
        let dict = alloc::sync::Arc::new(
            crate::decoding::Dictionary::load(dictionary)
                .map_err(|e| io::Error::from(crate::Error::Dictionary(e)))?,
        );
        self.inner
            .add_options_dict(dict)
            .map_err(|e| io::Error::from(crate::Error::Frame(e)))
    }

    /// Changes the dictionary used by this decompressor to a prepared
    /// [`DecoderDictionary`].
    pub fn set_prepared_dictionary(&mut self, dictionary: &DecoderDictionary) -> io::Result<()> {
        let dict = dictionary.shared()?;
        self.inner
            .add_options_dict(dict)
            .map_err(|e| io::Error::from(crate::Error::Frame(e)))
    }

    /// Decompresses into the Vec, growing it if the capacity is too small.
    /// Decodes through this decompressor, so a dictionary attached at
    /// construction applies (the zstd crate's semantics).
    pub fn decompress(&mut self, source: &[u8], capacity: usize) -> io::Result<Vec<u8>> {
        let mut capacity = capacity.max(64 * 1024);
        loop {
            let mut out = Vec::with_capacity(capacity);
            match self.inner.decode_all_to_vec(source, &mut out) {
                Ok(()) => return Ok(out),
                Err(crate::decoding::errors::FrameDecoderError::TargetTooSmall) => capacity *= 2,
                Err(e) => return Err(io::Error::from(crate::Error::Frame(e))),
            }
        }
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

    /// Frame content size from the leading frame header, with
    /// `ZSTD_getFrameContentSize` semantics: a header without the FCS
    /// field is an unknown size (`None`), not a bogus zero; a complete
    /// skippable frame is `Some(0)`; an unreadable head is `None`.
    ///
    /// The `zstd` crate only answers this with its `experimental` feature
    /// (`zstd_safe::decompress_bound`); this porting layer always can.
    pub fn upper_bound(data: &[u8]) -> Option<usize> {
        match read_frame_header(data) {
            // A complete skippable frame carries no content; a truncated
            // one is unreadable (both the reference's answers).
            Err(crate::decoding::errors::ReadFrameHeaderError::SkipFrame { length, .. })
                if data.len() >= 8 + length as usize =>
            {
                Some(0)
            },
            Ok((header, _)) if header.descriptor.declares_content_size() => {
                usize::try_from(header.frame_content_size()).ok()
            },
            // No FCS field (unknown size), a truncated skippable head, or
            // an unreadable head.
            _ => None,
        }
    }
}

impl Default for Decompressor {
    fn default() -> Self {
        Self::new().expect("frame decoder construction is infallible")
    }
}

/// The dictionary attached to a [`Compressor`]: raw bytes at the context's
/// level, or a prepared dictionary compressing at its own level.
#[derive(Debug)]
enum Attachment {
    None,
    /// Raw bytes from `set_dictionary`/`with_dictionary`, not yet parsed.
    Raw(Vec<u8>),
    /// The cached parse of formerly-raw bytes (resolved at the first
    /// compression after the attachment was set): the shared dictionary on
    /// success, the raw bytes back on failure so the error replays at
    /// every later compression (the error type is not `Clone`). Any
    /// re-set of the attachment discards the cache wholesale.
    RawParsed(Result<alloc::sync::Arc<crate::encoding::dictionary::EncDictionary>, Vec<u8>>),
    Prepared(EncoderDictionary),
}

/// An encoder that can be used to compress multiple frames, mirroring
/// `zstd::bulk::Compressor`.
#[derive(Debug)]
pub struct Compressor {
    level: i32,
    dict: Attachment,
    /// libzstd's dictIDFlag, a context parameter that outlives dictionary
    /// changes (unlike the dictionary itself).
    include_dictid: bool,
    /// Explicit long-distance-matching switch (the zstd crate's bool maps
    /// true to force-enable, false to libzstd's auto value).
    long_distance_matching: crate::LdmMode,
}

impl Compressor {
    /// Creates a new compressor.
    pub fn new(level: i32) -> io::Result<Self> {
        Ok(Self {
            level,
            dict: Attachment::None,
            include_dictid: true,
            long_distance_matching: crate::LdmMode::Auto,
        })
    }

    /// Creates a new compressor, using the given dictionary.
    ///
    /// Note that using a dictionary means that decompression will need to
    /// use the same dictionary.
    pub fn with_dictionary(level: i32, dictionary: &[u8]) -> io::Result<Self> {
        let mut compressor = Self::new(level)?;
        compressor.set_dictionary(level, dictionary)?;
        Ok(compressor)
    }

    /// Creates a new compressor using an existing prepared
    /// [`EncoderDictionary`]; the compression level is the one the
    /// dictionary was created with.
    ///
    /// Note that using a dictionary means that decompression will need to
    /// use the same dictionary.
    pub fn with_prepared_dictionary(dictionary: &EncoderDictionary) -> io::Result<Self> {
        Ok(Self {
            level: dictionary.compression_level(),
            dict: Attachment::Prepared(dictionary.clone()),
            include_dictid: true,
            long_distance_matching: crate::LdmMode::Auto,
        })
    }

    /// Changes the compression level used by this compressor.
    ///
    /// *This will clear any dictionary previously registered* (the zstd
    /// crate's semantics: pass the dictionary back through
    /// [`Self::set_dictionary`] to keep it).
    pub fn set_compression_level(&mut self, level: i32) -> io::Result<()> {
        self.set_dictionary(level, &[])
    }

    /// Changes the dictionary and compression level used by this
    /// compressor (an empty slice detaches the dictionary). Affects future
    /// compression jobs.
    ///
    /// Note that using a dictionary means that decompression will need to
    /// use the same dictionary.
    pub fn set_dictionary(&mut self, level: i32, dictionary: &[u8]) -> io::Result<()> {
        self.level = level;
        self.dict = if dictionary.is_empty() {
            Attachment::None
        } else {
            Attachment::Raw(dictionary.to_vec())
        };
        Ok(())
    }

    /// Changes the dictionary used by this compressor to a prepared
    /// [`EncoderDictionary`]; the level the dictionary was created with is
    /// used. Affects future compression jobs.
    ///
    /// Note that using a dictionary means that decompression will need to
    /// use the same dictionary.
    pub fn set_prepared_dictionary(&mut self, dictionary: &EncoderDictionary) -> io::Result<()> {
        self.dict = Attachment::Prepared(dictionary.clone());
        Ok(())
    }

    /// Enables or disables storing of the dict id (default on). With it
    /// off the dictionary still applies — content, entropy tables,
    /// repcodes — but the frame header carries no Dictionary_ID, so
    /// decoders need the dictionary explicitly and decoding with a wrong
    /// one is undefined. Affects future compression jobs; survives
    /// dictionary changes, like any context parameter.
    pub fn include_dictid(&mut self, include: bool) -> io::Result<()> {
        self.include_dictid = include;
        Ok(())
    }

    /// Enables or disables long-distance matching. `true` forces the
    /// far-match source on and, with no `window_log` set, raises the
    /// window to 27 as libzstd does; `false` restores the automatic
    /// per-level arming — the zstd crate's bool maps onto libzstd's auto
    /// value there, not an explicit disable. Affects future compression
    /// jobs.
    pub fn long_distance_matching(&mut self, enable: bool) -> io::Result<()> {
        self.long_distance_matching = if enable {
            crate::LdmMode::Enable
        } else {
            crate::LdmMode::Auto
        };
        Ok(())
    }

    /// Compresses a single block of data. The frame header declares the
    /// content size, as libzstd's one-shot compressor does.
    pub fn compress(&mut self, data: &[u8]) -> io::Result<Vec<u8>> {
        // We could allocate exactly compress_bound(len); the native path
        // already sizes its output, and the Vec is handed back as produced.
        let options = self.options(data.len() as u64)?;
        crate::bulk::compress_with(data, &options).map_err(io::Error::from)
    }

    /// Compresses a single block into the caller's buffer.
    pub fn compress_to_buffer(&mut self, data: &[u8], destination: &mut [u8]) -> io::Result<usize> {
        let compressed = self.compress(data)?;
        if destination.len() < compressed.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "destination buffer too small for the compressed frame",
            ));
        }
        destination[..compressed.len()].copy_from_slice(&compressed);
        Ok(compressed.len())
    }

    /// The options for one compression job: the effective level (a prepared
    /// dictionary carries its own) and the content-size pledge the `zstd`
    /// crate's bulk frames are written with. A raw attachment resolves its
    /// parse here, lazily: the parse ran at every compress call before, so
    /// an invalid dictionary errors at the first compress call (not at
    /// `set_dictionary`), and the outcome caches for every later call.
    fn options(&mut self, len: u64) -> io::Result<crate::EncoderOptions> {
        if matches!(self.dict, Attachment::Raw(_)) {
            let Attachment::Raw(raw) = core::mem::replace(&mut self.dict, Attachment::None) else {
                unreachable!("guarded by matches!");
            };
            self.dict = match crate::EncoderDictionary::parse(&raw) {
                Ok(parsed) => Attachment::RawParsed(Ok(parsed.shared())),
                Err(e) => {
                    self.dict = Attachment::RawParsed(Err(raw));
                    return Err(io::Error::from(e));
                },
            };
        }
        let level = match &self.dict {
            Attachment::Prepared(dict) => dict.compression_level(),
            _ => self.level,
        };
        let mut options = crate::EncoderOptions::new(map_level(level)).pledged_size(Some(len));
        options.include_dictid = self.include_dictid;
        options.long_distance_matching = self.long_distance_matching;
        match &self.dict {
            Attachment::None => Ok(options),
            Attachment::Raw(_) => unreachable!("resolved above"),
            Attachment::RawParsed(Ok(dict)) => {
                options.parsed_dictionary = Some(alloc::sync::Arc::clone(dict));
                Ok(options)
            },
            // Replay: the same bytes already failed this exact parse.
            Attachment::RawParsed(Err(raw)) => Err(io::Error::from(
                match crate::EncoderDictionary::parse(raw) {
                    Ok(_) => unreachable!("a failed dictionary parse fails again"),
                    Err(e) => e,
                },
            )),
            Attachment::Prepared(dict) => dict.apply(options),
        }
    }
}

#[test]
fn contexts_are_send() {
    fn assert_send<T: Send>(_: T) {}
    assert_send(Compressor::new(0));
    assert_send(Decompressor::new());
}
