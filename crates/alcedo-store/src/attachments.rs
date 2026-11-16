//! Attachment storage (P2).

use std::path::{Path, PathBuf};

use alcedo_core::private_fs::{
    create_private_dir_all, restrict_file_permissions, write_private_file_atomic,
};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use tracing::warn;

use alcedo_core::{
    AccountId, AttachmentLists, MessageId, attachment_is_downloadable, extension_of,
};

use crate::messages::refresh_message_list_denorm;
use crate::{AttachmentMeta, StoreError, StoreResult};

fn extension_expected_mime(ext: &str) -> Option<&'static str> {
    match ext {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "tif" | "tiff" => Some("image/tiff"),
        "heic" | "heif" => Some("image/heif"),
        "pdf" => Some("application/pdf"),
        "zip" => Some("application/zip"),
        "7z" => Some("application/x-7z-compressed"),
        "rar" => Some("application/vnd.rar"),
        "tar" => Some("application/x-tar"),
        "gz" => Some("application/gzip"),
        "txt" => Some("text/plain"),
        "csv" => Some("text/csv"),
        "ics" => Some("text/calendar"),
        "vcf" => Some("text/vcard"),
        "rtf" => Some("application/rtf"),
        "doc" => Some("application/msword"),
        "docx" => Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "xls" => Some("application/vnd.ms-excel"),
        "xlsx" => Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        "ppt" => Some("application/vnd.ms-powerpoint"),
        "pptx" => Some("application/vnd.openxmlformats-officedocument.presentationml.presentation"),
        "odt" => Some("application/vnd.oasis.opendocument.text"),
        "ods" => Some("application/vnd.oasis.opendocument.spreadsheet"),
        "odp" => Some("application/vnd.oasis.opendocument.presentation"),
        "mp3" => Some("audio/mpeg"),
        "mp4" => Some("video/mp4"),
        "m4a" => Some("audio/m4a"),
        "wav" => Some("audio/x-wav"),
        "mov" => Some("video/quicktime"),
        "avi" => Some("video/x-msvideo"),
        "json" => Some("application/json"),
        "xml" => Some("text/xml"),
        "sql" => Some("application/sql"),
        "eml" => Some("message/rfc822"),
        "msg" => Some("application/vnd.ms-outlook"),
        _ => None,
    }
}

fn label_attachment_mime(filename: &str, bytes: &[u8]) -> String {
    let ext_lower = extension_of(filename).unwrap_or_default();
    let sniffed = infer::get(bytes).map(|kind| kind.mime_type().to_string());
    let labeled = extension_expected_mime(&ext_lower)
        .map(|s| s.to_string())
        .or_else(|| sniffed.clone())
        .unwrap_or_else(|| "application/octet-stream".to_string());

    // Spoofed payloads (HTML named .png, MZ bytes named .docx): keep the
    // sniffed MIME when it is on the risky list so the reader can warn.
    if let Some(sniffed) = sniffed
        && AttachmentLists::defaults().mime_is_risky(&sniffed)
    {
        return sniffed;
    }
    labeled
}

/// Incoming/stored mail: persist every part; MIME is a label, not a gate.
pub fn validate_attachment_content(filename: &str, bytes: &[u8]) -> StoreResult<String> {
    Ok(label_attachment_mime(filename, bytes))
}

/// Compose/outgoing: label the part. The sender is not gated by type or size.
pub fn outgoing_attachment_mime(filename: &str, bytes: &[u8]) -> String {
    let ext_lower = extension_of(filename).unwrap_or_default();
    let sniffed = infer::get(bytes).map(|kind| kind.mime_type().to_string());
    extension_expected_mime(&ext_lower)
        .map(|s| s.to_string())
        .or(sniffed)
        .unwrap_or_else(|| "application/octet-stream".to_string())
}

pub fn save_attachment(
    conn: &Connection,
    attachments_dir: &Path,
    meta: AttachmentMeta,
    bytes: &[u8],
) -> StoreResult<PathBuf> {
    let mime = validate_attachment_content(&meta.filename, bytes)?;

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let sha256 = format!("{:x}", hasher.finalize());
    let size = bytes.len() as i64;

    create_private_dir_all(attachments_dir)?;
    let path = attachments_dir.join(&sha256);
    if !path.exists() {
        write_private_file_atomic(&path, bytes)?;
    }

    conn.execute(
        "INSERT INTO attachments (message_id, filename, mime, sha256, size, cid)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(message_id, sha256) DO UPDATE SET
            filename = excluded.filename,
            mime = excluded.mime,
            size = excluded.size,
            cid = excluded.cid",
        params![
            meta.message_id.0,
            meta.filename,
            mime,
            sha256,
            size,
            meta.cid,
        ],
    )?;

    refresh_message_list_denorm(conn, meta.message_id)?;

    if attachment_is_downloadable(
        &meta.filename,
        &mime,
        meta.cid.as_deref(),
        &AttachmentLists::defaults(),
    ) {
        conn.execute(
            "UPDATE messages SET has_attachments = 1 WHERE id = ?1",
            params![meta.message_id.0],
        )?;
    }

    Ok(path)
}

/// Drop one saved part so a later fetch can store the real bytes.
///
/// Leaves `messages.has_attachments` alone. The row was the truncated
/// transfer-encoded prefix, and the message still has a file on the server.
pub fn delete_attachment(
    conn: &Connection,
    attachments_dir: &Path,
    message_id: MessageId,
    sha256: &str,
) -> StoreResult<()> {
    conn.execute(
        "DELETE FROM attachments WHERE message_id = ?1 AND sha256 = ?2",
        params![message_id.0, sha256],
    )?;
    refresh_message_list_denorm(conn, message_id)?;
    delete_orphaned_attachment_files(conn, attachments_dir, &[sha256.to_string()])
}

pub fn load_attachment_bytes(attachments_dir: &Path, sha256: &str) -> StoreResult<Vec<u8>> {
    if !is_sha256_hex(sha256) {
        return Err(StoreError::AttachmentRejected(
            "invalid attachment id".to_string(),
        ));
    }
    let path = attachments_dir.join(sha256);
    Ok(std::fs::read(path)?)
}

/// Distinct attachment hashes referenced by messages in `account_id`.
pub(crate) fn attachment_sha256_for_account(
    conn: &Connection,
    account_id: AccountId,
) -> StoreResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT a.sha256
         FROM attachments a
         JOIN messages m ON m.id = a.message_id
         WHERE m.account_id = ?1",
    )?;
    let rows = stmt.query_map(params![account_id.0.to_string()], |row| row.get(0))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}

/// Remove on-disk blobs that are no longer referenced in `attachments`.
pub(crate) fn delete_orphaned_attachment_files(
    conn: &Connection,
    attachments_dir: &Path,
    sha256_hashes: &[String],
) -> StoreResult<()> {
    for sha256 in sha256_hashes {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM attachments WHERE sha256 = ?1",
            params![sha256],
            |row| row.get(0),
        )?;
        if count == 0 && is_sha256_hex(sha256) {
            let path = attachments_dir.join(sha256);
            if path.exists()
                && let Err(err) = std::fs::remove_file(&path)
            {
                warn!(?sha256, error = %err, "failed to delete orphan attachment file");
            }
        }
    }
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b: u8| b.is_ascii_hexdigit())
}

/// Make an existing attachments directory owner-only (`0700` dir, `0600`
/// files). Files are only walked when the directory itself was looser, so
/// this is a one-time cost for stores created by older versions.
pub fn secure_attachments_dir(attachments_dir: &Path) -> StoreResult<()> {
    let Ok(meta) = std::fs::metadata(attachments_dir) else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 == 0 {
            return Ok(());
        }
        for entry in std::fs::read_dir(attachments_dir)?.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                restrict_file_permissions(&entry.path())?;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    create_private_dir_all(attachments_dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal valid PNG (1x1 red pixel).
    const PNG_BYTES: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xdd, 0x8d, 0xb4, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    // Minimal valid GIF89a (1x1).
    const GIF_BYTES: &[u8] = &[
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0xff, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00,
        0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
    ];

    const HTML_BYTES: &[u8] = b"<html><body>evil</body></html>";

    #[test]
    fn valid_png_accepted() {
        let mime = validate_attachment_content("photo.png", PNG_BYTES).expect("valid png");
        assert_eq!(mime, "image/png");
    }

    #[test]
    fn html_renamed_to_png_kept_as_risky_mime() {
        let mime = validate_attachment_content("photo.png", HTML_BYTES).expect("html stored");
        assert_eq!(mime, "text/html");
        assert!(AttachmentLists::defaults().mime_is_risky(&mime));
    }

    #[test]
    fn gif_bytes_with_png_extension_kept() {
        let mime =
            validate_attachment_content("photo.png", GIF_BYTES).expect("safe sniff mismatch");
        assert_eq!(mime, "image/png");
    }

    #[test]
    fn html_with_txt_extension_kept_as_risky_mime() {
        let mime = validate_attachment_content("note.txt", HTML_BYTES).expect("html stored");
        assert_eq!(mime, "text/html");
    }

    #[test]
    fn plain_text_with_txt_extension_accepted() {
        let mime = validate_attachment_content("note.txt", b"hello world").expect("plain text");
        assert_eq!(mime, "text/plain");
    }

    #[test]
    fn exe_extension_is_stored() {
        let mime = validate_attachment_content("malware.exe", b"MZ\x90\x00").expect("exe stored");
        assert!(AttachmentLists::defaults().attachment_is_risky("malware.exe", &mime, None));
    }

    #[test]
    fn extensionless_exe_bytes_stored_as_risky_mime() {
        let mime = validate_attachment_content("payload", b"MZ\x90\x00").expect("mz stored");
        assert!(AttachmentLists::defaults().mime_is_risky(&mime));
    }

    #[test]
    fn exe_bytes_with_png_extension_stored_as_risky_mime() {
        let mime = validate_attachment_content("photo.png", b"MZ\x90\x00").expect("mz stored");
        assert!(AttachmentLists::defaults().mime_is_risky(&mime));
    }

    #[test]
    fn docx_extension_accepted() {
        let mime = validate_attachment_content(
            "board_minutes.docx",
            b"arbitrary bytes infer cannot sniff",
        )
        .expect("docx extension accepted");
        assert_eq!(
            mime,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
    }

    #[test]
    fn zip_sniffed_office_docx_is_accepted() {
        // Local file header only — infer reports application/zip when OOXML
        // internals are not at the offsets it expects (Outlook extra fields).
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.extend_from_slice(&[0u8; 64]);
        let mime = validate_attachment_content("HAMBLIN VILLAGE 2026 CONTRACT.docx", &bytes)
            .expect("zip-container docx");
        assert_eq!(
            mime,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
    }

    #[test]
    fn zip_sniffed_xlsx_is_accepted() {
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.extend_from_slice(&[0u8; 64]);
        let mime = validate_attachment_content("report.xlsx", &bytes).expect("zip-container xlsx");
        assert_eq!(
            mime,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
    }

    #[test]
    fn exe_bytes_with_docx_extension_stored_as_risky_mime() {
        let mime = validate_attachment_content("invoice.docx", b"MZ\x90\x00").expect("mz stored");
        assert!(AttachmentLists::defaults().mime_is_risky(&mime));
    }

    #[test]
    fn zip_bytes_with_png_extension_labeled_on_outgoing() {
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.extend_from_slice(&[0u8; 64]);
        let mime = outgoing_attachment_mime("photo.png", &bytes);
        assert_eq!(mime, "image/png");
    }

    #[test]
    fn xlsx_and_eml_extensions_accepted() {
        let xlsx_mime =
            validate_attachment_content("report.xlsx", b"arbitrary bytes infer cannot sniff")
                .expect("xlsx extension accepted");
        assert_eq!(
            xlsx_mime,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );

        let eml_mime = validate_attachment_content(
            "forwarded.eml",
            b"From: a@b.com\r\nTo: c@d.com\r\n\r\nbody",
        )
        .expect("eml extension accepted");
        assert_eq!(eml_mime, "message/rfc822");
    }

    #[test]
    fn sql_extension_accepted() {
        let mime = validate_attachment_content(
            "live fields 2026-09-15.sql",
            b"USE [GisBenefitsSite]\r\nGO\r\n",
        )
        .expect("sql extension accepted");
        assert_eq!(mime, "application/sql");
    }

    #[test]
    fn unknown_text_extension_accepted() {
        let mime = validate_attachment_content("notes.yaml", b"foo: bar").expect("yaml kept");
        assert_eq!(mime, "application/octet-stream");
    }

    #[test]
    fn extensionless_text_accepted() {
        let mime = validate_attachment_content("Makefile", b"all:\n\techo ok").expect("no ext");
        assert_eq!(mime, "application/octet-stream");
    }

    #[test]
    fn outgoing_mime_accepts_docx() {
        let mime =
            outgoing_attachment_mime("board_minutes.docx", b"arbitrary bytes infer cannot sniff");
        assert_eq!(
            mime,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
    }

    #[test]
    fn outgoing_mime_accepts_sql() {
        let mime = outgoing_attachment_mime("query.sql", b"SELECT 1;");
        assert_eq!(mime, "application/sql");
    }

    #[test]
    fn outgoing_mime_accepts_exe() {
        let mime = outgoing_attachment_mime("setup.exe", b"MZ\x90\x00");
        assert!(!mime.is_empty());
    }
}

#[cfg(all(test, unix))]
mod permission_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn secure_attachments_dir_tightens_legacy_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("attachments");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let file = dir.join("abc");
        std::fs::write(&file, b"x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        secure_attachments_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&file), 0o600);
        // Missing dir is fine.
        secure_attachments_dir(&tmp.path().join("missing")).unwrap();
    }
}
