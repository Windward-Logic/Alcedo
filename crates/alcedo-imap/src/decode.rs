//! Content-Transfer-Encoding decoding and completeness checks for MIME parts
//! fetched by IMAP section (`BODY.PEEK[n.m]`).
//!
//! Strict about completeness, lenient about noise: RFC 2045 §6.8 says to
//! ignore line breaks and stray characters inside base64, but a part that
//! ends mid-quantum, or arrives far shorter than BODYSTRUCTURE promised, is
//! an error, never "the file".

use std::fmt;

/// Why a fetched MIME part could not be turned into the exact file bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PartIntegrityError {
    /// Base64 ended with a dangling sextet: the part was cut off.
    TruncatedBase64,
    /// Fewer encoded octets arrived than BODYSTRUCTURE announced.
    ShortSection { expected: u64, received: u64 },
}

impl fmt::Display for PartIntegrityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedBase64 => write!(f, "base64 data ends mid-quantum (truncated)"),
            Self::ShortSection { expected, received } => write!(
                f,
                "received {received} of {expected} encoded octets announced by BODYSTRUCTURE"
            ),
        }
    }
}

impl std::error::Error for PartIntegrityError {}

/// Accept small differences between BODYSTRUCTURE octets and the literal
/// (servers disagree on CRLF accounting and some, notably Exchange, only
/// estimate sizes). A cut at a fetch/window cap loses far more than this.
const SECTION_LENGTH_TOLERANCE_PERCENT: u64 = 10;

/// Error when `received` is clearly shorter than BODYSTRUCTURE's `expected`.
/// `expected == 0` means the server did not report a size.
pub fn check_section_length(expected: u64, received: u64) -> Result<(), PartIntegrityError> {
    if expected == 0 {
        return Ok(());
    }
    let floor = expected - expected * SECTION_LENGTH_TOLERANCE_PERCENT / 100;
    if received < floor {
        return Err(PartIntegrityError::ShortSection { expected, received });
    }
    Ok(())
}

fn base64_value(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decode a base64 body (RFC 2045 §6.8).
///
/// Whitespace and other non-alphabet bytes are skipped. `=` padding closes
/// the current quantum; data after it is decoded as a new run (some mailers
/// concatenate padded chunks). Missing trailing padding is accepted. A final
/// quantum holding a single sextet cannot encode a byte, so it can only
/// come from truncation and is an error.
pub fn decode_base64_body(bytes: &[u8]) -> Result<Vec<u8>, PartIntegrityError> {
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 3);
    let mut acc: u32 = 0;
    let mut count = 0u8;
    let flush = |out: &mut Vec<u8>, acc: u32, count: u8| -> Result<(), PartIntegrityError> {
        match count {
            0 => Ok(()),
            1 => Err(PartIntegrityError::TruncatedBase64),
            2 => {
                out.push((acc >> 4) as u8);
                Ok(())
            }
            3 => {
                out.push((acc >> 10) as u8);
                out.push((acc >> 2) as u8);
                Ok(())
            }
            _ => unreachable!("quantum is flushed at four sextets"),
        }
    };
    for &byte in bytes {
        if byte == b'=' {
            flush(&mut out, acc, count)?;
            acc = 0;
            count = 0;
            continue;
        }
        let Some(value) = base64_value(byte) else {
            continue;
        };
        acc = (acc << 6) | value;
        count += 1;
        if count == 4 {
            out.push((acc >> 16) as u8);
            out.push((acc >> 8) as u8);
            out.push(acc as u8);
            acc = 0;
            count = 0;
        }
    }
    flush(&mut out, acc, count)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_padded_and_wrapped_base64() {
        assert_eq!(decode_base64_body(b"SGVs\r\nbG8=").unwrap(), b"Hello");
        assert_eq!(decode_base64_body(b"SGVsbG8h").unwrap(), b"Hello!");
        assert_eq!(decode_base64_body(b"SGk=").unwrap(), b"Hi");
    }

    #[test]
    fn accepts_missing_padding() {
        assert_eq!(decode_base64_body(b"SGVsbG8").unwrap(), b"Hello");
        assert_eq!(decode_base64_body(b"SGk").unwrap(), b"Hi");
    }

    #[test]
    fn ignores_noise_bytes() {
        assert_eq!(decode_base64_body(b" SG\tVs !bG8=\r\n").unwrap(), b"Hello");
    }

    #[test]
    fn decodes_concatenated_padded_runs() {
        assert_eq!(decode_base64_body(b"SGk=SGk=").unwrap(), b"HiHi");
    }

    #[test]
    fn dangling_sextet_is_truncation() {
        assert_eq!(
            decode_base64_body(b"SGVsbG8hS"),
            Err(PartIntegrityError::TruncatedBase64)
        );
    }

    #[test]
    fn empty_input_decodes_empty() {
        assert_eq!(decode_base64_body(b"").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn round_trips_all_byte_values() {
        let payload: Vec<u8> = (0..=255u8).cycle().take(4099).collect();
        let encoded = wrap76(&b64(&payload));
        assert_eq!(decode_base64_body(encoded.as_bytes()).unwrap(), payload);
    }

    #[test]
    fn section_length_check() {
        assert!(check_section_length(0, 5).is_ok());
        assert!(check_section_length(1000, 1000).is_ok());
        assert!(check_section_length(1000, 950).is_ok());
        assert!(check_section_length(1000, 1100).is_ok());
        assert_eq!(
            check_section_length(571_000, 262_144),
            Err(PartIntegrityError::ShortSection {
                expected: 571_000,
                received: 262_144
            })
        );
    }

    fn b64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk.len();
            let v = (u32::from(chunk[0]) << 16)
                | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
                | u32::from(*chunk.get(2).unwrap_or(&0));
            for i in 0..4 {
                if i <= n {
                    out.push(ALPHABET[((v >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    fn wrap76(text: &str) -> String {
        text.as_bytes()
            .chunks(76)
            .map(|line| std::str::from_utf8(line).unwrap())
            .collect::<Vec<_>>()
            .join("\r\n")
    }
}
