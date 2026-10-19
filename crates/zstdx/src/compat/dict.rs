//! Prepared dictionaries and dictionary training mirroring `zstd::dict`.
//!
//! The two wrapper types parse once, at [`EncoderDictionary::copy`] /
//! [`DecoderDictionary::copy`] time, and every later use shares the parse —
//! the `CDict`/`DDict` role without `zstd_safe`. A dictionary that fails to
//! parse still constructs (the `zstd` crate's `copy` is infallible too); the
//! raw bytes are kept and the parse re-run on use, replaying the failure at
//! the first compression/decompression — where libzstd surfaces it as well.
//!
//! The trainers exist under the `dict_builder` feature and delegate to
//! [`crate::dict`].

use std::{io, sync::Arc};
#[cfg(feature = "dict_builder")]
use std::{path, vec::Vec};

use crate::{decoding, encoding};

/// A dictionary prepared for compression, mirroring
/// `zstd::dict::EncoderDictionary`.
#[derive(Clone)]
pub struct EncoderDictionary {
    level: i32,
    /// The shared parse; on failure the raw bytes, re-parsed on use to
    /// regenerate the error (the error types are not `Clone`).
    parsed: Arc<Result<encoding::EncoderDictionary, Arc<[u8]>>>,
}

impl EncoderDictionary {
    /// Creates a prepared dictionary for compression, keeping its own copy
    /// of the data. `level` is the level compressions through this
    /// dictionary run at (the `zstd` crate's `with_prepared_dictionary`
    /// semantics: the dictionary's level wins over any context level).
    pub fn copy(dictionary: &[u8], level: i32) -> Self {
        let parsed = match encoding::EncoderDictionary::parse(dictionary) {
            Ok(parsed) => Ok(parsed),
            Err(_) => Err(dictionary.into()),
        };
        Self {
            level,
            parsed: Arc::new(parsed),
        }
    }

    /// The level this dictionary compresses at.
    pub(super) fn compression_level(&self) -> i32 {
        self.level
    }

    /// Attaches the parsed dictionary to `options`, or replays the parse
    /// failure kept from [`Self::copy`].
    pub(super) fn apply(
        &self,
        options: crate::EncoderOptions,
    ) -> io::Result<crate::EncoderOptions> {
        match &*self.parsed {
            Ok(parsed) => Ok(options.parsed_dictionary(parsed)),
            Err(raw) => Err(io::Error::from(
                match encoding::EncoderDictionary::parse(raw) {
                    // The same bytes already failed this exact parse.
                    Ok(_) => core::unreachable!("a failed dictionary parse fails again"),
                    Err(e) => e,
                },
            )),
        }
    }
}

impl core::fmt::Debug for EncoderDictionary {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EncoderDictionary")
            .field("level", &self.level)
            .field("parsed", &self.parsed.is_ok())
            .finish_non_exhaustive()
    }
}

/// A dictionary prepared for decompression, mirroring
/// `zstd::dict::DecoderDictionary`.
#[derive(Clone)]
pub struct DecoderDictionary {
    /// The shared parse; on failure the raw bytes, re-parsed on use to
    /// regenerate the error (the error types are not `Clone`).
    parsed: Arc<Result<Arc<decoding::Dictionary>, Arc<[u8]>>>,
}

impl DecoderDictionary {
    /// Creates a prepared dictionary for decompression, keeping its own
    /// copy of the data.
    pub fn copy(dictionary: &[u8]) -> Self {
        let parsed = match decoding::Dictionary::load(dictionary) {
            Ok(parsed) => Ok(Arc::new(parsed)),
            Err(_) => Err(dictionary.into()),
        };
        Self {
            parsed: Arc::new(parsed),
        }
    }

    /// The shared parse for attaching to a decoder, or the replayed parse
    /// failure kept from [`Self::copy`].
    pub(super) fn shared(&self) -> io::Result<Arc<decoding::Dictionary>> {
        match &*self.parsed {
            Ok(parsed) => Ok(Arc::clone(parsed)),
            Err(raw) => Err(io::Error::from(crate::Error::Dictionary(
                // The same bytes already failed this parse.
                decoding::Dictionary::load(raw).expect_err("a failed dictionary parse fails again"),
            ))),
        }
    }
}

impl core::fmt::Debug for DecoderDictionary {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DecoderDictionary")
            .field("parsed", &self.parsed.is_ok())
            .finish_non_exhaustive()
    }
}

/// Trains a dictionary from a big continuous chunk of data, with all
/// samples contiguous in memory (delegates to [`crate::dict`]'s formatted
/// trainer under the `dict_builder` feature).
///
/// * `sample_data` is the concatenation of all sample data.
/// * `sample_sizes` is the size of each sample; the sum must equal `sample_data.len()`.
/// * `max_size` is the maximum size of the dictionary to generate.
#[cfg(feature = "dict_builder")]
#[cfg_attr(docsrs, doc(cfg(feature = "dict_builder")))]
pub fn from_continuous(
    sample_data: &[u8],
    sample_sizes: &[usize],
    max_size: usize,
) -> io::Result<Vec<u8>> {
    // Complain if the lengths don't add up to the entire data, with the
    // zstd crate's exact wording.
    if sample_sizes.iter().sum::<usize>() != sample_data.len() {
        return Err(io::Error::other("sample sizes don't add up"));
    }
    let samples = split_samples(sample_data, sample_sizes);
    Ok(train(&samples, max_size))
}

/// Trains a dictionary from multiple samples.
///
/// * `samples` is a list of individual samples to train on.
/// * `max_size` is the maximum size of the dictionary to generate.
#[cfg(feature = "dict_builder")]
#[cfg_attr(docsrs, doc(cfg(feature = "dict_builder")))]
pub fn from_samples<S: AsRef<[u8]>>(samples: &[S], max_size: usize) -> io::Result<Vec<u8>> {
    let refs: Vec<&[u8]> = samples.iter().map(AsRef::as_ref).collect();
    Ok(train(&refs, max_size))
}

/// Trains a dictionary from an iterator of fallible readers, mirroring
/// `zstd::dict::from_sample_iterator`: each item is read to end as one
/// sample, and read errors propagate.
#[cfg(feature = "dict_builder")]
#[cfg_attr(docsrs, doc(cfg(feature = "dict_builder")))]
pub fn from_sample_iterator<I, R>(samples: I, max_size: usize) -> io::Result<Vec<u8>>
where
    I: IntoIterator<Item = io::Result<R>>,
    R: io::Read,
{
    let mut data = Vec::new();
    let mut sizes = Vec::new();
    for sample in samples {
        let mut sample = sample?;
        sizes.push(sample.read_to_end(&mut data)?);
    }
    from_continuous(&data, &sizes, max_size)
}

/// Trains a dictionary from a list of files; each file is one sample.
#[cfg(feature = "dict_builder")]
#[cfg_attr(docsrs, doc(cfg(feature = "dict_builder")))]
pub fn from_files<I, P>(filenames: I, max_size: usize) -> io::Result<Vec<u8>>
where
    P: AsRef<path::Path>,
    I: IntoIterator<Item = P>,
{
    from_sample_iterator(filenames.into_iter().map(std::fs::File::open), max_size)
}

/// Splits the continuous buffer back into samples (the caller checked the
/// sizes add up to the length).
#[cfg(feature = "dict_builder")]
fn split_samples<'a>(data: &'a [u8], sizes: &[usize]) -> Vec<&'a [u8]> {
    let mut samples = Vec::with_capacity(sizes.len());
    let mut offset = 0;
    for &size in sizes {
        samples.push(&data[offset..offset + size]);
        offset += size;
    }
    samples
}

#[cfg(feature = "dict_builder")]
fn train(samples: &[&[u8]], max_size: usize) -> Vec<u8> {
    crate::dict::train_formatted(samples, max_size, &crate::dict::TrainConfig::default()).dict
}
