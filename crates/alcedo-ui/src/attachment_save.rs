//! Shared, GTK-free path for writing reader attachments to disk.
//!
//! "Save", "Save all" and "Open" all go through here so they apply the same
//! rules: verify the cached bytes are the complete decoded file first, and
//! write through a temporary file plus rename so a failed or interrupted
//! write never leaves a partial file under the user's chosen name.

use alcedo_i18n::{ntr, tr, trf};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use sha2::{Digest, Sha256};
use unicode_general_category::{GeneralCategory, get_general_category};

/// One attachment ready to be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SaveItem {
    pub filename: String,
    pub mime: String,
    pub sha256: String,
    pub bytes: Vec<u8>,
}

/// Why cached bytes are not safe to hand to the user as the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CorruptReason {
    Empty,
    /// Bytes no longer match the content hash they were stored under.
    HashMismatch,
    /// Bytes are still the base64 transfer text of a binary file (the
    /// part was cut off or never decoded).
    UndecodedBase64,
}

impl fmt::Display for CorruptReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "empty",
            Self::HashMismatch => "content hash mismatch",
            Self::UndecodedBase64 => "still base64-encoded (incomplete download)",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SaveError {
    NotLoaded {
        filename: String,
    },
    Corrupt {
        filename: String,
        reason: CorruptReason,
    },
}

impl SaveError {
    /// Toast text for the reader.
    pub(crate) fn user_message(&self) -> String {
        match self {
            Self::NotLoaded { filename } => {
                trf!("“{filename}” is still downloading", filename = filename)
            }
            Self::Corrupt { filename, .. } => trf!(
                "“{filename}” didn’t download completely, so it wasn’t saved. Reopen the message to download it again.",
                filename = filename
            ),
        }
    }
}

/// Check that cached attachment bytes are the complete decoded file.
pub(crate) fn verify_attachment_bytes(
    sha256: &str,
    mime: &str,
    bytes: &[u8],
) -> Result<(), CorruptReason> {
    if bytes.is_empty() {
        return Err(CorruptReason::Empty);
    }
    if is_sha256_hex(sha256) {
        let actual = format!("{:x}", Sha256::digest(bytes));
        if !actual.eq_ignore_ascii_case(sha256) {
            return Err(CorruptReason::HashMismatch);
        }
    }
    if alcedo_core::attachment_bytes_are_undecoded_base64(mime, bytes) {
        return Err(CorruptReason::UndecodedBase64);
    }
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Look up and verify every requested `(sha256, filename)`.
///
/// All-or-nothing: the first missing or corrupt attachment is reported and
/// nothing is written, so "Save all" never leaves a mix of good and bad
/// files behind. `lookup` returns `(bytes, mime)` from the reader's cache.
pub(crate) fn prepare_save_items(
    requested: &[(String, String)],
    lookup: impl Fn(&str) -> Option<(Vec<u8>, String)>,
) -> Result<Vec<SaveItem>, SaveError> {
    let mut items = Vec::with_capacity(requested.len());
    for (sha256, filename) in requested {
        let Some((bytes, mime)) = lookup(sha256) else {
            return Err(SaveError::NotLoaded {
                filename: filename.clone(),
            });
        };
        if let Err(reason) = verify_attachment_bytes(sha256, &mime, &bytes) {
            tracing::warn!(
                sha256 = %sha256,
                filename = %filename,
                size = bytes.len(),
                %reason,
                "refusing to save corrupt attachment"
            );
            return Err(SaveError::Corrupt {
                filename: filename.clone(),
                reason,
            });
        }
        items.push(SaveItem {
            filename: filename.clone(),
            mime,
            sha256: sha256.clone(),
            bytes,
        });
    }
    Ok(items)
}

static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Bytes of the target name kept in a temp file name, which also carries a
/// leading dot, pid, counter and `.alcedo-part` (under 40 bytes together).
const MAX_TEMP_NAME_BYTES: usize = 100;

/// Write `bytes` to `path` atomically: a hidden temp file in the same
/// directory is written, flushed to disk and renamed over `path`. Readers of
/// `path` see either the old file or the complete new one, never a prefix.
pub(crate) fn write_file_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "attachment".to_string());
    let (temp_path, mut file) = create_temp_file(dir, &name)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn create_temp_file(dir: &Path, name: &str) -> io::Result<(PathBuf, File)> {
    // Cap by bytes: 64 four-byte characters would already pass the 255-byte
    // limit once the pid, counter and suffix are added.
    let mut short = String::new();
    for ch in name.chars() {
        if short.len() + ch.len_utf8() > MAX_TEMP_NAME_BYTES {
            break;
        }
        short.push(ch);
    }
    for _ in 0..16 {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = dir.join(format!(".{short}.{}.{n}.alcedo-part", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a temporary file",
    ))
}

/// Outcome of writing several attachments into one folder.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SaveSummary {
    pub saved: usize,
    pub total: usize,
}

impl SaveSummary {
    pub(crate) fn user_message(&self) -> String {
        if self.saved == self.total {
            if self.saved == 1 {
                tr!("Attachment saved")
            } else {
                ntr!("{n} attachment saved", "{n} attachments saved", self.saved)
            }
        } else if self.saved == 0 {
            tr!("Could not save attachments")
        } else {
            ntr!(
                "Saved {saved} of {n} attachment",
                "Saved {saved} of {n} attachments",
                self.total,
                saved = self.saved
            )
        }
    }
}

/// Write verified items into `dir` under sanitized names, never replacing
/// an existing file (`name.ext`, `name_1.ext`, …).
pub(crate) fn write_items_to_dir(dir: &Path, items: &[SaveItem]) -> SaveSummary {
    let mut summary = SaveSummary {
        saved: 0,
        total: items.len(),
    };
    for item in items {
        let name = sanitize_filename(&item.filename, "attachment");
        match write_file_no_clobber(dir, &name, &item.bytes) {
            Ok(_) => summary.saved += 1,
            Err(err) => {
                tracing::warn!(?err, dir = %dir.display(), "failed to save attachment");
            }
        }
    }
    summary
}

/// Candidate names for `safe_name`: itself, then `stem_1.ext`, `stem_2.ext`, ….
pub(crate) fn destination_candidates(safe_name: &str) -> impl Iterator<Item = String> + '_ {
    let (stem, ext) = match safe_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => (stem, format!(".{ext}")),
        _ => (safe_name, String::new()),
    };
    std::iter::once(safe_name.to_string())
        .chain((1..10_000u32).map(move |n| format!("{stem}_{n}{ext}")))
}

/// Write `bytes` into `dir` under the first free candidate name. The file
/// appears complete and atomically (temp file + `link`, which fails instead
/// of overwriting); on filesystems without hard links it falls back to
/// `create_new`, which still never clobbers.
pub(crate) fn write_file_no_clobber(
    dir: &Path,
    safe_name: &str,
    bytes: &[u8],
) -> io::Result<PathBuf> {
    let (temp_path, mut file) = create_temp_file(dir, safe_name)?;
    let written = (|| {
        file.write_all(bytes)?;
        file.sync_all()
    })();
    drop(file);
    if let Err(err) = written {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }
    let mut link_supported = true;
    let mut result = Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free file name",
    ));
    for name in destination_candidates(safe_name) {
        let candidate = dir.join(&name);
        if link_supported {
            match fs::hard_link(&temp_path, &candidate) {
                Ok(()) => {
                    result = Ok(candidate);
                    break;
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => link_supported = false,
            }
        }
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut out) => {
                result = out
                    .write_all(bytes)
                    .and_then(|()| out.sync_all())
                    .map(|()| candidate.clone());
                if result.is_err() {
                    let _ = fs::remove_file(&candidate);
                }
                break;
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                result = Err(err);
                break;
            }
        }
    }
    let _ = fs::remove_file(&temp_path);
    result
}

/// Longest sanitized name in UTF-8 bytes. Filesystems cap one path component
/// at 255 bytes; this leaves room for the 8-character hash and `_` that the
/// opened-attachment cache prefixes (9 bytes), and also for `.eml` (4) and the
/// `_9999` collision suffix (5).
pub(crate) const MAX_FILENAME_BYTES: usize = 255 - 9;

/// What `sanitize_filename` does with one character.
enum CharClass {
    Keep,
    Drop,
    Replace,
}

fn classify_filename_char(ch: char) -> CharClass {
    if ch.is_ascii() {
        return if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
            CharClass::Keep
        } else {
            CharClass::Replace
        };
    }
    match get_general_category(ch) {
        // Control and format characters (bidi overrides, zero-width marks,
        // BOM, soft hyphen, tag characters) are invisible and can disguise a
        // name, so they are removed rather than shown as `_`.
        GeneralCategory::Control | GeneralCategory::Format => CharClass::Drop,
        _ if is_invisible_filler(ch) => CharClass::Drop,
        GeneralCategory::UppercaseLetter
        | GeneralCategory::LowercaseLetter
        | GeneralCategory::TitlecaseLetter
        | GeneralCategory::ModifierLetter
        | GeneralCategory::OtherLetter
        | GeneralCategory::DecimalNumber
        | GeneralCategory::LetterNumber
        | GeneralCategory::NonspacingMark
        | GeneralCategory::SpacingMark
        | GeneralCategory::EnclosingMark => CharClass::Keep,
        _ => CharClass::Replace,
    }
}

/// Letters and marks that render as nothing: the combining grapheme joiner,
/// variation selectors, and the Hangul fillers.
fn is_invisible_filler(ch: char) -> bool {
    matches!(
        ch,
        '\u{034F}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{3164}'
            | '\u{FFA0}'
            | '\u{180B}'..='\u{180D}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{E0100}'..='\u{E01EF}'
    )
}

/// The one filename sanitizer: keep the base name only; keep ASCII letters,
/// digits, `.`, `-`, `_` and Unicode letters, digits and combining marks;
/// drop non-ASCII control, format and invisible characters; replace anything
/// else with `_`. Strips leading dots (no hidden or `..` names), caps the
/// result at 120 chars and `MAX_FILENAME_BYTES` bytes (never splitting a
/// character), and uses `fallback` when nothing is left.
pub(crate) fn sanitize_filename(name: &str, fallback: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let mut cleaned = String::with_capacity(base.len().min(MAX_FILENAME_BYTES));
    let mut chars = 0;
    for ch in base.chars() {
        let kept = match classify_filename_char(ch) {
            CharClass::Keep => ch,
            CharClass::Replace => '_',
            CharClass::Drop => continue,
        };
        // Leading dots are checked after drops so an invisible character
        // cannot hide one.
        if cleaned.is_empty() && kept == '.' {
            continue;
        }
        if chars == 120 || cleaned.len() + kept.len_utf8() > MAX_FILENAME_BYTES {
            break;
        }
        cleaned.push(kept);
        chars += 1;
    }
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

    fn sha(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn docx_bytes() -> Vec<u8> {
        let mut bytes = b"PK\x03\x04\x14\x00\x08\x08\x08\x00".to_vec();
        bytes.resize(4096, 7);
        bytes
    }

    /// The first bytes of the broken file Carter saved: base64 text of a DOCX.
    fn broken_docx_text() -> Vec<u8> {
        let mut text =
            b"UEsDBBQACAgIAAAAAAAAAAAAAAAAAAAAAAAAFAAAAHdvcmQvZG9jdW1lbnQueG1s\r\n".to_vec();
        text.extend_from_slice(&[b'A'; 2000]);
        text
    }

    #[test]
    fn verify_accepts_complete_file() {
        let bytes = docx_bytes();
        assert_eq!(verify_attachment_bytes(&sha(&bytes), DOCX, &bytes), Ok(()));
    }

    #[test]
    fn verify_rejects_undecoded_base64_even_when_hash_matches() {
        // The cache key is the hash of whatever was stored, so the hash
        // alone cannot catch bytes that were bad when stored.
        let bytes = broken_docx_text();
        assert_eq!(
            verify_attachment_bytes(&sha(&bytes), DOCX, &bytes),
            Err(CorruptReason::UndecodedBase64)
        );
    }

    #[test]
    fn verify_rejects_hash_mismatch_and_empty() {
        let bytes = docx_bytes();
        let other = sha(b"other");
        assert_eq!(
            verify_attachment_bytes(&other, DOCX, &bytes),
            Err(CorruptReason::HashMismatch)
        );
        assert_eq!(
            verify_attachment_bytes(&sha(b""), DOCX, b""),
            Err(CorruptReason::Empty)
        );
    }

    #[test]
    fn verify_skips_hash_check_for_non_hash_keys() {
        let bytes = docx_bytes();
        assert_eq!(verify_attachment_bytes("cid:logo", DOCX, &bytes), Ok(()));
    }

    fn cache(entries: &[(&str, &[u8], &str)]) -> HashMap<String, (Vec<u8>, String)> {
        entries
            .iter()
            .map(|(key, bytes, mime)| (key.to_string(), (bytes.to_vec(), mime.to_string())))
            .collect()
    }

    /// Regression: Save and Save all wrote the cached base64 text as the
    /// .docx. Both now share `prepare_save_items`, which refuses it.
    #[test]
    fn prepare_refuses_whole_batch_when_one_attachment_is_corrupt() {
        let good = docx_bytes();
        let bad = broken_docx_text();
        let (good_sha, bad_sha) = (sha(&good), sha(&bad));
        let cache = cache(&[(&good_sha, &good, DOCX), (&bad_sha, &bad, DOCX)]);
        let requested = vec![
            (good_sha.clone(), "small.docx".to_string()),
            (bad_sha.clone(), "OPTIVATIONS.docx".to_string()),
        ];
        let err = prepare_save_items(&requested, |key| cache.get(key).cloned())
            .expect_err("corrupt attachment must not be saved");
        assert_eq!(
            err,
            SaveError::Corrupt {
                filename: "OPTIVATIONS.docx".to_string(),
                reason: CorruptReason::UndecodedBase64
            }
        );
        assert!(err.user_message().contains("didn’t download completely"));

        // Single save goes through the same function.
        let single = vec![(bad_sha, "OPTIVATIONS.docx".to_string())];
        assert!(prepare_save_items(&single, |key| cache.get(key).cloned()).is_err());
        let single = vec![(good_sha, "small.docx".to_string())];
        let items = prepare_save_items(&single, |key| cache.get(key).cloned()).unwrap();
        assert_eq!(items[0].bytes, good);
    }

    #[test]
    fn prepare_reports_missing_bytes() {
        let requested = vec![("ab".repeat(32), "a.pdf".to_string())];
        assert_eq!(
            prepare_save_items(&requested, |_| None),
            Err(SaveError::NotLoaded {
                filename: "a.pdf".to_string()
            })
        );
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "alcedo-attachment-save-{tag}-{}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn leftover_temp_files(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".alcedo-part")
            })
            .count()
    }

    #[test]
    fn atomic_write_replaces_existing_file_completely() {
        let dir = temp_dir("replace");
        let path = dir.join("report.docx");
        fs::write(&path, b"old contents that are longer than the new ones").unwrap();
        write_file_atomic(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(leftover_temp_files(&dir), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn atomic_write_failure_leaves_no_partial_file() {
        let dir = temp_dir("fail");
        // Renaming a file over a non-empty directory fails.
        let path = dir.join("taken");
        fs::create_dir_all(path.join("child")).unwrap();
        assert!(write_file_atomic(&path, b"data").is_err());
        assert!(path.is_dir());
        assert_eq!(leftover_temp_files(&dir), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_items_to_dir_writes_exact_bytes() {
        let dir = temp_dir("dir");
        let bytes = docx_bytes();
        let items = vec![
            SaveItem {
                filename: "a.docx".to_string(),
                mime: DOCX.to_string(),
                sha256: sha(&bytes),
                bytes: bytes.clone(),
            },
            SaveItem {
                filename: "b.bin".to_string(),
                mime: "application/octet-stream".to_string(),
                sha256: sha(b"xyz"),
                bytes: b"xyz".to_vec(),
            },
        ];
        let summary = write_items_to_dir(&dir, &items);
        assert_eq!(summary, SaveSummary { saved: 2, total: 2 });
        assert_eq!(summary.user_message(), "2 attachments saved");
        assert_eq!(fs::read(dir.join("a.docx")).unwrap(), bytes);
        assert_eq!(fs::read(dir.join("b.bin")).unwrap(), b"xyz");
        // A second save never overwrites: it picks the next free name.
        let again = write_items_to_dir(&dir, &items[1..]);
        assert_eq!(again.saved, 1);
        assert_eq!(fs::read(dir.join("b.bin")).unwrap(), b"xyz");
        assert_eq!(fs::read(dir.join("b_1.bin")).unwrap(), b"xyz");
        let leftovers = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".alcedo-part"))
            .count();
        assert_eq!(leftovers, 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_clobber_write_skips_existing_names() {
        let dir = temp_dir("noclobber");
        fs::write(dir.join("photo.png"), b"old").unwrap();
        fs::write(dir.join("photo_1.png"), b"old1").unwrap();
        let path = write_file_no_clobber(&dir, "photo.png", b"new").unwrap();
        assert_eq!(path, dir.join("photo_2.png"));
        assert_eq!(fs::read(dir.join("photo.png")).unwrap(), b"old");
        assert_eq!(fs::read(&path).unwrap(), b"new");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sanitize_filename_strips_paths_hidden_dots_and_specials() {
        assert_eq!(
            sanitize_filename("invoice.pdf", "attachment"),
            "invoice.pdf"
        );
        assert_eq!(
            sanitize_filename("../../etc/passwd", "attachment"),
            "passwd"
        );
        assert_eq!(sanitize_filename(".bashrc", "attachment"), "bashrc");
        assert_eq!(sanitize_filename("...", "attachment"), "attachment");
        assert_eq!(sanitize_filename("..", "attachment"), "attachment");
        assert_eq!(sanitize_filename("", "message"), "message");
        assert_eq!(sanitize_filename("my file (1).pdf", "x"), "my_file__1_.pdf");
        assert_eq!(sanitize_filename(&"a".repeat(300), "x").len(), 120);
        let names: Vec<String> = destination_candidates("notes").take(3).collect();
        assert_eq!(names, ["notes", "notes_1", "notes_2"]);
        let names: Vec<String> = destination_candidates("a.tar.gz").take(2).collect();
        assert_eq!(names, ["a.tar.gz", "a.tar_1.gz"]);
    }

    #[test]
    fn sanitize_keeps_unicode_letters_digits_and_marks() {
        // Precomposed, decomposed (base letter + combining acute), other
        // scripts, a script whose vowel signs are combining marks, and a
        // non-ASCII decimal digit.
        for name in [
            "résumé.pdf",
            "re\u{301}sume\u{301}.pdf",
            "日本語のメモ.txt",
            "Отчёт.docx",
            "ملف.pdf",
            "नमस्ते.txt",
            "٣٤٥.txt",
        ] {
            assert_eq!(sanitize_filename(name, "attachment"), name, "{name:?}");
        }
    }

    #[test]
    fn sanitize_still_replaces_punctuation_and_symbols_outside_ascii() {
        assert_eq!(sanitize_filename("a\u{a0}b“c”€.txt", "x"), "a_b_c__.txt");
    }

    #[test]
    fn sanitize_drops_bidi_overrides_instead_of_spoofing_extension() {
        // Rendered with the override active this reads "invoiceexe.pdf".
        assert_eq!(
            sanitize_filename("invoice\u{202E}fdp.exe", "attachment"),
            "invoicefdp.exe"
        );
    }

    #[test]
    fn sanitize_drops_invisible_and_formatting_characters() {
        let invisible = [
            '\u{200B}',
            '\u{200C}',
            '\u{200D}',
            '\u{200E}',
            '\u{200F}',
            '\u{202A}',
            '\u{202B}',
            '\u{202C}',
            '\u{202D}',
            '\u{202E}',
            '\u{2066}',
            '\u{2067}',
            '\u{2068}',
            '\u{2069}',
            '\u{FEFF}',
            '\u{00AD}',
            '\u{2060}',
            '\u{061C}',
            '\u{E0041}',
            '\u{FE0F}',
            '\u{034F}',
            '\u{0085}',
            '\u{009F}',
        ];
        for ch in invisible {
            let name = format!("re{ch}port.pdf");
            assert_eq!(
                sanitize_filename(&name, "x"),
                "report.pdf",
                "U+{:04X}",
                ch as u32
            );
        }
    }

    #[test]
    fn sanitize_falls_back_when_only_invisible_characters_remain() {
        assert_eq!(
            sanitize_filename("\u{202E}\u{200B}\u{FEFF}", "attachment"),
            "attachment"
        );
        // Dropped characters must not hide a leading dot.
        assert_eq!(sanitize_filename("\u{200B}.bashrc", "x"), "bashrc");
    }

    #[test]
    fn sanitize_keeps_base_name_only_for_unicode_paths() {
        assert_eq!(sanitize_filename("../résumés/été.pdf", "x"), "été.pdf");
        assert_eq!(sanitize_filename(".été", "x"), "été");
    }

    #[test]
    fn sanitize_caps_multibyte_names_in_bytes_at_a_char_boundary() {
        // 200 chars would be 600 bytes; 120 chars would still be 360 > 255.
        let long = "日".repeat(200);
        let safe = sanitize_filename(&long, "x");
        assert!(safe.len() <= MAX_FILENAME_BYTES, "{} bytes", safe.len());
        assert_eq!(safe, "日".repeat(MAX_FILENAME_BYTES / 3));

        // 4-byte chars and a 2-byte tail: never splits a character.
        let long = format!("{}é.pdf", "𠀀".repeat(100));
        let safe = sanitize_filename(&long, "x");
        assert!(safe.len() <= MAX_FILENAME_BYTES);
        assert!(safe.chars().all(|ch| ch == '𠀀'));
        assert_eq!(safe.chars().count(), MAX_FILENAME_BYTES / 4);
    }

    #[test]
    fn sanitize_keeps_the_120_character_cap_for_ascii() {
        assert_eq!(sanitize_filename(&"a".repeat(300), "x"), "a".repeat(120));
    }

    #[test]
    fn capped_name_fits_cache_prefix_eml_suffix_and_collision_suffix() {
        let safe = sanitize_filename(&"日".repeat(200), "x");
        assert!(safe.starts_with('日') && safe.len() > 120);
        let cache_name = format!("{}_{safe}", "abcd1234");
        assert!(cache_name.len() <= 255, "{} bytes", cache_name.len());
        assert!(format!("{safe}.eml").len() <= 255);
        let last = destination_candidates(&safe).last().unwrap();
        assert!(last.len() <= 255, "{} bytes", last.len());
    }

    #[test]
    fn long_unicode_names_can_be_written_to_disk() {
        // Temp file names embed the (capped) name; they must stay under the
        // filesystem's 255-byte component limit too.
        let dir = temp_dir("unicode-long");
        let items = vec![SaveItem {
            filename: "𠀀".repeat(100),
            mime: "application/octet-stream".to_string(),
            sha256: sha(b"xyz"),
            bytes: b"xyz".to_vec(),
        }];
        let summary = write_items_to_dir(&dir, &items);
        assert_eq!(summary, SaveSummary { saved: 1, total: 1 });
        let expected = sanitize_filename(&items[0].filename, "attachment");
        assert!(expected.starts_with('𠀀'));
        assert_eq!(fs::read(dir.join(expected)).unwrap(), b"xyz");
        assert_eq!(leftover_temp_files(&dir), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn summary_messages() {
        assert_eq!(
            SaveSummary { saved: 1, total: 1 }.user_message(),
            "Attachment saved"
        );
        assert_eq!(
            SaveSummary { saved: 0, total: 2 }.user_message(),
            "Could not save attachments"
        );
        assert_eq!(
            SaveSummary { saved: 1, total: 3 }.user_message(),
            "Saved 1 of 3 attachments"
        );
    }
}
