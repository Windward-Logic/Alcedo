//! Line-ending normalization for signed MIME content.

/// Bare LF becomes CRLF; existing CRLF is kept. Nothing is appended when the
/// data does not end in a line break, so the bytes still match what the
/// sender signed.
pub fn crlf_line_endings(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 32);
    let mut prev = 0u8;
    for &byte in data {
        if byte == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        out.push(byte);
        prev = byte;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_line_endings_converts_only_bare_lf() {
        assert_eq!(crlf_line_endings(b"a\nb\r\nc"), b"a\r\nb\r\nc".to_vec());
        assert_eq!(crlf_line_endings(b"x\r\n"), b"x\r\n".to_vec());
    }
}
