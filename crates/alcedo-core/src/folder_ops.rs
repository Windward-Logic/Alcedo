//! Pure helpers for IMAP mailbox create, rename, and delete.

use crate::account::{Folder, SpecialUse};
use crate::ids::FolderId;
use crate::mailbox_utf7::{decode_mailbox_name, encode_mailbox_name};

pub fn folder_is_mutable(folder: &Folder) -> bool {
    folder.selectable && folder.special == SpecialUse::Other
}

/// Human-readable folder label (special-use title case, else last IMAP path segment).
pub fn folder_display_name(folder: &Folder) -> String {
    match folder.special {
        SpecialUse::Inbox => "Inbox".to_string(),
        SpecialUse::Sent => "Sent".to_string(),
        SpecialUse::Drafts => "Drafts".to_string(),
        SpecialUse::Trash => "Trash".to_string(),
        SpecialUse::Junk => "Junk".to_string(),
        SpecialUse::Archive => "Archive".to_string(),
        SpecialUse::Snoozed => "Snoozed".to_string(),
        SpecialUse::Outbox => "Outbox".to_string(),
        SpecialUse::All => "All".to_string(),
        SpecialUse::Other => {
            let leaf = if folder.delimiter.is_empty() {
                folder.imap_name.as_str()
            } else {
                folder
                    .imap_name
                    .rsplit(&folder.delimiter)
                    .next()
                    .unwrap_or(&folder.imap_name)
            };
            decode_mailbox_name(leaf)
        }
    }
}

/// Server spelling of a new mailbox: `parent` is already server-spelled, and
/// `leaf` is what the user typed, so it is encoded (modified UTF-7).
pub fn join_mailbox_path(parent_imap: Option<&str>, delimiter: &str, leaf: &str) -> String {
    let leaf = encode_mailbox_name(leaf);
    match parent_imap {
        Some(parent) => format!("{parent}{delimiter}{leaf}"),
        None => leaf.to_string(),
    }
}

/// IMAP hierarchy delimiter, or `"/"` when the server reported NIL/empty.
pub fn effective_mailbox_delimiter(delimiter: &str) -> &str {
    if delimiter.is_empty() { "/" } else { delimiter }
}

/// Delimiter for a create/rename parent: the parent folder, else Inbox, else `"/"`.
pub fn folder_delimiter_for_parent(folders: &[Folder], parent: Option<FolderId>) -> String {
    let source = match parent {
        Some(parent_id) => folders.iter().find(|folder| folder.id == parent_id),
        None => folders
            .iter()
            .find(|folder| matches!(folder.special, SpecialUse::Inbox)),
    };
    effective_mailbox_delimiter(source.map(|folder| folder.delimiter.as_str()).unwrap_or(""))
        .to_string()
}

pub fn validate_folder_leaf(leaf: &str, delimiter: &str) -> Result<(), String> {
    let trimmed = leaf.trim();
    if trimmed.is_empty() {
        return Err("Enter a folder name".to_string());
    }
    if !delimiter.is_empty() && trimmed.contains(delimiter) {
        return Err(format!("Folder name cannot contain “{delimiter}”"));
    }
    Ok(())
}

pub fn imap_name_exists(folders: &[Folder], imap_name: &str) -> bool {
    folders.iter().any(|folder| folder.imap_name == imap_name)
}

pub fn descendant_imap_names(
    folders: &[Folder],
    parent_imap: &str,
    delimiter: &str,
) -> Vec<String> {
    if delimiter.is_empty() {
        return folders
            .iter()
            .filter(|folder| folder.imap_name == parent_imap)
            .map(|folder| folder.imap_name.clone())
            .collect();
    }
    let prefix = format!("{parent_imap}{delimiter}");
    folders
        .iter()
        .filter(|folder| folder.imap_name == parent_imap || folder.imap_name.starts_with(&prefix))
        .map(|folder| folder.imap_name.clone())
        .collect()
}

/// Nested IMAP names under `parent_imap`, excluding the parent, deepest first.
///
/// Empty Trash/Junk uses this so folders moved under Trash are deleted with
/// the messages, matching typical IMAP clients.
pub fn nested_imap_names(folders: &[Folder], parent_imap: &str, delimiter: &str) -> Vec<String> {
    let mut names = descendant_imap_names(folders, parent_imap, delimiter);
    names.retain(|name| name != parent_imap);
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    names
}

pub fn rename_imap_map(
    old_name: &str,
    new_name: &str,
    delimiter: &str,
    names: &[String],
) -> Vec<(String, String)> {
    let old_prefix = format!("{old_name}{delimiter}");
    let mut pairs: Vec<(String, String)> = names
        .iter()
        .filter_map(|name| {
            if name == old_name {
                Some((name.clone(), new_name.to_string()))
            } else if !delimiter.is_empty() && name.starts_with(&old_prefix) {
                let suffix = &name[old_prefix.len()..];
                Some((name.clone(), format!("{new_name}{delimiter}{suffix}")))
            } else {
                None
            }
        })
        .collect();
    pairs.sort_by(|(left, _), (right, _)| right.len().cmp(&left.len()));
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::AccountId;
    use uuid::Uuid;

    fn sample_folder(imap_name: &str) -> Folder {
        Folder {
            id: FolderId(1),
            account_id: AccountId(Uuid::new_v4()),
            imap_name: imap_name.to_string(),
            delimiter: "/".to_string(),
            special: SpecialUse::Other,
            selectable: true,
            uidvalidity: None,
            uidnext: None,
            highest_modseq: None,
            unread: 0,
            total: 0,
        }
    }

    #[test]
    fn folder_display_name_special_use_and_leaf() {
        let mut inbox = sample_folder("INBOX");
        inbox.special = SpecialUse::Inbox;
        assert_eq!(folder_display_name(&inbox), "Inbox");

        let nested = sample_folder("[Gmail]/Sent Mail");
        assert_eq!(folder_display_name(&nested), "Sent Mail");

        let mut no_delim = sample_folder("INBOX");
        no_delim.delimiter.clear();
        assert_eq!(folder_display_name(&no_delim), "INBOX");
    }

    #[test]
    fn join_mailbox_path_root_and_nested() {
        assert_eq!(join_mailbox_path(None, "/", "Projects"), "Projects");
        assert_eq!(
            join_mailbox_path(Some("Projects"), "/", "Work"),
            "Projects/Work"
        );
    }

    #[test]
    fn validate_folder_leaf_empty_and_delimiter_errors() {
        assert_eq!(
            validate_folder_leaf("", "/"),
            Err("Enter a folder name".to_string())
        );
        assert_eq!(
            validate_folder_leaf("   ", "/"),
            Err("Enter a folder name".to_string())
        );
        assert_eq!(
            validate_folder_leaf("a/b", "/"),
            Err("Folder name cannot contain “/”".to_string())
        );
    }

    #[test]
    fn imap_name_exists_checks_exact_match() {
        let folders = vec![sample_folder("INBOX"), sample_folder("Projects")];
        assert!(imap_name_exists(&folders, "Projects"));
        assert!(!imap_name_exists(&folders, "Archive"));
    }

    #[test]
    fn descendant_imap_names_include_nested() {
        let folders = vec![
            sample_folder("Projects"),
            sample_folder("Projects/Work"),
            sample_folder("Projects/Work/Done"),
            sample_folder("Archive"),
        ];
        let names = descendant_imap_names(&folders, "Projects", "/");
        assert_eq!(
            names,
            vec![
                "Projects".to_string(),
                "Projects/Work".to_string(),
                "Projects/Work/Done".to_string(),
            ]
        );
    }

    #[test]
    fn rename_imap_map_prefix_replace() {
        let names = vec![
            "Projects".to_string(),
            "Projects/Work".to_string(),
            "Archive".to_string(),
        ];
        let pairs = rename_imap_map("Projects", "Archive", "/", &names);
        assert_eq!(
            pairs,
            vec![
                ("Projects/Work".to_string(), "Archive/Work".to_string()),
                ("Projects".to_string(), "Archive".to_string()),
            ]
        );
    }

    #[test]
    fn descendant_imap_names_empty_delimiter_is_exact_only() {
        let folders = vec![sample_folder("Work"), sample_folder("Workstuff")];
        let names = descendant_imap_names(&folders, "Work", "");
        assert_eq!(names, vec!["Work".to_string()]);
    }

    #[test]
    fn nested_imap_names_exclude_parent_deepest_first() {
        let folders = vec![
            sample_folder("Trash"),
            sample_folder("Trash/A"),
            sample_folder("Trash/A/B"),
            sample_folder("Trash/C"),
            sample_folder("Archive"),
        ];
        assert_eq!(
            nested_imap_names(&folders, "Trash", "/"),
            vec![
                "Trash/A/B".to_string(),
                "Trash/A".to_string(),
                "Trash/C".to_string(),
            ]
        );
        assert!(nested_imap_names(&folders, "Trash", "").is_empty());
        assert!(nested_imap_names(&folders, "Archive", "/").is_empty());
    }

    #[test]
    fn rename_imap_map_empty_delimiter_does_not_prefix_match() {
        let names = vec!["Work".to_string(), "Workstuff".to_string()];
        let pairs = rename_imap_map("Work", "Archive", "", &names);
        assert_eq!(pairs, vec![("Work".to_string(), "Archive".to_string())]);
    }

    #[test]
    fn folder_delimiter_for_parent_uses_inbox_then_slash() {
        let mut inbox = sample_folder("INBOX");
        inbox.special = SpecialUse::Inbox;
        inbox.delimiter = ".".to_string();
        let child = sample_folder("INBOX.Projects");
        let folders = vec![inbox, child];
        assert_eq!(folder_delimiter_for_parent(&folders, None), ".");
        assert_eq!(
            folder_delimiter_for_parent(&folders, Some(FolderId(1))),
            "."
        );
        assert_eq!(folder_delimiter_for_parent(&[], None), "/");
    }

    #[test]
    fn folder_is_mutable_requires_selectable_other() {
        let mut folder = sample_folder("Projects");
        assert!(folder_is_mutable(&folder));

        folder.special = SpecialUse::Inbox;
        assert!(!folder_is_mutable(&folder));

        folder.special = SpecialUse::Other;
        folder.selectable = false;
        assert!(!folder_is_mutable(&folder));
    }
}
