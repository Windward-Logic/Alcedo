//! Attachment limits, filename helpers, and configurable type lists.

use std::path::Path;

pub const MAX_COMPOSE_INLINES: usize = 10;

/// Used when the server has not advertised an SMTP `SIZE` limit.
pub const DEFAULT_COMPOSE_SIZE_LIMIT: u64 = 25 * 1024 * 1024;

/// Defaults for [`AttachmentLists::risky_extensions`] / GSettings
/// `risky-attachment-extensions`.
pub const DEFAULT_RISKY_ATTACHMENT_EXTENSIONS: &[&str] = &[
    "exe", "com", "bat", "cmd", "scr", "msi", "html", "htm", "js", "svg", "wasm", "dll", "so",
];

/// Defaults for [`AttachmentLists::risky_mime_types`] / GSettings
/// `risky-attachment-mime-types`.
pub const DEFAULT_RISKY_ATTACHMENT_MIME_TYPES: &[&str] = &[
    "text/html",
    "application/javascript",
    "text/javascript",
    "image/svg+xml",
    "application/wasm",
    "application/x-msdownload",
    "application/vnd.microsoft.portable-executable",
    "application/vnd.ms-msi",
    "application/x-msi",
    "application/x-sharedlib",
    "application/x-executable",
    "application/x-dosexec",
];

/// Defaults for [`AttachmentLists::typical_inline_image_extensions`] / GSettings
/// `typical-inline-image-extensions`.
pub const DEFAULT_TYPICAL_INLINE_IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp"];

/// Runtime copies of the attachment type lists (from GSettings in the UI,
/// or [`AttachmentLists::defaults`] in store/sync).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentLists {
    pub risky_extensions: Vec<String>,
    pub risky_mime_types: Vec<String>,
    pub typical_inline_image_extensions: Vec<String>,
}

impl AttachmentLists {
    pub fn defaults() -> Self {
        Self {
            risky_extensions: owned_tokens(DEFAULT_RISKY_ATTACHMENT_EXTENSIONS),
            risky_mime_types: owned_tokens(DEFAULT_RISKY_ATTACHMENT_MIME_TYPES),
            typical_inline_image_extensions: owned_tokens(DEFAULT_TYPICAL_INLINE_IMAGE_EXTENSIONS),
        }
    }

    pub fn mime_is_risky(&self, mime: &str) -> bool {
        let mime = strip_mime_params(mime);
        self.risky_mime_types
            .iter()
            .any(|listed| mime.eq_ignore_ascii_case(listed))
    }

    pub fn extension_is_risky(&self, filename: &str) -> bool {
        extension_of(filename).is_some_and(|ext| {
            self.risky_extensions
                .iter()
                .any(|listed| ext.eq_ignore_ascii_case(listed))
        })
    }

    /// True when the name, declared MIME, or sniffed MIME is on the risky lists.
    pub fn attachment_is_risky(
        &self,
        filename: &str,
        mime: &str,
        sniffed_mime: Option<&str>,
    ) -> bool {
        self.extension_is_risky(filename)
            || self.mime_is_risky(mime)
            || sniffed_mime.is_some_and(|sniffed| self.mime_is_risky(sniffed))
    }

    pub fn filename_is_typical_inline_image(&self, filename: &str) -> bool {
        filename_is_safe(filename)
            && extension_of(filename).is_some_and(|ext| self.extension_is_typical_inline(&ext))
    }

    pub fn mime_is_typical_inline_image(&self, mime: &str) -> bool {
        let mime = strip_mime_params(mime);
        let Some(subtype) = mime
            .strip_prefix("image/")
            .map(|rest| rest.to_ascii_lowercase())
        else {
            return false;
        };
        self.extension_is_typical_inline(&subtype)
            || (subtype == "jpeg" && self.extension_is_typical_inline("jpg"))
            || (subtype == "jpg" && self.extension_is_typical_inline("jpeg"))
    }

    pub fn is_typical_inline_image(&self, filename: &str, mime: &str) -> bool {
        self.filename_is_typical_inline_image(filename) || self.mime_is_typical_inline_image(mime)
    }

    fn extension_is_typical_inline(&self, ext: &str) -> bool {
        self.typical_inline_image_extensions
            .iter()
            .any(|listed| ext.eq_ignore_ascii_case(listed))
    }
}

fn owned_tokens(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn strip_mime_params(mime: &str) -> &str {
    mime.split(';').next().unwrap_or(mime).trim()
}

/// Split a preferences field into lowercase type tokens (extensions or MIME).
pub fn parse_type_list(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.split(|ch: char| ch == ',' || ch.is_ascii_whitespace()) {
        if let Some(token) = normalize_type_token(raw)
            && !out.iter().any(|existing| existing == &token)
        {
            out.push(token);
        }
    }
    out
}

/// Join type tokens for an EntryRow.
pub fn format_type_list(items: &[String]) -> String {
    items.join(", ")
}

pub fn normalize_type_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches(|ch| ch == '.' || ch == '"');
    if trimmed.is_empty() {
        return None;
    }
    let token = trimmed.to_ascii_lowercase();
    if token.contains('/') {
        Some(token)
    } else {
        token
            .rsplit('.')
            .next()
            .map(str::to_string)
            .filter(|ext| !ext.is_empty())
    }
}

pub fn tokens_from_strv(entries: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<String> {
    let mut out = Vec::new();
    for entry in entries {
        if let Some(token) = normalize_type_token(entry.as_ref())
            && !out.iter().any(|existing| existing == &token)
        {
            out.push(token);
        }
    }
    out
}

pub fn extension_of(filename: &str) -> Option<String> {
    Path::new(filename)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| !s.is_empty())
}

/// Downloadable file vs CID inline image.
///
/// Outlook often puts a Content-ID on real attachments (PDFs, etc.). Those
/// should still show a paperclip and a reader chip. Image parts with a CID are
/// treated as inline (`multipart/related`) and do not — unless they are on the
/// risky lists, in which case they stay a file the user must enable.
pub fn part_is_file_attachment(mime: &str, cid: Option<&str>) -> bool {
    let cid = cid.map(str::trim).filter(|value| !value.is_empty());
    match cid {
        None => true,
        Some(_) => !mime.trim().to_ascii_lowercase().starts_with("image/"),
    }
}

/// Chip / paperclip predicate: real files, plus risky CID images that must
/// not render until the user enables them.
pub fn attachment_is_downloadable(
    filename: &str,
    mime: &str,
    cid: Option<&str>,
    lists: &AttachmentLists,
) -> bool {
    part_is_file_attachment(mime, cid) || lists.attachment_is_risky(filename, mime, None)
}

/// Reject names that would break MIME headers or look like path traversal.
pub fn filename_is_safe(filename: &str) -> bool {
    !filename.is_empty()
        && !filename.contains(['/', '\\', '"'])
        && filename.chars().all(|ch| !ch.is_control())
}

/// SMTP size cap: the server's `SIZE` when it is known and non-zero, otherwise
/// about 25 MB.
pub fn compose_size_limit(smtp_size: Option<u64>) -> u64 {
    match smtp_size {
        Some(limit) if limit > 0 => limit,
        _ => DEFAULT_COMPOSE_SIZE_LIMIT,
    }
}

fn format_megabytes(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 10.0 {
        format!("{} MB", mb.round() as u64)
    } else {
        format!("{mb:.1} MB")
    }
}

/// Warning when outgoing attachments exceed [`compose_size_limit`]. `None`
/// means the total is within the limit. The composer still attaches the
/// file; the server makes the final call.
pub fn attachment_size_warning(total_bytes: u64, smtp_size: Option<u64>) -> Option<String> {
    let limit = compose_size_limit(smtp_size);
    if total_bytes <= limit {
        return None;
    }
    let total = format_megabytes(total_bytes);
    let limit = format_megabytes(limit);
    Some(match smtp_size.filter(|size| *size > 0) {
        Some(_) => format!("Attachments total about {total}, over this server's {limit} limit"),
        None => format!(
            "Attachments total about {total}. Many mail servers reject messages over {limit}"
        ),
    })
}

/// Content-ID token used in `Content-ID` / `cid:` (no angle brackets).
pub fn content_id_is_safe(content_id: &str) -> bool {
    let Some((local, domain)) = content_id.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && content_id.bytes().filter(|&b| b == b'@').count() == 1
        && content_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '@'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outlook_pdf_with_cid_is_file_attachment() {
        assert!(part_is_file_attachment(
            "application/pdf",
            Some("1DC871341A4EFD4890482BF088FCFABC@namprd19.prod.outlook.com")
        ));
    }

    #[test]
    fn cid_image_is_inline_not_file() {
        assert!(!part_is_file_attachment(
            "image/png",
            Some("logo@example.com")
        ));
        assert!(!part_is_file_attachment(
            "IMAGE/JPEG",
            Some(" pic@example.com ")
        ));
    }

    #[test]
    fn missing_or_empty_cid_is_file_attachment() {
        assert!(part_is_file_attachment("image/png", None));
        assert!(part_is_file_attachment("image/png", Some("")));
        assert!(part_is_file_attachment("image/png", Some("  ")));
        assert!(part_is_file_attachment("text/plain", None));
    }

    #[test]
    fn gmail_sql_with_cid_is_file_attachment() {
        assert!(part_is_file_attachment(
            "application/sql",
            Some("f_mu2rugzc0")
        ));
    }

    #[test]
    fn risky_cid_svg_is_downloadable() {
        let lists = AttachmentLists::defaults();
        assert!(attachment_is_downloadable(
            "logo.svg",
            "image/svg+xml",
            Some("logo@example.com"),
            &lists
        ));
        assert!(!attachment_is_downloadable(
            "logo.png",
            "image/png",
            Some("logo@example.com"),
            &lists
        ));
    }

    #[test]
    fn exe_and_html_are_risky_by_default() {
        let lists = AttachmentLists::defaults();
        assert!(lists.attachment_is_risky("setup.exe", "application/octet-stream", None));
        assert!(lists.attachment_is_risky("photo.png", "image/png", Some("application/x-dosexec")));
        assert!(lists.attachment_is_risky("note.html", "text/html", None));
        assert!(!lists.attachment_is_risky("report.pdf", "application/pdf", None));
    }

    #[test]
    fn typical_inline_images_match_jpeg_alias() {
        let lists = AttachmentLists::defaults();
        assert!(lists.filename_is_typical_inline_image("pic.jpg"));
        assert!(lists.filename_is_typical_inline_image("pic.JPEG"));
        assert!(lists.mime_is_typical_inline_image("image/jpeg"));
        assert!(lists.mime_is_typical_inline_image("image/png; charset=binary"));
        assert!(!lists.filename_is_typical_inline_image("pic.bmp"));
        assert!(!lists.mime_is_typical_inline_image("image/bmp"));
        assert!(!lists.filename_is_typical_inline_image("pic.svg"));
    }

    #[test]
    fn parse_type_list_splits_and_normalizes() {
        assert_eq!(
            parse_type_list("EXE, .Html, image/svg+xml js"),
            vec![
                "exe".to_string(),
                "html".to_string(),
                "image/svg+xml".to_string(),
                "js".to_string()
            ]
        );
        assert!(parse_type_list("  ,  ").is_empty());
        assert_eq!(format_type_list(&parse_type_list("png, gif")), "png, gif");
    }

    #[test]
    fn empty_lists_mean_nothing_is_risky_or_typical() {
        let lists = AttachmentLists {
            risky_extensions: Vec::new(),
            risky_mime_types: Vec::new(),
            typical_inline_image_extensions: Vec::new(),
        };
        assert!(!lists.attachment_is_risky("setup.exe", "application/x-dosexec", None));
        assert!(!lists.filename_is_typical_inline_image("pic.png"));
    }

    #[test]
    fn size_limit_uses_smtp_size_or_25_mb() {
        assert_eq!(compose_size_limit(None), DEFAULT_COMPOSE_SIZE_LIMIT);
        assert_eq!(compose_size_limit(Some(0)), DEFAULT_COMPOSE_SIZE_LIMIT);
        assert_eq!(compose_size_limit(Some(10 * 1024 * 1024)), 10 * 1024 * 1024);
    }

    #[test]
    fn size_warning_is_quiet_under_the_limit() {
        assert!(attachment_size_warning(20 * 1024 * 1024, None).is_none());
        assert!(attachment_size_warning(8 * 1024 * 1024, Some(10 * 1024 * 1024)).is_none());
    }

    #[test]
    fn size_warning_names_the_fallback_and_the_server_limit() {
        let fallback = attachment_size_warning(30 * 1024 * 1024, None).expect("over 25 MB");
        assert!(fallback.contains("30 MB"), "{fallback}");
        assert!(fallback.contains("25 MB"), "{fallback}");

        let server = attachment_size_warning(12 * 1024 * 1024, Some(10 * 1024 * 1024))
            .expect("over server size");
        assert!(server.contains("12 MB"), "{server}");
        assert!(server.contains("10 MB"), "{server}");
    }
}
