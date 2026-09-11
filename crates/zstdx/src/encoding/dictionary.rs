//! Encoder-side dictionary support: parse a raw zstd dictionary, convert
//! its entropy tables into encoder tables, and seed a fresh
//! [`CompressState`] with dictionary content as match history and the
//! tables as the frame's reusable entropy state.

use alloc::vec::Vec;

use crate::{
    Error, InputShape, Level,
    decoding::Dictionary,
    encoding::{MatchGeneratorDriver, frame_compressor::CompressState},
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
    huff: HuffmanTable,
    ll: FSETable,
    ml: FSETable,
    of: FSETable,
}

impl EncDictionary {
    /// Parse a raw zstd dictionary (as produced by `zstd --train`).
    pub(crate) fn parse(raw: &[u8]) -> Result<Self, Error> {
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
            huff,
            ll,
            ml,
            of,
        })
    }

    /// The dictID to declare in the frame header: a zero id means "no id"
    /// (the format's null dictID) — entropy seeding is skipped there too,
    /// so the frame stays decodable without the dictionary.
    pub fn header_id(&self) -> Option<u64> {
        (self.id != 0).then(|| self.id as u64)
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
    state.matcher.load_dictionary(&dict.content, dict.rep);
    state.last_huff_table = None;
    state.fse_tables.ll_previous = None;
    state.fse_tables.ml_previous = None;
    state.fse_tables.of_previous = None;
    if dict.id != 0 {
        state.last_huff_table = Some(dict.huff.clone());
        state.fse_tables.ll_previous = Some(dict.ll.clone());
        state.fse_tables.ml_previous = Some(dict.ml.clone());
        state.fse_tables.of_previous = Some(dict.of.clone());
    }
}
