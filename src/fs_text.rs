//! Reading text files in whatever encoding they were saved in.
//!
//! Ported from Markpad's `src-tauri/src/fs_safety.rs` (`decode_text` and its
//! tests). The write half (`encode_text`, `atomic_write`) is not needed by a
//! viewer and was left out.
//!
//! Copyright (c) 2023, Alec Ames. Licensed under the BSD 3-Clause License;
//! see THIRD_PARTY_LICENSES.md.

use std::fs;
use std::path::Path;

/// The encoding label of a plain UTF-8 file.
pub const UTF8_LABEL: &str = "UTF-8";
/// UTF-8 with a leading byte order mark. `encoding_rs` reports both as
/// "UTF-8"; the distinction is kept so the status line can show it.
pub const UTF8_BOM_LABEL: &str = "UTF-8-BOM";

/// Text decoded from a file, with the fidelity of the decode.
#[derive(Debug, Clone)]
pub struct DecodedText {
    pub content: String,
    /// `true` when some bytes could not be decoded and became U+FFFD.
    pub lossy: bool,
    /// The encoding the text was decoded with (a WHATWG label or one of the
    /// constants above). Always `UTF-8` when `lossy`.
    pub encoding: String,
}

/// Decode a document, detecting its encoding the way a browser does.
///
/// 1. A byte order mark decides on its own (UTF-8, UTF-16LE, UTF-16BE).
/// 2. Valid UTF-8 is UTF-8; no heuristic gets a say over it.
/// 3. Otherwise guess with `chardetng`, the detector Firefox ships, and decode
///    with what it says. ISO-2022-JP is allowed.
pub fn decode_text(bytes: &[u8]) -> DecodedText {
    let decoded = |encoding: &'static encoding_rs::Encoding, label: &str, body: &[u8]| {
        let (content, had_errors) = encoding.decode_without_bom_handling(body);
        DecodedText {
            content: content.into_owned(),
            lossy: had_errors,
            encoding: if had_errors { UTF8_LABEL } else { label }.to_owned(),
        }
    };

    if let Some((encoding, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        let label = if encoding == encoding_rs::UTF_8 {
            UTF8_BOM_LABEL
        } else {
            encoding.name()
        };
        return decoded(encoding, label, &bytes[bom_len..]);
    }

    if let Ok(content) = std::str::from_utf8(bytes) {
        return DecodedText {
            content: content.to_owned(),
            lossy: false,
            encoding: UTF8_LABEL.to_owned(),
        };
    }

    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    detector.feed(bytes, true);
    let encoding = detector.guess(None, chardetng::Utf8Detection::Deny);
    let text = decoded(encoding, encoding.name(), bytes);
    // `chardetng` never answers GB18030, and the GBK decoder reads GB18030's
    // four-byte sequences although GBK cannot represent them. Such a file is
    // GB18030 and is labelled so.
    if encoding == encoding_rs::GBK && !text.lossy && encoding.encode(&text.content).2 {
        return decoded(encoding_rs::GB18030, encoding_rs::GB18030.name(), bytes);
    }
    text
}

/// Read and decode a file.
pub fn read_to_string_lossy(path: &Path) -> std::io::Result<DecodedText> {
    Ok(decode_text(&fs::read(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Simplified Chinese line long enough for the detector to have
    /// something to work with.
    const CHINESE_SAMPLE: &str = "# 中文标题\n\n这是一个用国标编码保存的文档，里面全是汉字。\n";

    /// One realistic document per legacy encoding a reader is likely to meet.
    const LEGACY_SAMPLES: [(&encoding_rs::Encoding, &str); 5] = [
        (encoding_rs::GBK, CHINESE_SAMPLE),
        (
            encoding_rs::BIG5,
            "# 中文標題\n\n這是一個用大五碼儲存的文件。\n",
        ),
        (
            encoding_rs::SHIFT_JIS,
            "# 日本語の見出し\n\nこれはシフトJISで保存された文書です。\n",
        ),
        (
            encoding_rs::WINDOWS_1252,
            "# Café résumé\n\nNaïve façade — priced at 50\u{A0}£.\n",
        ),
        (
            encoding_rs::WINDOWS_1251,
            "# Заголовок документа\n\nЭто документ, сохранённый в кодировке Windows-1251, с обычным русским текстом.\n",
        ),
    ];

    fn legacy_bytes(encoding: &'static encoding_rs::Encoding, text: &str) -> Vec<u8> {
        let (bytes, _, unmappable) = encoding.encode(text);
        assert!(
            !unmappable,
            "fixture must be representable in {}",
            encoding.name()
        );
        assert!(
            std::str::from_utf8(&bytes).is_err(),
            "fixture must not also be valid UTF-8, or it proves nothing",
        );
        bytes.into_owned()
    }

    #[test]
    fn a_legacy_encoded_document_decodes_instead_of_becoming_replacement_characters() {
        for (encoding, text) in LEGACY_SAMPLES {
            let decoded = decode_text(&legacy_bytes(encoding, text));
            assert!(
                !decoded.lossy,
                "{} was reported as undecodable",
                encoding.name()
            );
            assert_eq!(
                decoded.content,
                text,
                "{} decoded to the wrong text",
                encoding.name()
            );
        }
    }

    #[test]
    fn a_gb18030_four_byte_sequence_is_labelled_gb18030() {
        let mut original = legacy_bytes(encoding_rs::GB18030, CHINESE_SAMPLE);
        original.extend_from_slice(&[0x95, 0x32, 0x82, 0x36]);

        let decoded = decode_text(&original);
        assert!(!decoded.lossy);
        assert!(decoded.content.ends_with('\u{20000}'));
        assert_eq!(decoded.encoding, "gb18030");
    }

    #[test]
    fn valid_utf8_is_never_handed_to_the_detector() {
        let decoded = decode_text("中文".as_bytes());
        assert!(!decoded.lossy);
        assert_eq!(decoded.content, "中文");
        assert_eq!(decoded.encoding, UTF8_LABEL);
    }

    #[test]
    fn a_byte_order_mark_is_stripped_and_reported() {
        let text = "# Title\n\nBody.\n";
        for (label, bom) in [
            (UTF8_BOM_LABEL, vec![0xEF, 0xBB, 0xBF]),
            ("UTF-16LE", vec![0xFF, 0xFE]),
            ("UTF-16BE", vec![0xFE, 0xFF]),
        ] {
            let mut original = bom;
            match label {
                "UTF-16LE" => original.extend(text.encode_utf16().flat_map(u16::to_le_bytes)),
                "UTF-16BE" => original.extend(text.encode_utf16().flat_map(u16::to_be_bytes)),
                _ => original.extend_from_slice(text.as_bytes()),
            }
            let decoded = decode_text(&original);
            assert!(!decoded.lossy, "{label} decode was lossy");
            assert_eq!(decoded.encoding, label);
            assert_eq!(decoded.content, text, "{label} left the BOM in the buffer");
        }
    }

    #[test]
    fn bytes_nothing_decodes_are_still_reported_as_lossy() {
        let truncated = [0xFF, 0xFE, b'A', 0x00, b'B'];
        let decoded = decode_text(&truncated);
        assert!(decoded.lossy, "got {:?}", decoded.content);
        assert!(decoded.content.contains('\u{FFFD}'));
        assert_eq!(decoded.encoding, UTF8_LABEL);
    }

    #[test]
    fn read_to_string_lossy_reads_a_legacy_file_from_disk() {
        let path = std::env::temp_dir().join(format!(
            "qmarkdown-gbk-{}-{}.md",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, legacy_bytes(encoding_rs::GBK, CHINESE_SAMPLE)).unwrap();
        let decoded = read_to_string_lossy(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(!decoded.lossy);
        assert_eq!(decoded.content, CHINESE_SAMPLE);
    }
}
