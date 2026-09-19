//! Utilities and representations for a frame header.
use alloc::vec::Vec;

use crate::{
    bit_io::BitWriter,
    common::MAGIC_NUM,
    encoding::util::{find_min_size, minify_val},
};

/// A header for a single Zstandard frame.
///
/// <https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md#frame_header>
#[derive(Debug)]
pub struct FrameHeader {
    /// Optionally, the original (uncompressed) size of the data within the frame in bytes.
    /// If not present, `window_size` must be set.
    pub frame_content_size: Option<u64>,
    /// If set to true, data must be regenerated within a single
    /// continuous memory segment.
    pub single_segment: bool,
    /// If set to true, a 32 bit content checksum will be present
    /// at the end of the frame.
    pub content_checksum: bool,
    /// If a dictionary ID is provided, the ID of that dictionary. The wire
    /// field is at most 4 bytes (RFC 8878 Dictionary_ID_flag 1/2/3), so the
    /// u32 type itself rules out unserializable ids at compile time.
    pub dictionary_id: Option<u32>,
    /// The minimum memory buffer required to compress a frame. If not present,
    /// `single_segment` will be set to true. If present, this value must be greater than 1KB
    /// and less than 3.75TB. Encoders should not generate a frame that requires a window size
    /// larger than 8mb.
    pub window_size: Option<u64>,
}

impl FrameHeader {
    /// Writes the serialized frame header into the provided buffer.
    ///
    /// The returned header *does include* a frame header descriptor.
    pub fn serialize(self, output: &mut Vec<u8>) {
        vprintln!("Serializing frame with header: {self:?}");
        // https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md#frame_header
        let header = self.normalized();
        // Magic Number:
        output.extend_from_slice(&MAGIC_NUM.to_le_bytes());

        // `Frame_Header_Descriptor`:
        output.push(header.descriptor());

        // `Window_Descriptor
        // TODO: https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md#window_descriptor
        if !header.single_segment
            && let Some(window_size) = header.window_size
        {
            let log = window_size.next_power_of_two().ilog2();
            let exponent = if log > 10 {
                log - 10
            } else {
                1
            } as u8;
            output.push(exponent << 3);
        }

        if let Some(id) = header.dictionary_id {
            output.extend(minify_val(u64::from(id)));
        }

        if let Some(frame_content_size) = header.frame_content_size {
            output.extend(minify_val_fcs(frame_content_size));
        }
    }

    /// Restrict the header to what the format can actually express: without
    /// the single-segment flag the 1-byte Frame_Content_Size class does not
    /// exist (flag 0 means the field is absent), so a size below 256 bytes
    /// cannot be declared in a windowed frame. Drop the declaration instead
    /// of emitting a stray byte that shifts every following field — both
    /// this crate's and the reference decoder reject the shifted frame.
    /// The pledged size keeps shaping the encode; only the header promise is
    /// lost.
    fn normalized(mut self) -> Self {
        if !self.single_segment
            && self
                .frame_content_size
                .is_some_and(|v| find_min_size(v) == 1)
        {
            self.frame_content_size = None;
        }
        self
    }

    /// Generate a serialized frame header descriptor for the frame header.
    ///
    /// https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md#frame_header_descriptor
    fn descriptor(&self) -> u8 {
        let mut bw = BitWriter::new();
        // A frame header starts with a frame header descriptor.
        // It describes what other fields are present
        // https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md#frame_header_descriptor
        // Writing the frame header descriptor:
        // `Frame_Content_Size_flag`:
        // The Frame_Content_Size_flag specifies if
        // the Frame_Content_Size field is provided within the header.
        // TODO: The Frame_Content_Size field isn't set at all, we should prefer to include it
        // always. If the `Single_Segment_flag` is set and this value is zero,
        // the size of the FCS field is 1 byte.
        // Otherwise, the FCS field is omitted.
        // | Value | Size of field (Bytes)
        // | 0     | 0 or 1
        // | 1     | 2
        // | 2     | 4
        // | 3     | 8

        // `Dictionary_ID_flag`:
        if let Some(id) = self.dictionary_id {
            // RFC 8878: 1 -> 1 byte, 2 -> 2, 3 -> 4; the flag's 0 means no
            // field. A u32 id keeps find_min_size in 1/2/4, so the wider
            // classes are unrepresentable, not runtime-checked.
            let flag_value: u8 = match find_min_size(u64::from(id)) {
                1 => 1,
                2 => 2,
                4 => 3,
                _ => unreachable!("find_min_size of a u32 yields 1, 2 or 4"),
            };
            bw.write_bits(flag_value, 2);
        } else {
            // A `Dictionary_ID` was not provided
            bw.write_bits(0u8, 2);
        }

        // `Content_Checksum_flag`:
        if self.content_checksum {
            bw.write_bits(1u8, 1);
        } else {
            bw.write_bits(0u8, 1);
        }

        // `Reserved_bit`:
        // This value must be zero
        bw.write_bits(0u8, 1);

        // `Unused_bit`:
        // An encoder compliant with this spec must set this bit to zero
        bw.write_bits(0u8, 1);

        // `Single_Segment_flag`:
        // If this flag is set, data must be regenerated within a single continuous memory segment,
        // and the `Frame_Content_Size` field must be present in the header.
        // If this flag is not set, the `Window_Descriptor` field must be present in the frame
        // header.
        if self.single_segment {
            assert!(
                self.frame_content_size.is_some(),
                "if the `single_segment` flag is set to true, then a frame content size must be \
                 provided"
            );
            bw.write_bits(1u8, 1);
        } else {
            assert!(
                self.window_size.is_some(),
                "if the `single_segment` flag is set to false, then a window size must be provided"
            );
            bw.write_bits(0u8, 1);
        }

        if let Some(frame_content_size) = self.frame_content_size {
            let field_size = find_min_size(frame_content_size);
            // FCS flag encodes the field size (RFC 8878): 0 -> 1 byte,
            // 1 -> 2, 2 -> 4, 3 -> 8. `find_min_size` yields 1/2/4/8.
            let flag_value: u8 = match field_size {
                1 => 0,
                2 => 1,
                4 => 2,
                8 => 3,
                _ => panic!(),
            };

            bw.write_bits(flag_value, 2);
        } else {
            // `Frame_Content_Size` was not provided
            bw.write_bits(0u8, 2);
        }

        bw.dump()[0]
    }
}

/// Identical to [`minify_val`], but it implements the following edge case:
///
/// > When FCS_Field_Size is 1, 4 or 8 bytes, the value is read directly. When FCS_Field_Size is 2,
/// > the offset of 256 is added.
///
/// https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md#frame_content_size
fn minify_val_fcs(val: u64) -> Vec<u8> {
    let new_size = find_min_size(val);
    let mut val = val;
    if new_size == 2 {
        val -= 256;
    }
    val.to_le_bytes()[0..new_size].to_vec()
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::FrameHeader;
    use crate::decoding::frame::{FrameDescriptor, read_frame_header};

    #[test]
    fn frame_header_descriptor_decode() {
        let header = FrameHeader {
            frame_content_size: Some(1),
            single_segment: true,
            content_checksum: false,
            dictionary_id: None,
            window_size: None,
        };
        let descriptor = header.descriptor();
        let decoded_descriptor = FrameDescriptor(descriptor);
        assert_eq!(decoded_descriptor.frame_content_size_bytes().unwrap(), 1);
        assert!(!decoded_descriptor.content_checksum_flag());
        assert_eq!(decoded_descriptor.dictionary_id_bytes().unwrap(), 0);
    }

    /// The FCS flag/field-size mapping across all four RFC 8878 classes,
    /// exercised at each class's boundary: the descriptor must carry the
    /// matching flag, the FCS field the matching width (flag 1 stores
    /// value - 256), and our decoder must read the size back. The 8-byte
    /// class (FCS >= 2^32) previously fell into a dead `3 => 8` match arm
    /// and panicked, taking down every > 4 GiB MT compression and any
    /// stream pledged at 2^32 or more.
    #[test]
    fn frame_header_fcs_field_sizes() {
        // (frame_content_size, FCS flag, serialized FCS field bytes)
        let cases: &[(u64, u8, &[u8])] = &[
            (255, 0, &[0xff]),
            (256, 1, &[0x00, 0x00]),
            (u16::MAX as u64, 1, &[0xff, 0xfe]),
            (1 << 16, 2, &[0x00, 0x00, 0x01, 0x00]),
            (u32::MAX as u64, 2, &[0xff, 0xff, 0xff, 0xff]),
            (1u64 << 32, 3, &[
                0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            ]),
        ];
        for &(fcs, flag, field) in cases {
            let header = FrameHeader {
                frame_content_size: Some(fcs),
                single_segment: true,
                content_checksum: false,
                dictionary_id: None,
                window_size: None,
            };
            let mut serialized = Vec::new();
            header.serialize(&mut serialized);
            // Single-segment: magic + descriptor + FCS field, nothing else.
            assert_eq!(&serialized[..4], &crate::common::MAGIC_NUM.to_le_bytes());
            assert_eq!(serialized[4], (flag << 6) | 0x20, "fcs {fcs}");
            assert_eq!(&serialized[5..], field, "fcs {fcs}");
            let parsed = read_frame_header(serialized.as_slice()).unwrap().0;
            assert_eq!(parsed.frame_content_size(), fcs, "fcs {fcs}");
        }
    }

    /// The Dictionary_ID field's three wire classes (RFC 8878 flag 1/2/3 =
    /// 1/2/4 bytes), exercised at each class's boundary: the descriptor
    /// carries the matching flag, the field the matching width, and our
    /// decoder reads the id back. Wider ids are unrepresentable — the u32
    /// field type replaced the old `_ => panic!()` arm a > u32::MAX id
    /// could reach on the publicly-constructible header.
    #[test]
    fn frame_header_dict_id_field_sizes() {
        // (dictionary_id, Dictionary_ID_flag, serialized field bytes)
        let cases: &[(u32, u8, &[u8])] = &[
            (1, 1, &[0x01]),
            (0xff, 1, &[0xff]),
            (0x100, 2, &[0x00, 0x01]),
            (0xffff, 2, &[0xff, 0xff]),
            (0x1_0000, 3, &[0x00, 0x00, 0x01, 0x00]),
            (u32::MAX, 3, &[0xff, 0xff, 0xff, 0xff]),
        ];
        for &(id, flag, field) in cases {
            let header = FrameHeader {
                frame_content_size: Some(1),
                single_segment: true,
                content_checksum: false,
                dictionary_id: Some(id),
                window_size: None,
            };
            let mut serialized = Vec::new();
            header.serialize(&mut serialized);
            // Single-segment: magic + descriptor + dict id + 1-byte FCS.
            assert_eq!(&serialized[..4], &crate::common::MAGIC_NUM.to_le_bytes());
            assert_eq!(serialized[4] & 0b11, flag, "id {id:#x}");
            assert_eq!(&serialized[5..5 + field.len()], field, "id {id:#x}");
            let parsed = read_frame_header(serialized.as_slice()).unwrap().0;
            assert_eq!(parsed.dictionary_id(), Some(id), "id {id:#x}");
        }
    }

    #[test]
    fn frame_header_decode() {
        let header = FrameHeader {
            frame_content_size: Some(1),
            single_segment: true,
            content_checksum: false,
            dictionary_id: None,
            window_size: None,
        };

        let mut serialized_header = Vec::new();
        header.serialize(&mut serialized_header);
        let parsed_header = read_frame_header(serialized_header.as_slice()).unwrap().0;
        assert!(parsed_header.dictionary_id().is_none());
        assert_eq!(parsed_header.frame_content_size(), 1);
    }

    #[test]
    #[should_panic(expected = "a frame content size must be provided")]
    fn catches_single_segment_no_fcs() {
        let header = FrameHeader {
            frame_content_size: None,
            single_segment: true,
            content_checksum: false,
            dictionary_id: None,
            window_size: Some(1),
        };

        let mut serialized_header = Vec::new();
        header.serialize(&mut serialized_header);
    }

    #[test]
    #[should_panic(expected = "a window size must be provided")]
    fn catches_single_segment_no_winsize() {
        let header = FrameHeader {
            frame_content_size: Some(7),
            single_segment: false,
            content_checksum: false,
            dictionary_id: None,
            window_size: None,
        };

        let mut serialized_header = Vec::new();
        header.serialize(&mut serialized_header);
    }

    /// A windowed frame cannot declare the 1-byte FCS class: flag 0 means
    /// the field is absent without the single-segment flag, so sizes below
    /// 256 used to serialize a stray byte that shifted every following
    /// field (both decoders rejected the frame). The declaration is dropped
    /// instead; the 2-byte class (256 and above) keeps declaring.
    #[test]
    fn windowed_small_fcs_is_dropped() {
        for fcs in [0u64, 1, 200, 255] {
            let header = FrameHeader {
                frame_content_size: Some(fcs),
                single_segment: false,
                content_checksum: false,
                dictionary_id: None,
                window_size: Some(1024),
            };
            let mut serialized = Vec::new();
            header.serialize(&mut serialized);
            let parsed = read_frame_header(serialized.as_slice()).unwrap().0;
            assert_eq!(parsed.frame_content_size(), 0, "fcs {fcs}");
            let desc = parsed.descriptor;
            assert_eq!(desc.frame_content_size_flag(), 0, "fcs {fcs}");
            assert!(!desc.single_segment_flag(), "fcs {fcs}");
            // magic + descriptor + window descriptor, nothing else
            assert_eq!(serialized.len(), 6, "fcs {fcs}");
        }
        for fcs in [256u64, 300, u16::MAX as u64] {
            let header = FrameHeader {
                frame_content_size: Some(fcs),
                single_segment: false,
                content_checksum: false,
                dictionary_id: None,
                window_size: Some(1024),
            };
            let mut serialized = Vec::new();
            header.serialize(&mut serialized);
            let parsed = read_frame_header(serialized.as_slice()).unwrap().0;
            assert_eq!(parsed.frame_content_size(), fcs, "fcs {fcs}");
            let desc = parsed.descriptor;
            assert!(
                desc.frame_content_size_flag() != 0 || desc.single_segment_flag(),
                "fcs {fcs} must declare"
            );
        }
    }
}
