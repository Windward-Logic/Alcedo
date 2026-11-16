//! MIME header decoding (RFC 2047 encoded-words).

use mail_parser::parsers::MessageStream;

/// Decode RFC 2047 encoded-words in a header value (e.g. Subject, From display name).
pub fn decode_mime_header(value: &str) -> String {
    if !value.contains("=?") {
        return value.to_string();
    }
    // `parse_unstructured` terminates on newline; append one for bare header values.
    let mut input = value.to_string();
    if !input.ends_with('\n') {
        input.push('\n');
    }
    let mut stream = MessageStream::new(input.as_bytes());
    match stream.parse_unstructured() {
        mail_parser::HeaderValue::Text(text) => text.into_owned(),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_single_encoded_word() {
        let input = "=?UTF-8?Q?Hello_World?=";
        assert_eq!(decode_mime_header(input), "Hello World");
    }

    #[test]
    fn decode_split_encoded_words() {
        let input = "=?UTF-8?Q?New_announcement:_BACK_TO?= =?UTF-8?Q?_SCHOOL_BASH_TONIGHT?=";
        assert_eq!(
            decode_mime_header(input),
            "New announcement: BACK TO SCHOOL BASH TONIGHT"
        );
    }

    #[test]
    fn decode_split_encoded_words_without_space() {
        let input = "=?UTF-8?Q?New_announcement:_BACK_TO?==?UTF-8?Q?_SCHOOL_BASH_TONIGHT?=";
        assert_eq!(
            decode_mime_header(input),
            "New announcement: BACK TO SCHOOL BASH TONIGHT"
        );
    }

    #[test]
    fn decode_emoji_quoted_printable() {
        let input = "=?UTF-8?Q?TONIGHT=F0=9F=8D=8E?=";
        assert_eq!(decode_mime_header(input), "TONIGHT🍎");
    }

    #[test]
    fn plain_text_passthrough() {
        assert_eq!(decode_mime_header("Regular subject"), "Regular subject");
    }

    #[test]
    fn decode_multibyte_encoded_word() {
        // Same charset tables as the body. `A1 X` is an em dash in Big5.
        let input = "=?big5?B?UmVxdWVzdCChWCBiYW5r?=";
        assert_eq!(decode_mime_header(input), "Request — bank");
    }
}
