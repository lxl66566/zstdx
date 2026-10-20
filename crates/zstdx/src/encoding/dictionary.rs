//! Encoder-side dictionary support: parse a zstd dictionary (formatted, or
//! raw content without a header), convert its entropy tables into encoder
//! tables, and seed a fresh [`CompressState`] with dictionary content as
//! match history and the tables as the frame's reusable entropy state.

use alloc::vec::Vec;

use crate::{
    Error, InputShape, Level,
    decoding::{Dictionary, dictionary::MAGIC_NUM},
    encoding::{
        Matcher,
        block_enc::compressed::{DictEntropy, LitStreams, SeqCostMode},
        frame_compressor::CompressState,
    },
    fse::fse_encoder::{FSETable, build_table_from_probabilities},
    huff0::huff0_encoder::HuffmanTable,
};

/// A dictionary in encoder-usable form: the raw parse plus encoder-side
/// entropy tables derived from it.
pub(crate) struct EncDictionary {
    pub id: u32,
    /// Content that becomes the frame's match history (trimmed to the
    /// level's window at load time).
    pub content: Vec<u8>,
    pub rep: [u32; 3],
    /// Entropy tables, present only for formatted dictionaries (raw content
    /// dictionaries seed match history alone, like libzstd's content load).
    huff: Option<HuffmanTable>,
    ll: Option<FSETable>,
    ml: Option<FSETable>,
    of: Option<FSETable>,
}

// The entropy tables would dominate any Debug output; the identity of a
// dictionary is its id and content.
impl core::fmt::Debug for EncDictionary {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EncDictionary")
            .field("id", &self.id)
            .field("content_bytes", &self.content.len())
            .finish_non_exhaustive()
    }
}

impl EncDictionary {
    /// A raw-content dictionary: pure match history, no id, no entropy
    /// tables (libzstd's `ZSTD_dct_rawContent` load).
    pub(crate) fn raw_content(content: &[u8]) -> Self {
        Self {
            id: 0,
            content: content.to_vec(),
            rep: [1, 4, 8],
            huff: None,
            ll: None,
            ml: None,
            of: None,
        }
    }

    /// Parse a dictionary: formatted (as produced by `zstd --train`) or raw
    /// content (as produced by the in-tree trainer), which loads as pure
    /// match history with the format-default repcodes.
    pub(crate) fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.first_chunk::<4>() != Some(&MAGIC_NUM) {
            return Ok(Self::raw_content(raw));
        }
        let dict = Dictionary::decode_dict(raw)?;
        // libzstd's load check: a repcode must be 0 (unused) or point into
        // the content. The parse has no dedicated variant; surface it as the
        // generic dictionary error.
        for &r in &dict.offset_hist {
            if r != 0 && r as usize > dict.dict_content.len() {
                return Err(Error::Dictionary(
                    crate::decoding::errors::DictionaryDecodeError::NotEnoughBytes,
                ));
            }
        }
        let huff = HuffmanTable::build_from_code_lengths(dict.huf.table.code_lengths());
        let ll = build_table_from_probabilities(
            &dict.fse.literal_lengths.symbol_probabilities,
            dict.fse.literal_lengths.accuracy_log,
        );
        let ml = build_table_from_probabilities(
            &dict.fse.match_lengths.symbol_probabilities,
            dict.fse.match_lengths.accuracy_log,
        );
        let of = build_table_from_probabilities(
            &dict.fse.offsets.symbol_probabilities,
            dict.fse.offsets.accuracy_log,
        );
        Ok(Self {
            id: dict.id,
            content: dict.dict_content,
            rep: dict.offset_hist,
            huff: Some(huff),
            ll: Some(ll),
            ml: Some(ml),
            of: Some(of),
        })
    }

    /// The dictID to declare in the frame header: a zero id means "no id"
    /// (the format's null dictID) — entropy seeding is skipped there too,
    /// so the frame stays decodable without the dictionary. u32 caps the
    /// value at the Dictionary_ID field's 4-byte wire limit.
    pub fn header_id(&self) -> Option<u32> {
        (self.id != 0).then_some(self.id)
    }
}

/// Reset `state` for a dictionary frame: shape applied, dictionary content
/// loaded as match history, and — when the dictionary carries an id — its
/// entropy tables installed as the frame's reusable previous tables (the
/// first blocks may then emit treeless literals / repeat-mode sequences,
/// exactly like between-block reuse; the dictID in the header makes that
/// legal). The length in `shape` must already include the dictionary
/// content (libzstd clamps the window by src+dict).
#[cfg(any(not(feature = "std"), feature = "dict_builder"))]
pub(crate) fn reset_with_dictionary<M: Matcher>(
    state: &mut CompressState<M>,
    dict: &EncDictionary,
    level: Level,
    shape: InputShape,
) {
    reset_with_dictionary_mode(state, dict, level, shape, crate::LdmMode::Auto);
}

/// [`reset_with_dictionary`] with the frame's explicit LDM switch declared
/// before the reset (the reset's size gate reads it).
pub(crate) fn reset_with_dictionary_mode<M: Matcher>(
    state: &mut CompressState<M>,
    dict: &EncDictionary,
    level: Level,
    shape: InputShape,
    ldm: crate::LdmMode,
) {
    state.matcher.set_ldm_mode(ldm);
    state.matcher.set_input_shape(shape);
    state.matcher.reset(level);
    state
        .matcher
        .load_dictionary(&dict.content, dict.rep, level);
    state.last_huff_table = None;
    state.fse_tables.ll_previous = None;
    state.fse_tables.ml_previous = None;
    state.fse_tables.of_previous = None;
    state.dict_entropy = DictEntropy::default();
    state.split = super::pre_split::FrameSavings::new();
    if dict.id != 0 {
        state.last_huff_table.clone_from(&dict.huff);
        state.fse_tables.ll_previous.clone_from(&dict.ll);
        state.fse_tables.ml_previous.clone_from(&dict.ml);
        state.fse_tables.of_previous.clone_from(&dict.of);
        state.dict_entropy = DictEntropy::ALL;
    }
    // Sequence-table selection semantics: mirror which arm of libzstd's
    // `ZSTD_selectEncodingType` the frame's strategy takes. Levels 5+ are
    // the lazy family or above in both of libzstd's cParams tables, so
    // dictionary frames there run the exact three-way cost comparison;
    // The superblock literals layout keys on the frame's known length
    // like every plain frame (dict content included in `shape.len`).
    state.dict_entropy.lit_streams = LitStreams::for_frame(shape.len);
    // level 4 is greedy's tighter heuristic bar. Fast rows (1-3) keep the
    // stock heuristic. Sticky per frame (survives the seeding flags
    // clearing above); no-dict frames never see it.
    state.dict_entropy.cost_mode = match level.as_i32() {
        1..=3 => SeqCostMode::Stock,
        4 => SeqCostMode::Greedy,
        _ => SeqCostMode::Exact,
    };
}

/// Single-entry dictionary seed image cache, riding a pooled
/// [`CompressState`] (see `seed_with_cache`): after a state is seeded for
/// `(dictionary, level, shape, ldm mode)`, the image of everything the
/// seeding wrote is captured; a later seeding with the same key restores
/// the image instead of re-running `reset_with_dictionary` — the
/// single-threaded counterpart of libzstd's CDict attach economics (digest
/// once, re-arm near-free), minus its shared match state (each pooled
/// state carries its own image, so no locking is involved: the slot is
/// only touched while the owning thread holds the state).
///
/// The key is collision-proof by construction: the dictionary enters as
/// `Arc` target identity and the image holds its own `Arc` clone, so the
/// address cannot be recycled by a different dictionary while the image
/// lives; level, shape and LDM mode are the remaining inputs the seeding
/// reads (`reset` + `load_dictionary` derive every table size, row swap
/// and gate from them; the reach choice and LDM arming context are forced
/// to the deterministic dict-frame values at every `seed_with_cache`
/// call, so they need no key slot). Restores are exact by construction:
/// the image is the full overwrite of everything seeding wrote (see
/// `match_generator::DriverSeedImage` for the deliberate exceptions and
/// their neutrality arguments).
#[cfg(feature = "std")]
pub(crate) struct SeedCache {
    /// Pins the key's address (see the type doc).
    dict: alloc::sync::Arc<EncDictionary>,
    level: Level,
    shape: InputShape,
    ldm: crate::LdmMode,
    driver: super::match_generator::DriverSeedImage,
    last_huff_table: Option<HuffmanTable>,
    ll_previous: Option<FSETable>,
    ml_previous: Option<FSETable>,
    of_previous: Option<FSETable>,
    dict_entropy: DictEntropy,
}

#[cfg(feature = "std")]
impl SeedCache {
    fn matches(
        &self,
        dict: &alloc::sync::Arc<EncDictionary>,
        level: Level,
        shape: InputShape,
        ldm: crate::LdmMode,
    ) -> bool {
        alloc::sync::Arc::ptr_eq(&self.dict, dict)
            && self.level == level
            && self.shape == shape
            && self.ldm == ldm
    }

    fn restore_into<M: Matcher>(&self, state: &mut CompressState<M>) {
        state.matcher.restore_seed_image(&self.driver);
        state.last_huff_table.clone_from(&self.last_huff_table);
        state.fse_tables.ll_previous.clone_from(&self.ll_previous);
        state.fse_tables.ml_previous.clone_from(&self.ml_previous);
        state.fse_tables.of_previous.clone_from(&self.of_previous);
        state.dict_entropy = self.dict_entropy;
        state.split = super::pre_split::FrameSavings::new();
    }
}

/// Seed `state` for a dictionary frame through its single-entry image
/// cache: a key hit restores the captured post-seed image, a miss runs
/// the full seeding (see [`reset_with_dictionary_mode`]) and captures an
/// image when the matcher provides one (the built-in driver). The reach
/// choice and LDM arming context are forced to the dict-frame determinism
/// first (the fresh-state values every dict entry point implies today),
/// so the key needs no slot for them.
#[cfg(feature = "std")]
pub(crate) fn seed_with_cache<M: Matcher>(
    state: &mut CompressState<M>,
    dict: &alloc::sync::Arc<EncDictionary>,
    level: Level,
    shape: InputShape,
    ldm: crate::LdmMode,
) {
    state.matcher.set_ldm_mode(ldm);
    state.matcher.force_dict_frame_inputs();
    // The slot is taken out for the restore: the image lives inside the
    // state it restores, so the borrow must not overlap.
    let parked = state.seed_cache.take();
    if let Some(cache) = parked
        .as_ref()
        .filter(|c| c.matches(dict, level, shape, ldm))
    {
        cache.restore_into(state);
        state.seed_cache = parked;
        return;
    }
    reset_with_dictionary_mode(state, dict, level, shape, ldm);
    state.matcher.canonicalize_for_seed_capture();
    if let Some(driver) = state.matcher.capture_seed_image() {
        state.seed_cache = Some(alloc::boxed::Box::new(SeedCache {
            driver,
            last_huff_table: state.last_huff_table.clone(),
            ll_previous: state.fse_tables.ll_previous.clone(),
            ml_previous: state.fse_tables.ml_previous.clone(),
            of_previous: state.fse_tables.of_previous.clone(),
            dict_entropy: state.dict_entropy,
            dict: alloc::sync::Arc::clone(dict),
            level,
            shape,
            ldm,
        }));
    }
}

/// A parsed dictionary shared across any number of compressions: the parse
/// (content split plus the encoder-side entropy-table digest) runs once, at
/// [Self::parse], and every frame built from it re-seeds its own matcher
/// from the shared tables — the per-frame content load into a fresh matcher
/// pays the frame model's re-arm, exactly as libzstd's CDict reuses the
/// digested dictionary but still re-arms the frame from it; the
/// single-threaded entry points cache the re-arm itself as a seed image
/// (see [`seed_with_cache`]).
#[derive(Clone)]
pub struct EncoderDictionary {
    inner: alloc::sync::Arc<EncDictionary>,
}

impl EncoderDictionary {
    /// Parse once, reuse for every compression (see [EncoderOptions]'s
    /// [EncoderOptions::parsed_dictionary] builder).
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        Ok(Self {
            inner: alloc::sync::Arc::new(EncDictionary::parse(raw)?),
        })
    }

    /// The dictID frames built from this dictionary declare (see [EncDictionary::header_id]).
    pub fn header_id(&self) -> Option<u32> {
        self.inner.header_id()
    }

    pub(crate) fn shared(&self) -> alloc::sync::Arc<EncDictionary> {
        alloc::sync::Arc::clone(&self.inner)
    }
}
