//! Encoder-side dictionary support: parse a zstd dictionary (formatted, or
//! raw content without a header), convert its entropy tables into encoder
//! tables, and seed a fresh [`CompressState`] with dictionary content as
//! match history and the tables as the frame's reusable entropy state.

use alloc::vec::Vec;

use crate::{
    Error, InputShape, Level,
    decoding::{Dictionary, dictionary::MAGIC_NUM},
    encoding::{
        block_enc::compressed::{DictEntropy, SeqCostMode},
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
pub(crate) fn reset_with_dictionary<M: crate::encoding::Matcher>(
    state: &mut CompressState<M>,
    dict: &EncDictionary,
    level: Level,
    shape: InputShape,
) {
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
    // level 4 is greedy's tighter heuristic bar. Fast rows (1-3) keep the
    // stock heuristic. Sticky per frame (survives the seeding flags
    // clearing above); no-dict frames never see it.
    state.dict_entropy.cost_mode = match level.as_i32() {
        1..=3 => SeqCostMode::Stock,
        4 => SeqCostMode::Greedy,
        _ => SeqCostMode::Exact,
    };
}
