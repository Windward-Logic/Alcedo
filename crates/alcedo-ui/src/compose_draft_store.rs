//! Local compose draft persistence (crash recovery).
//!
//! Each open composer autosaves to its own file,
//! `<data>/alcedo/compose-drafts/<session>-<slot>.json`, so several composers
//! never overwrite each other. Files are written `0600` on a background
//! thread, and attachment / inline bytes are left out of the JSON (only their
//! file names are kept so the user can be told what to re-attach). On the next
//! launch every file from an earlier session is offered for restore.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};

use alcedo_core::ComposeDraft;
use alcedo_core::private_fs::{create_private_dir_all, write_private_file_atomic};
use serde::{Deserialize, Serialize};
use tracing::warn;

/// Which composer a saved draft belongs to within this session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DraftSlot(u64);

impl DraftSlot {
    /// The composer embedded in the main window.
    pub const INLINE: DraftSlot = DraftSlot(0);

    /// A detached composer window (window ids start at 1).
    pub fn window(id: u64) -> Self {
        DraftSlot(id)
    }
}

/// A draft recovered from an earlier session.
#[derive(Clone)]
pub struct RecoveredDraft {
    pub draft: ComposeDraft,
    /// Names of attachments / inline images that were not saved and must be re-added.
    pub missing_files: Vec<String>,
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct StoredDraft {
    draft: ComposeDraft,
    #[serde(default)]
    missing_files: Vec<String>,
}

impl StoredDraft {
    fn from_draft(draft: &ComposeDraft) -> Self {
        let missing_files = draft
            .attachments
            .iter()
            .map(|a| a.filename.clone())
            .chain(draft.inlines.iter().map(|i| i.filename.clone()))
            .collect();
        let draft = ComposeDraft {
            attachments: Vec::new(),
            inlines: Vec::new(),
            ..draft.clone()
        };
        Self {
            draft,
            missing_files,
        }
    }
}

/// Directory-scoped draft store; the free functions below use the user's data dir.
#[derive(Clone, Debug)]
struct DraftDir {
    root: PathBuf,
    session: String,
}

impl DraftDir {
    fn path_for(&self, slot: DraftSlot) -> PathBuf {
        self.root.join(format!("{}-{}.json", self.session, slot.0))
    }

    fn legacy_path(&self) -> Option<PathBuf> {
        self.root
            .parent()
            .map(|parent| parent.join("compose-draft.json"))
    }

    fn encode(draft: &ComposeDraft) -> Option<Vec<u8>> {
        serde_json::to_vec(&StoredDraft::from_draft(draft)).ok()
    }

    fn write(path: &Path, data: &[u8]) {
        if let Some(parent) = path.parent()
            && let Err(err) = create_private_dir_all(parent)
        {
            warn!(error = %err, "failed to create compose draft directory");
            return;
        }
        if let Err(err) = write_private_file_atomic(path, data) {
            warn!(error = %err, "failed to write compose draft");
        }
    }

    /// Files written by earlier sessions (plus the pre-slot legacy file).
    fn previous_session_paths(&self) -> Vec<PathBuf> {
        let prefix = format!("{}-", self.session);
        let mut paths: Vec<PathBuf> = fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    return false;
                };
                name.ends_with(".json") && !name.starts_with(&prefix)
            })
            .collect();
        paths.sort();
        if let Some(legacy) = self.legacy_path().filter(|p| p.is_file()) {
            paths.insert(0, legacy);
        }
        paths
    }

    fn load_previous(&self) -> Vec<RecoveredDraft> {
        self.previous_session_paths()
            .into_iter()
            .filter_map(|path| {
                let data = fs::read(&path).ok()?;
                let stored = serde_json::from_slice::<StoredDraft>(&data)
                    .or_else(|_| {
                        // Legacy single-file format stored the bare draft.
                        serde_json::from_slice::<ComposeDraft>(&data)
                            .map(|draft| StoredDraft::from_draft(&draft))
                    })
                    .ok();
                match stored {
                    Some(stored) => Some(RecoveredDraft {
                        draft: stored.draft,
                        missing_files: stored.missing_files,
                        path,
                    }),
                    None => {
                        set_aside_unreadable(&path);
                        None
                    }
                }
            })
            .collect()
    }
}

/// Move a draft this build cannot parse out of the way instead of deleting
/// it: it may be unsent text written by a newer version, and the user can
/// still recover it by hand. The new name no longer ends in `.json`, so it
/// is not offered again.
fn set_aside_unreadable(path: &Path) {
    let mut kept = path.as_os_str().to_owned();
    kept.push(".unreadable");
    if let Err(err) = fs::rename(path, &kept) {
        warn!(error = %err, "failed to set aside an unreadable compose draft");
    }
}

enum Job {
    Write(PathBuf, Vec<u8>),
    Remove(PathBuf),
}

fn draft_dir() -> Option<&'static DraftDir> {
    static DIR: OnceLock<Option<DraftDir>> = OnceLock::new();
    DIR.get_or_init(|| {
        let base = directories::BaseDirs::new()?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        Some(DraftDir {
            root: base.data_dir().join("alcedo").join("compose-drafts"),
            session: format!("{nanos:x}{:x}", std::process::id()),
        })
    })
    .as_ref()
}

/// Single background writer so file I/O never blocks the GTK main loop and
/// saves / removals for a slot apply in order.
fn writer() -> &'static Sender<Job> {
    static WRITER: OnceLock<Sender<Job>> = OnceLock::new();
    WRITER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("alcedo-draft-autosave".into())
            .spawn(move || {
                for job in rx {
                    match job {
                        Job::Write(path, data) => DraftDir::write(&path, &data),
                        Job::Remove(path) => {
                            let _ = fs::remove_file(path);
                        }
                    }
                }
            })
            .expect("spawn draft autosave thread");
        tx
    })
}

/// Persist the in-progress draft of one composer (off the UI thread).
pub fn save_compose_draft(slot: DraftSlot, draft: &ComposeDraft) {
    let Some(dir) = draft_dir() else {
        return;
    };
    let Some(data) = DraftDir::encode(draft) else {
        warn!("failed to serialize compose draft");
        return;
    };
    let _ = writer().send(Job::Write(dir.path_for(slot), data));
}

/// Remove one composer's saved draft after send or explicit discard.
pub fn clear_compose_draft(slot: DraftSlot) {
    if let Some(dir) = draft_dir() {
        let _ = writer().send(Job::Remove(dir.path_for(slot)));
    }
}

/// Drafts left behind by earlier sessions (e.g. after a crash).
pub fn load_recovered_drafts() -> Vec<RecoveredDraft> {
    draft_dir().map(DraftDir::load_previous).unwrap_or_default()
}

/// Delete recovered drafts once restored (they re-save under new slots) or discarded.
pub fn forget_recovered_drafts(drafts: &[RecoveredDraft]) {
    for draft in drafts {
        let _ = writer().send(Job::Remove(draft.path.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{ComposeAttachment, ComposeInline};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "alcedo-drafts-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir.join("compose-drafts")
    }

    fn draft(subject: &str) -> ComposeDraft {
        serde_json::from_value(serde_json::json!({
            "account_id": "00000000-0000-0000-0000-000000000001", "to": [], "cc": [], "bcc": [], "subject": subject,
            "body_text": "hi", "request_mdn": false, "tracking_pixel_url": null
        }))
        .unwrap()
    }

    fn save_sync(dir: &DraftDir, slot: DraftSlot, d: &ComposeDraft) {
        DraftDir::write(&dir.path_for(slot), &DraftDir::encode(d).unwrap());
    }

    #[test]
    fn each_composer_has_its_own_file_and_previous_sessions_are_recovered() {
        let root = scratch("slots");
        let old = DraftDir {
            root: root.clone(),
            session: "old".into(),
        };
        save_sync(&old, DraftSlot::INLINE, &draft("inline"));
        save_sync(&old, DraftSlot::window(1), &draft("window one"));
        save_sync(&old, DraftSlot::window(2), &draft("window two"));
        let now = DraftDir {
            root: root.clone(),
            session: "new".into(),
        };
        save_sync(&now, DraftSlot::INLINE, &draft("current"));
        let subjects: Vec<String> = now
            .load_previous()
            .into_iter()
            .map(|r| r.draft.subject)
            .collect();
        assert_eq!(subjects, ["inline", "window one", "window two"]);
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn attachment_bytes_are_not_written_but_names_are_kept() {
        let root = scratch("bytes");
        let dir = DraftDir {
            root: root.clone(),
            session: "s".into(),
        };
        let mut d = draft("with files");
        d.attachments.push(ComposeAttachment {
            filename: "report.pdf".into(),
            mime: "application/pdf".into(),
            bytes: vec![0xAB; 4096],
        });
        d.inlines.push(ComposeInline {
            content_id: "logo@x".into(),
            filename: "logo.png".into(),
            mime: "image/png".into(),
            bytes: vec![0xCD; 4096],
        });
        save_sync(&dir, DraftSlot::INLINE, &d);
        let raw = fs::read(dir.path_for(DraftSlot::INLINE)).unwrap();
        assert!(
            raw.len() < 2048,
            "bytes leaked into autosave: {}",
            raw.len()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir.path_for(DraftSlot::INLINE)), 0o600);
            assert_eq!(mode(&root), 0o700);
        }
        let later = DraftDir {
            root: root.clone(),
            session: "t".into(),
        };
        let recovered = later.load_previous();
        assert_eq!(recovered.len(), 1);
        assert!(recovered[0].draft.attachments.is_empty());
        assert_eq!(recovered[0].missing_files, ["report.pdf", "logo.png"]);
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn legacy_single_file_is_recovered_and_corrupt_files_set_aside() {
        let root = scratch("legacy");
        fs::create_dir_all(&root).unwrap();
        let legacy = root.parent().unwrap().join("compose-draft.json");
        fs::write(&legacy, serde_json::to_vec(&draft("legacy")).unwrap()).unwrap();
        fs::write(root.join("old-0.json"), b"{not json").unwrap();
        let dir = DraftDir {
            root: root.clone(),
            session: "new".into(),
        };
        let recovered = dir.load_previous();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].draft.subject, "legacy");
        assert!(!root.join("old-0.json").exists());
        assert_eq!(
            fs::read(root.join("old-0.json.unreadable")).unwrap(),
            b"{not json",
            "an unreadable draft is kept for manual recovery, not deleted"
        );
        assert!(
            dir.load_previous()
                .iter()
                .all(|r| r.draft.subject == "legacy"),
            "the set-aside file is not offered again"
        );
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }
}
