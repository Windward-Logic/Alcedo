//! IMAP "modified UTF-7" mailbox names (RFC 3501 §5.1.3).
//!
//! Servers without `UTF8=ACCEPT` send and expect non-ASCII folder names as
//! `&<base64 of UTF-16BE>-`, with `,` in place of `/` and `&` written `&-`.
//! Names are stored and sent exactly as the server spells them; these helpers
//! convert only at the edges, for display and for names the user types.

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

fn is_direct(ch: char) -> bool {
    ('\u{20}'..='\u{7e}').contains(&ch) && ch != '&'
}

/// Spell `name` the way an IMAP server expects it.
pub fn encode_mailbox_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut run: Vec<u16> = Vec::new();
    for ch in name.chars() {
        if is_direct(ch) {
            flush_run(&mut out, &mut run);
            out.push(ch);
        } else if ch == '&' {
            flush_run(&mut out, &mut run);
            out.push_str("&-");
        } else {
            let mut buf = [0u16; 2];
            run.extend_from_slice(ch.encode_utf16(&mut buf));
        }
    }
    flush_run(&mut out, &mut run);
    out
}

fn flush_run(out: &mut String, run: &mut Vec<u16>) {
    if run.is_empty() {
        return;
    }
    let bytes: Vec<u8> = run.iter().flat_map(|unit| unit.to_be_bytes()).collect();
    out.push('&');
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        // 1 byte -> 2 chars, 2 bytes -> 3 chars, 3 bytes -> 4 chars (no padding).
        for i in 0..=chunk.len() {
            out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out.push('-');
    run.clear();
}

/// Human-readable form of a server-spelled mailbox name. A name that is not
/// valid modified UTF-7 is returned unchanged rather than mangled.
pub fn decode_mailbox_name(name: &str) -> String {
    if !name.contains('&') {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('-') else {
            return name.to_string();
        };
        let encoded = &after[..end];
        if encoded.is_empty() {
            out.push('&');
        } else if let Some(text) = decode_run(encoded) {
            out.push_str(&text);
        } else {
            return name.to_string();
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// One `&...-` run. `None` unless it is what an encoder could have produced:
/// whole UTF-16 units, fewer than 6 leftover bits and all of them zero, and no
/// printable ASCII (RFC 3501 forbids encoding those). The strictness keeps
/// names from non-compliant servers, such as a raw `Q&A-2024`, as they are.
fn decode_run(encoded: &str) -> Option<String> {
    let mut bits: u32 = 0;
    let mut nbits = 0;
    let mut bytes = Vec::new();
    for ch in encoded.chars() {
        let value = B64.iter().position(|b| *b as char == ch)? as u32;
        bits = (bits << 6) | value;
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            bytes.push((bits >> nbits) as u8);
            bits &= (1 << nbits) - 1;
        }
    }
    if nbits >= 6 || bits != 0 || bytes.is_empty() || bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    if units
        .iter()
        .any(|unit| *unit == 0 || (0x20..=0x7e).contains(unit))
    {
        return None;
    }
    String::from_utf16(&units).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_3501_example_round_trips() {
        let name = "~peter/mail/台北/日本語";
        let wire = "~peter/mail/&U,BTFw-/&ZeVnLIqe-";
        // The RFC's example spells "/" as part of the hierarchy, outside the runs.
        assert_eq!(decode_mailbox_name(wire), name);
        assert_eq!(encode_mailbox_name(name), wire);
    }

    #[test]
    fn ampersand_and_accents_round_trip() {
        for name in ["R&D", "Entwürfe", "Boîte d'envoi", "a&b é c", "😀 Fun", ""] {
            assert_eq!(decode_mailbox_name(&encode_mailbox_name(name)), name);
        }
        assert_eq!(encode_mailbox_name("R&D"), "R&-D");
        assert_eq!(encode_mailbox_name("Entwürfe"), "Entw&APw-rfe");
        assert_eq!(decode_mailbox_name("Entw&APw-rfe"), "Entwürfe");
    }

    #[test]
    fn plain_ascii_is_untouched_and_malformed_input_is_kept() {
        assert_eq!(encode_mailbox_name("INBOX/Work"), "INBOX/Work");
        assert_eq!(decode_mailbox_name("INBOX/Work"), "INBOX/Work");
        assert_eq!(decode_mailbox_name("broken&AP"), "broken&AP");
        assert_eq!(decode_mailbox_name("bad&!!!-run"), "bad&!!!-run");
    }

    #[test]
    fn runs_an_encoder_could_not_produce_are_kept_verbatim() {
        // A raw "&A-" from a server that does not encode names: 6 leftover bits.
        assert_eq!(decode_mailbox_name("Q&A-2024"), "Q&A-2024");
        // Decodes to U+0000.
        assert_eq!(decode_mailbox_name("x&AAA-y"), "x&AAA-y");
        // Decodes to "a", which must be written directly.
        assert_eq!(decode_mailbox_name("&AGE-"), "&AGE-");
        // Non-zero padding bits.
        assert_eq!(decode_mailbox_name("Entw&APx-rfe"), "Entw&APx-rfe");
    }
}
