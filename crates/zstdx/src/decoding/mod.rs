//! Structures and utilities used for decoding zstd formatted data

pub mod errors;
#[cfg(all(feature = "std", feature = "hash"))]
mod frame_checksum;
mod frame_decoder;
#[cfg(feature = "seq_dump")]
pub mod seq_dump;
mod streaming_decoder;

pub use dictionary::Dictionary;
pub use frame_decoder::{BlockDecodingStrategy, DEFAULT_MAX_WINDOW_SIZE, FrameDecoder};
#[cfg(feature = "std")]
#[doc(hidden)]
pub use mt::{mt_decode_all_for_tests, mt_decode_to_vec_for_tests};
pub use streaming_decoder::StreamingDecoder;

pub(crate) mod block_decoder;
pub(crate) mod decode_buffer;
pub(crate) mod dictionary;
pub(crate) mod flat_buffer;
pub(crate) mod frame;
pub(crate) mod frame_source;
pub(crate) mod literals_section_decoder;
#[cfg(feature = "std")]
pub(crate) mod mt;
#[cfg(feature = "std")]
pub(crate) mod mt_pieces;
mod ringbuffer;
#[allow(dead_code)]
pub(crate) mod scratch;
pub(crate) mod sequence_execution;
pub(crate) mod sequence_section_decoder;
