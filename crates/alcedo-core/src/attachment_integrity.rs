//! Cheap checks that stored attachment bytes are the decoded file.
//!
//! The fetch layer is the primary guard against truncated or undecoded parts.
//! These checks are the last line before bytes reach the cache or the user's
//! disk, so they must be cheap (they only look at the first few bytes) and
//! must not reject legitimate text payloads.

use mail_parser::decoders::base64::base64_decode;

/// Saved or parsed bytes are still the base64 text of a binary part (PNG
/// `iVBORw0KGgo…`, JPEG `/9j/…`, PDF `JVBERi…`, DOCX/ZIP `UEsDB…`).
///
/// mail-parser keeps a part's raw transfer text when it cannot decode it (for
/// example a part cut off before its closing boundary), so the "file" is the
/// encoded text. Only binary MIME types are checked; text types may
/// legitimately contain base64-looking content.
pub fn attachment_bytes_are_undecoded_base64(mime: &str, bytes: &[u8]) -> bool {
    if bytes.len() < 16 || mime_allows_ascii_payload(mime) {
        return false;
    }
    let compact: Vec<u8> = bytes
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .take(64)
        .collect();
    let take = compact.len() - (compact.len() % 4);
    if take < 16 {
        return false;
    }
    let sample = &compact[..take];
    if !sample.iter().copied().all(base64_alphabet_byte) {
        return false;
    }
    let Some(decoded) = base64_decode(sample) else {
        return false;
    };
    binary_file_magic(&decoded)
}

fn mime_allows_ascii_payload(mime: &str) -> bool {
    let mime = mime.trim().to_ascii_lowercase();
    mime.starts_with("text/")
        || mime.starts_with("message/")
        || mime == "application/json"
        || mime == "application/xml"
        || mime == "application/javascript"
        || mime == "application/sql"
}

fn base64_alphabet_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')
}

/// Leading bytes of common binary attachment formats.
pub fn binary_file_magic(bytes: &[u8]) -> bool {
    // SVG/XML payloads may lead with whitespace before the tag.
    let text = bytes
        .iter()
        .copied()
        .take_while(|byte| byte.is_ascii_whitespace())
        .count();
    let trimmed = &bytes[text.min(bytes.len())..];
    bytes.starts_with(b"\xFF\xD8\xFF")
        || bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || bytes.starts_with(b"%PDF")
        || bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(&[0x1f, 0x8b])
        // BMP `BM`, TIFF `II*\0` / `MM\0*`, OLE `D0 CF 11 E0 …`.
        || bytes.starts_with(b"BM")
        || bytes.starts_with(&[0x49, 0x49, 0x2A, 0x00])
        || bytes.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
        || bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
        // WebP `RIFF....WEBP`, MP4/MOV/HEIC size word plus `ftyp`.
        || (bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP")
        || (bytes.len() >= 8 && &bytes[4..8] == b"ftyp")
        // SVG/XML text: only reachable here when the stored bytes were
        // base64 text (decoded SVG fails the alphabet check above).
        || trimmed.starts_with(b"<svg")
        || trimmed.starts_with(b"<?xml")
}

/// MIME type of `bytes` when they are a raster image, judged by content.
///
/// The sender chooses an attachment's declared type, so anything that
/// decides how bytes are rendered must look at the bytes instead. Formats
/// that can carry markup or script (SVG, XML, HTML) are never matched.
pub fn raster_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\xFF\xD8\xFF") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"BM") {
        Some("image/bmp")
    } else if bytes.starts_with(&[0x49, 0x49, 0x2A, 0x00])
        || bytes.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
    {
        Some("image/tiff")
    } else if bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        Some("image/x-icon")
    } else if bytes.starts_with(&[0xFF, 0x0A])
        || bytes.starts_with(&[
            0, 0, 0, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A,
        ])
    {
        Some("image/jxl")
    } else if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        // ISO base media: the major brand tells stills from video.
        match &bytes[8..12] {
            b"avif" | b"avis" => Some("image/avif"),
            b"heic" | b"heix" | b"mif1" | b"msf1" => Some("image/heic"),
            _ => None,
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raster_images_are_recognized_by_content() {
        assert_eq!(
            raster_image_mime(b"\xFF\xD8\xFF\xE0rest"),
            Some("image/jpeg")
        );
        assert_eq!(
            raster_image_mime(b"\x89PNG\r\n\x1a\nrest"),
            Some("image/png")
        );
        assert_eq!(raster_image_mime(b"GIF89a..."), Some("image/gif"));
        assert_eq!(
            raster_image_mime(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(
            raster_image_mime(b"\x00\x00\x00\x1cftypavif\x00\x00"),
            Some("image/avif")
        );
    }

    #[test]
    fn markup_and_other_documents_are_never_images() {
        assert_eq!(raster_image_mime(b"<html><body>hi</body></html>"), None);
        assert_eq!(
            raster_image_mime(
                b"<?xml version=\"1.0\"?><html xmlns=\"http://www.w3.org/1999/xhtml\"/>"
            ),
            None
        );
        assert_eq!(
            raster_image_mime(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
            None
        );
        assert_eq!(raster_image_mime(b"%PDF-1.7"), None);
        assert_eq!(raster_image_mime(b"\x00\x00\x00\x18ftypisom\x00\x00"), None);
        assert_eq!(raster_image_mime(b""), None);
    }

    #[test]
    fn docx_base64_text_is_flagged() {
        // The exact prefix of the broken file Carter saved.
        let bytes = b"UEsDBBQACAgIAAAAAAAAAAAAAAAAAAAAAAAAFAAAAHdvcmQvZG9jdW1lbnQueG1s";
        assert!(attachment_bytes_are_undecoded_base64(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            bytes
        ));
    }

    #[test]
    fn decoded_zip_is_not_flagged() {
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.extend_from_slice(&[0u8; 60]);
        assert!(!attachment_bytes_are_undecoded_base64(
            "application/zip",
            &bytes
        ));
    }

    #[test]
    fn text_types_are_never_flagged() {
        let bytes = b"UEsDBBQACAgIAAAAAAAAAAAAAAAAAAAAAAAA";
        assert!(!attachment_bytes_are_undecoded_base64("text/plain", bytes));
    }
}
