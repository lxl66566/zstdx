//! The [StreamingDecoder] wraps a [FrameDecoder] and provides a Read impl that decodes data when necessary

use core::borrow::BorrowMut;

use crate::decoding::errors::FrameDecoderError;
use crate::decoding::{frame_source, BlockDecodingStrategy, FrameDecoder};
#[cfg(not(feature = "std"))]
use crate::io::ErrorKind;
use crate::io::{Error, Read};

/// High level Zstandard frame decoder that can be used to decompress a given Zstandard frame.
///
/// This decoder implements `io::Read`, so you can interact with it by calling
/// `io::Read::read_to_end` / `io::Read::read_exact` or passing this to another library / module as a source for the decoded content
///
/// The stream may contain any number of concatenated frames, with skippable
/// frames in between; they are decoded transparently, as the reference
/// decoders do. Reads return `Ok(0)` once the last frame is drained; trailing
/// bytes that do not start a valid frame surface as an error.
///
/// If you need more control over how decompression takes place, you can use
/// the lower level [FrameDecoder], which allows for greater control over how
/// decompression takes place but the implementor must call
/// [FrameDecoder::decode_blocks] repeatedly to decode the entire frame.
///
/// ```no_run
/// // `read_to_end` is not implemented by the no_std implementation.
/// #[cfg(feature = "std")]
/// {
///     use std::fs::File;
///     use std::io::Read;
///     use zstdx::decoding::StreamingDecoder;
///
///     // Read a Zstandard archive from the filesystem then decompress it into a vec.
///     let mut f: File = todo!("Read a .zstd archive from somewhere");
///     let mut decoder = StreamingDecoder::new(f).unwrap();
///     let mut result = Vec::new();
///     Read::read_to_end(&mut decoder, &mut result).unwrap();
/// }
/// ```
pub struct StreamingDecoder<READ: Read, DEC: BorrowMut<FrameDecoder>> {
    pub decoder: DEC,
    source: READ,
}

impl<READ: Read, DEC: BorrowMut<FrameDecoder>> StreamingDecoder<READ, DEC> {
    pub fn new_with_decoder(
        mut source: READ,
        mut decoder: DEC,
    ) -> Result<StreamingDecoder<READ, DEC>, FrameDecoderError> {
        frame_source::init_first_frame(&mut source, decoder.borrow_mut())?;
        Ok(StreamingDecoder { decoder, source })
    }
}

impl<READ: Read> StreamingDecoder<READ, FrameDecoder> {
    pub fn new(
        mut source: READ,
    ) -> Result<StreamingDecoder<READ, FrameDecoder>, FrameDecoderError> {
        let mut decoder = FrameDecoder::new();
        frame_source::init_first_frame(&mut source, &mut decoder)?;
        Ok(StreamingDecoder { decoder, source })
    }

    /// Like [StreamingDecoder::new], but first raises the wrapped decoder's
    /// window limit to `max_window_size`. See [FrameDecoder::set_max_window_size]
    /// for the semantics and the security caveat.
    pub fn new_with_max_window_size(
        mut source: READ,
        max_window_size: u64,
    ) -> Result<StreamingDecoder<READ, FrameDecoder>, FrameDecoderError> {
        let mut decoder = FrameDecoder::new();
        decoder.set_max_window_size(max_window_size);
        frame_source::init_first_frame(&mut source, &mut decoder)?;
        Ok(StreamingDecoder { decoder, source })
    }
}

impl<READ: Read, DEC: BorrowMut<FrameDecoder>> StreamingDecoder<READ, DEC> {
    /// Gets a reference to the underlying reader.
    pub fn get_ref(&self) -> &READ {
        &self.source
    }

    /// Gets a mutable reference to the underlying reader.
    ///
    /// It is inadvisable to directly read from the underlying reader.
    pub fn get_mut(&mut self) -> &mut READ {
        &mut self.source
    }

    /// Destructures this object into the inner reader.
    pub fn into_inner(self) -> READ
    where
        READ: Sized,
    {
        self.source
    }

    /// Destructures this object into both the inner reader and [FrameDecoder].
    pub fn into_parts(self) -> (READ, DEC)
    where
        READ: Sized,
    {
        (self.source, self.decoder)
    }

    /// Destructures this object into the inner [FrameDecoder].
    pub fn into_frame_decoder(self) -> DEC {
        self.decoder
    }
}

impl<READ: Read, DEC: BorrowMut<FrameDecoder>> Read for StreamingDecoder<READ, DEC> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let decoder = self.decoder.borrow_mut();
        let source = &mut self.source;

        // Loop until the decoder has decoded far enough to fill the buffer,
        // so large reads pay the per-call overhead once per buffer instead of
        // once per block. Returning 0 bytes here would signal an EOF which
        // would be wrong when the decoder is not yet finished; if a call makes
        // no progress, hand back what is collectible instead of spinning.
        while decoder.can_collect() < buf.len() {
            if decoder.is_finished() {
                if decoder.can_collect() == 0 {
                    // The stream may hold more frames behind this one; a
                    // clean end of the source ends the whole stream.
                    match frame_source::init_next_frame(&mut *source, decoder) {
                        Ok(true) => continue,
                        Ok(false) => return Ok(0),
                        Err(e) => return Err(into_read_error(e)),
                    }
                }
                break;
            }
            let collectible_before = decoder.can_collect();
            match decoder.decode_blocks(&mut *source, BlockDecodingStrategy::UptoBlocks(1)) {
                Ok(_) => { /*Nothing to do*/ }
                Err(e) => return Err(into_read_error(e)),
            }
            if decoder.can_collect() == collectible_before && collectible_before > 0 {
                break;
            }
        }

        decoder.read(buf)
    }
}

fn into_read_error(e: FrameDecoderError) -> Error {
    #[cfg(feature = "std")]
    {
        Error::other(e)
    }
    #[cfg(not(feature = "std"))]
    {
        Error::new(ErrorKind::Other, alloc::boxed::Box::new(e))
    }
}
