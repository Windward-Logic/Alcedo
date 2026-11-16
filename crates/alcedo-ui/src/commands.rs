//! Command palette catalog, fuzzy matching, and keyboard shortcut overrides.

use std::collections::HashMap;

use crate::menu::{
    ACTION_ABOUT, ACTION_ACTIVITY_LOG, ACTION_ADD_ACCOUNT, ACTION_ARCHIVE,
    ACTION_CHOOSE_COPY_FOLDER, ACTION_CHOOSE_MOVE_FOLDER, ACTION_COMMAND_PALETTE, ACTION_COMPOSE,
    ACTION_CONTACTS, ACTION_CREATE_RULE, ACTION_DELETE, ACTION_DELETE_PERMANENTLY,
    ACTION_EDIT_FOLDER, ACTION_FORWARD, ACTION_GO_UNIFIED, ACTION_HELP, ACTION_KEYBOARD_SHORTCUTS,
    ACTION_MARK_NOT_SPAM, ACTION_MARK_READ, ACTION_MARK_SPAM, ACTION_MARK_UNREAD,
    ACTION_MESSAGE_RULES, ACTION_NEW_FOLDER, ACTION_PREFERENCES, ACTION_QUIT, ACTION_REPLY,
    ACTION_REPLY_ALL, ACTION_SELECT_ALL, ACTION_SHOW_MAIL, ACTION_STAR, ACTION_SYNC_FOLDER,
    ACTION_TOGGLE_PREVIEW, ACTION_TOGGLE_RAIL, ACTION_UNSTAR,
};
use crate::settings;

/// Palette / keyboard prefs grouping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CommandCategory {
    Mail,
    GoTo,
    Compose,
    Search,
    View,
}

impl CommandCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mail => "Mail",
            Self::GoTo => "Go to",
            Self::Compose => "Compose",
            Self::Search => "Search",
            Self::View => "View",
        }
    }

    pub fn palette_order(self) -> u8 {
        match self {
            Self::Mail => 0,
            Self::GoTo => 1,
            Self::Compose => 2,
            Self::Search => 3,
            Self::View => 4,
        }
    }
}

/// Where a shortcut is installed and how prefs describe it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutScope {
    /// `gtk_application_set_accels_for_action`.
    AppAccel,
    /// Single key on the window `ShortcutController` when the message list is focused.
    ListFocus,
    /// Window `ShortcutController` chord (e.g. Ctrl+K).
    PaletteChord,
    /// Window `ShortcutController` chord with typed-input gate (reply, forward).
    WindowChord,
}

/// Static command metadata for the palette and keyboard preferences.
#[derive(Clone, Copy, Debug)]
pub struct CommandDef {
    /// Override map key (unique).
    pub id: &'static str,
    /// GIO action activated from the palette, if any.
    pub gio_action: Option<&'static str>,
    pub label: &'static str,
    pub category: CommandCategory,
    pub default_accel: Option<&'static str>,
    /// Extra shortcut shown in the palette (e.g. list-focus key).
    pub extra_display_accel: Option<&'static str>,
    pub scope: ShortcutScope,
    pub icon: &'static str,
    pub aliases: &'static [&'static str],
    /// Listed in the static command palette catalog.
    pub in_palette: bool,
}

const SEARCH_OPS: &[(&str, &str)] = &[
    ("search.op.from", "from:"),
    ("search.op.to", "to:"),
    ("search.op.subject", "subject:"),
    ("search.op.folder", "folder:"),
    ("search.op.in", "in:"),
    ("search.op.after", "after:"),
    ("search.op.before", "before:"),
    ("search.op.larger", "larger:"),
    ("search.op.smaller", "smaller:"),
    ("search.op.is-unread", "is:unread"),
    ("search.op.is-read", "is:read"),
    ("search.op.is-starred", "is:starred"),
    ("search.op.is-unstarred", "is:unstarred"),
    ("search.op.has-attachment", "has:attachment"),
    ("search.op.in-anywhere", "in:anywhere"),
];

/// Tokens inserted by the command palette and search-field autocomplete.
pub fn search_operator_tokens() -> impl Iterator<Item = &'static str> {
    SEARCH_OPS.iter().map(|(_, token)| *token)
}

/// Full keyboard / palette catalog.
pub fn command_catalog() -> &'static [CommandDef] {
    use CommandCategory::*;
    use ShortcutScope::*;

    static CATALOG: &[CommandDef] = &[
        CommandDef {
            id: ACTION_SELECT_ALL,
            gio_action: Some(ACTION_SELECT_ALL),
            label: "Select All",
            category: Mail,
            default_accel: Some("<Primary>a"),
            extra_display_accel: None,
            scope: WindowChord,
            icon: "edit-select-all-symbolic",
            aliases: &[],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_REPLY,
            gio_action: Some(ACTION_REPLY),
            label: "Reply",
            category: Mail,
            default_accel: Some("<Primary>r"),
            extra_display_accel: Some("r"),
            scope: WindowChord,
            icon: "mail-reply-sender-symbolic",
            aliases: &["respond"],
            in_palette: true,
        },
        CommandDef {
            id: "list.win.mail-reply",
            gio_action: Some(ACTION_REPLY),
            label: "Reply",
            category: Mail,
            default_accel: Some("r"),
            extra_display_accel: None,
            scope: ListFocus,
            icon: "mail-reply-sender-symbolic",
            aliases: &[],
            in_palette: false,
        },
        CommandDef {
            id: ACTION_REPLY_ALL,
            gio_action: Some(ACTION_REPLY_ALL),
            label: "Reply All",
            category: Mail,
            default_accel: Some("<Primary><Shift>r"),
            extra_display_accel: Some("a"),
            scope: WindowChord,
            icon: "mail-reply-all-symbolic",
            aliases: &[],
            in_palette: true,
        },
        CommandDef {
            id: "list.win.mail-reply-all",
            gio_action: Some(ACTION_REPLY_ALL),
            label: "Reply All",
            category: Mail,
            default_accel: Some("a"),
            extra_display_accel: None,
            scope: ListFocus,
            icon: "mail-reply-all-symbolic",
            aliases: &[],
            in_palette: false,
        },
        CommandDef {
            id: ACTION_FORWARD,
            gio_action: Some(ACTION_FORWARD),
            label: "Forward",
            category: Mail,
            default_accel: Some("<Primary>l"),
            extra_display_accel: Some("f"),
            scope: WindowChord,
            icon: "mail-forward-symbolic",
            aliases: &[],
            in_palette: true,
        },
        CommandDef {
            id: "list.win.mail-forward",
            gio_action: Some(ACTION_FORWARD),
            label: "Forward",
            category: Mail,
            default_accel: Some("f"),
            extra_display_accel: None,
            scope: ListFocus,
            icon: "mail-forward-symbolic",
            aliases: &[],
            in_palette: false,
        },
        CommandDef {
            id: ACTION_MARK_READ,
            gio_action: Some(ACTION_MARK_READ),
            label: "Mark as Read",
            category: Mail,
            default_accel: Some("<Shift>i"),
            extra_display_accel: None,
            scope: ListFocus,
            icon: "mail-read-symbolic",
            aliases: &["read"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_MARK_UNREAD,
            gio_action: Some(ACTION_MARK_UNREAD),
            label: "Mark as Unread",
            category: Mail,
            default_accel: Some("u"),
            extra_display_accel: None,
            scope: ListFocus,
            icon: "mail-unread-symbolic",
            aliases: &["unread"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_STAR,
            gio_action: Some(ACTION_STAR),
            label: "Star",
            category: Mail,
            default_accel: Some("s"),
            extra_display_accel: None,
            scope: ListFocus,
            icon: "starred-symbolic",
            aliases: &["favorite"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_UNSTAR,
            gio_action: Some(ACTION_UNSTAR),
            label: "Unstar",
            category: Mail,
            default_accel: None,
            extra_display_accel: None,
            scope: ListFocus,
            icon: "non-starred-symbolic",
            aliases: &[],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_MARK_SPAM,
            gio_action: Some(ACTION_MARK_SPAM),
            label: "Mark as Spam",
            category: Mail,
            default_accel: None,
            extra_display_accel: None,
            scope: ListFocus,
            icon: "mail-mark-junk-symbolic",
            aliases: &["junk"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_MARK_NOT_SPAM,
            gio_action: Some(ACTION_MARK_NOT_SPAM),
            label: "Mark as Not Spam",
            category: Mail,
            default_accel: None,
            extra_display_accel: None,
            scope: ListFocus,
            icon: "mail-mark-not-junk-symbolic",
            aliases: &[],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_CHOOSE_MOVE_FOLDER,
            gio_action: Some(ACTION_CHOOSE_MOVE_FOLDER),
            label: "Move to Folder…",
            category: Mail,
            default_accel: Some("v"),
            extra_display_accel: None,
            scope: ListFocus,
            icon: "folder-symbolic",
            aliases: &["move"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_CHOOSE_COPY_FOLDER,
            gio_action: Some(ACTION_CHOOSE_COPY_FOLDER),
            label: "Copy to Folder…",
            category: Mail,
            default_accel: None,
            extra_display_accel: None,
            scope: ListFocus,
            icon: "edit-copy-symbolic",
            aliases: &["copy"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_CREATE_RULE,
            gio_action: Some(ACTION_CREATE_RULE),
            label: "Create Rule…",
            category: Mail,
            default_accel: None,
            extra_display_accel: None,
            scope: ListFocus,
            icon: "filter-symbolic",
            aliases: &["rule"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_ARCHIVE,
            gio_action: Some(ACTION_ARCHIVE),
            label: "Archive",
            category: Mail,
            default_accel: Some("e"),
            extra_display_accel: None,
            scope: ListFocus,
            icon: "mail-archive-symbolic",
            aliases: &["file"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_DELETE,
            gio_action: Some(ACTION_DELETE),
            label: "Delete",
            category: Mail,
            default_accel: Some("numbersign"),
            extra_display_accel: None,
            scope: ListFocus,
            icon: "user-trash-symbolic",
            aliases: &["trash"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_DELETE_PERMANENTLY,
            gio_action: Some(ACTION_DELETE_PERMANENTLY),
            label: "Delete Permanently…",
            category: Mail,
            default_accel: Some("<Shift>Delete"),
            extra_display_accel: None,
            scope: ListFocus,
            icon: "edit-delete-symbolic",
            aliases: &["permanent", "expunge"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_SYNC_FOLDER,
            gio_action: Some(ACTION_SYNC_FOLDER),
            label: "Sync Folder",
            category: Mail,
            default_accel: Some("F5"),
            extra_display_accel: None,
            scope: AppAccel,
            icon: "view-refresh-symbolic",
            aliases: &["refresh"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_ACTIVITY_LOG,
            gio_action: Some(ACTION_ACTIVITY_LOG),
            label: "Activity Log",
            category: View,
            default_accel: None,
            extra_display_accel: None,
            scope: AppAccel,
            icon: "document-open-recent-symbolic",
            aliases: &["log", "activity"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_GO_UNIFIED,
            gio_action: Some(ACTION_GO_UNIFIED),
            label: "Unified Inbox",
            category: GoTo,
            default_accel: None,
            extra_display_accel: None,
            scope: ListFocus,
            icon: "mail-inbox-symbolic",
            aliases: &["inbox", "unified"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_SHOW_MAIL,
            gio_action: Some(ACTION_SHOW_MAIL),
            label: "Mail",
            category: GoTo,
            default_accel: None,
            extra_display_accel: None,
            scope: ListFocus,
            icon: "mail-unread-symbolic",
            aliases: &[],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_CONTACTS,
            gio_action: Some(ACTION_CONTACTS),
            label: "Contacts",
            category: GoTo,
            default_accel: None,
            extra_display_accel: None,
            scope: ListFocus,
            icon: "contact-new-symbolic",
            aliases: &[],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_PREFERENCES,
            gio_action: Some(ACTION_PREFERENCES),
            label: "Preferences",
            category: GoTo,
            default_accel: Some("<Primary>comma"),
            extra_display_accel: None,
            scope: AppAccel,
            icon: "preferences-system-symbolic",
            aliases: &["settings"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_COMPOSE,
            gio_action: Some(ACTION_COMPOSE),
            label: "New Message",
            category: Compose,
            default_accel: Some("<Primary>n"),
            extra_display_accel: None,
            scope: AppAccel,
            icon: "mail-message-new-symbolic",
            aliases: &["compose", "new"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_COMMAND_PALETTE,
            gio_action: Some(ACTION_COMMAND_PALETTE),
            label: "Command Palette",
            category: View,
            default_accel: Some("<Primary>k"),
            extra_display_accel: Some("k"),
            scope: PaletteChord,
            icon: "edit-find-symbolic",
            aliases: &["commands", "palette"],
            in_palette: true,
        },
        CommandDef {
            id: "list.app.command-palette",
            gio_action: Some(ACTION_COMMAND_PALETTE),
            label: "Command Palette",
            category: View,
            default_accel: Some("k"),
            extra_display_accel: None,
            scope: ListFocus,
            icon: "edit-find-symbolic",
            aliases: &[],
            in_palette: false,
        },
        CommandDef {
            id: ACTION_TOGGLE_RAIL,
            gio_action: Some(ACTION_TOGGLE_RAIL),
            label: "Show app rail",
            category: View,
            default_accel: Some("F9"),
            extra_display_accel: None,
            scope: AppAccel,
            icon: "sidebar-show-symbolic",
            aliases: &["rail"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_TOGGLE_PREVIEW,
            gio_action: Some(ACTION_TOGGLE_PREVIEW),
            label: "Show message previews",
            category: View,
            default_accel: None,
            extra_display_accel: None,
            scope: ListFocus,
            icon: "view-list-symbolic",
            aliases: &["preview"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_MESSAGE_RULES,
            gio_action: Some(ACTION_MESSAGE_RULES),
            label: "Message Rules",
            category: View,
            default_accel: None,
            extra_display_accel: None,
            scope: ListFocus,
            icon: "filter-symbolic",
            aliases: &["rules"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_NEW_FOLDER,
            gio_action: Some(ACTION_NEW_FOLDER),
            label: "New Folder…",
            category: View,
            default_accel: None,
            extra_display_accel: None,
            scope: ListFocus,
            icon: "folder-new-symbolic",
            aliases: &[],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_EDIT_FOLDER,
            gio_action: Some(ACTION_EDIT_FOLDER),
            label: "Edit Folder…",
            category: View,
            default_accel: None,
            extra_display_accel: None,
            scope: ListFocus,
            icon: "folder-symbolic",
            aliases: &[],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_ADD_ACCOUNT,
            gio_action: Some(ACTION_ADD_ACCOUNT),
            label: "Add Account",
            category: View,
            default_accel: None,
            extra_display_accel: None,
            scope: ListFocus,
            icon: "list-add-symbolic",
            aliases: &[],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_KEYBOARD_SHORTCUTS,
            gio_action: Some(ACTION_KEYBOARD_SHORTCUTS),
            label: "Keyboard Shortcuts",
            category: View,
            default_accel: Some("<Primary>question"),
            extra_display_accel: None,
            scope: AppAccel,
            icon: "input-keyboard-symbolic",
            aliases: &["shortcuts", "keys"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_HELP,
            gio_action: Some(ACTION_HELP),
            label: "Help",
            category: View,
            default_accel: Some("F1"),
            extra_display_accel: None,
            scope: AppAccel,
            icon: "help-browser-symbolic",
            aliases: &["contents", "documentation"],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_ABOUT,
            gio_action: Some(ACTION_ABOUT),
            label: "About",
            category: View,
            default_accel: None,
            extra_display_accel: None,
            scope: ListFocus,
            icon: "help-about-symbolic",
            aliases: &[],
            in_palette: true,
        },
        CommandDef {
            id: ACTION_QUIT,
            gio_action: Some(ACTION_QUIT),
            label: "Quit",
            category: View,
            default_accel: Some("<Primary>q"),
            extra_display_accel: None,
            scope: AppAccel,
            icon: "application-exit-symbolic",
            aliases: &["exit"],
            in_palette: true,
        },
    ];

    CATALOG
}

/// Search-operator rows appended after static catalog entries.
pub fn search_operator_defs() -> &'static [CommandDef] {
    use CommandCategory::Search;
    use ShortcutScope::ListFocus;

    static OPS: std::sync::OnceLock<Vec<CommandDef>> = std::sync::OnceLock::new();
    OPS.get_or_init(|| {
        SEARCH_OPS
            .iter()
            .map(|(id, token)| CommandDef {
                id,
                gio_action: None,
                label: token,
                category: Search,
                default_accel: None,
                extra_display_accel: None,
                scope: ListFocus,
                icon: "edit-find-symbolic",
                aliases: &[],
                in_palette: true,
            })
            .collect()
    })
    .as_slice()
}

pub fn catalog_entry(id: &str) -> Option<&'static CommandDef> {
    command_catalog()
        .iter()
        .chain(search_operator_defs())
        .find(|entry| entry.id == id)
}

pub fn encode_shortcut_overrides(map: &std::collections::HashMap<String, String>) -> Vec<String> {
    settings::encode_keyboard_shortcut_overrides(map)
}

pub fn decode_shortcut_overrides(
    entries: impl IntoIterator<Item = impl AsRef<str>>,
) -> HashMap<String, String> {
    settings::decode_keyboard_shortcut_overrides(entries)
}

pub fn effective_accel(id: &str) -> Option<String> {
    let overrides = settings::keyboard_shortcut_overrides();
    if let Some(trigger) = overrides.get(id) {
        return Some(trigger.clone());
    }
    catalog_entry(id).and_then(|entry| entry.default_accel.map(str::to_string))
}

pub fn display_accel_for(entry: &CommandDef) -> Option<String> {
    let primary = effective_accel(entry.id)?;
    if let Some(extra) = entry.extra_display_accel {
        Some(format!("{primary}  {extra}"))
    } else {
        Some(primary)
    }
}

pub fn format_accel_for_display(trigger: &str) -> String {
    match relm4::gtk::accelerator_parse(trigger) {
        Some((key, mods)) => relm4::gtk::accelerator_get_label(key, mods).to_string(),
        None => trigger.to_string(),
    }
}

/// Tooltip text. A shortcut hint is appended when `shortcut` is non-empty.
///
/// `shortcut` is already display text (`Ctrl+R`), not a GTK accelerator.
pub fn tooltip_with_shortcut(label: &str, shortcut: Option<&str>) -> String {
    match shortcut.map(str::trim).filter(|hint| !hint.is_empty()) {
        Some(hint) => format!("{label} ({hint})"),
        None => label.to_string(),
    }
}

/// Tooltip for a catalog command, using the effective accelerator.
///
/// Widgets that show this text refresh when shortcut overrides change.
pub fn command_tooltip(id: &str, label: &str) -> String {
    shortcut_tooltip(
        label,
        effective_accel(id).as_deref(),
        format_accel_for_display,
    )
}

fn shortcut_tooltip(
    label: &str,
    accel: Option<&str>,
    format: impl FnOnce(&str) -> String,
) -> String {
    let display = accel.map(format);
    tooltip_with_shortcut(label, display.as_deref())
}

/// Shortcuts dialog built from [`command_catalog`], one section per category.
/// Built when opened, so it shows the accelerators in effect at that moment.
pub fn present_shortcuts_dialog(parent: &impl relm4::gtk::prelude::IsA<relm4::gtk::Widget>) {
    use relm4::adw::prelude::AdwDialogExt;

    let dialog = relm4::adw::ShortcutsDialog::new();
    let mut groups: Vec<(CommandCategory, Vec<&CommandDef>)> = Vec::new();
    for entry in command_catalog() {
        if effective_accel(entry.id).is_none() {
            continue;
        }
        if let Some((_, rows)) = groups
            .iter_mut()
            .find(|(category, _)| *category == entry.category)
        {
            rows.push(entry);
        } else {
            groups.push((entry.category, vec![entry]));
        }
    }
    groups.sort_by_key(|(category, _)| category.palette_order());
    for (category, rows) in groups {
        let section = relm4::adw::ShortcutsSection::new(Some(category.label()));
        for entry in rows {
            let Some(accel) = effective_accel(entry.id) else {
                continue;
            };
            let item = relm4::adw::ShortcutsItem::new(entry.label, &accel);
            let subtitle = scope_subtitle(entry.scope);
            if !subtitle.is_empty() {
                item.set_subtitle(subtitle);
            }
            section.add(item);
        }
        dialog.add(section);
    }
    dialog.present(Some(parent));
}

pub fn fuzzy_score(haystack: &str, needle: &str) -> Option<u32> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay = haystack.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    if hay == needle {
        return Some(1000);
    }
    if hay.starts_with(&needle) {
        return Some(900);
    }
    if let Some(index) = hay.find(&needle) {
        return Some(800 - index.min(100) as u32);
    }
    let mut hay_idx = hay.chars();
    let mut gap_penalty = 0u32;
    let mut last_match = None;
    for (needle_pos, ch) in needle.chars().enumerate() {
        let mut matched = false;
        for hay_ch in hay_idx.by_ref() {
            if hay_ch == ch {
                if let Some(last) = last_match {
                    gap_penalty += needle_pos.saturating_sub(last + 1) as u32;
                }
                last_match = Some(needle_pos);
                matched = true;
                break;
            }
        }
        if !matched {
            return None;
        }
    }
    Some(400 - gap_penalty.min(200))
}

pub fn command_haystack(entry: &CommandDef) -> String {
    let mut hay = entry.label.to_string();
    if !entry.aliases.is_empty() {
        hay.push(' ');
        hay.push_str(&entry.aliases.join(" "));
    }
    hay.push(' ');
    hay.push_str(entry.id);
    hay
}

pub fn filter_catalog<'a>(
    query: &str,
    items: impl IntoIterator<Item = &'a CommandDef>,
) -> Vec<(u32, &'a CommandDef)> {
    let needle = query.trim();
    let mut scored: Vec<(u32, &'a CommandDef)> = items
        .into_iter()
        .filter_map(|entry| {
            let score = fuzzy_score(&command_haystack(entry), needle)?;
            Some((score, entry))
        })
        .collect();
    scored.sort_by(|(a_score, a), (b_score, b)| {
        b_score.cmp(a_score).then_with(|| a.label.cmp(b.label))
    });
    scored
}

pub fn palette_static_commands(query: &str) -> Vec<&'static CommandDef> {
    let static_rows: Vec<&CommandDef> = command_catalog()
        .iter()
        .filter(|entry| entry.in_palette)
        .chain(search_operator_defs().iter())
        .collect();

    if query.trim().is_empty() {
        let mut rows = static_rows;
        rows.sort_by_key(|entry| (entry.category.palette_order(), entry.label));
        return rows;
    }

    filter_catalog(query, static_rows)
        .into_iter()
        .map(|(_, entry)| entry)
        .take(50)
        .collect()
}

/// Whether a palette row should be sensitive for a named GIO action.
pub fn command_row_sensitive(action_enabled: bool) -> bool {
    action_enabled
}

pub fn remappable_commands() -> Vec<&'static CommandDef> {
    command_catalog()
        .iter()
        .filter(|entry| entry.default_accel.is_some())
        .collect()
}

pub fn scope_subtitle(scope: ShortcutScope) -> &'static str {
    match scope {
        ShortcutScope::ListFocus => "When the message list is focused",
        ShortcutScope::PaletteChord => "Everywhere except compose and other text fields",
        ShortcutScope::AppAccel | ShortcutScope::WindowChord => "",
    }
}

pub fn accel_in_use(trigger: &str, except_id: &str) -> Option<&'static CommandDef> {
    let normalized = trigger.trim();
    if normalized.is_empty() {
        return None;
    }
    command_catalog()
        .iter()
        .filter(|entry| entry.default_accel.is_some())
        .find(|entry| {
            entry.id != except_id && effective_accel(entry.id).as_deref() == Some(normalized)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_empty_needle_scores_zero() {
        assert_eq!(fuzzy_score("Star", ""), Some(0));
    }

    #[test]
    fn fuzzy_prefix_beats_contains_beats_subsequence() {
        assert!(
            fuzzy_score("Star", "star").unwrap()
                > fuzzy_score("Mark as Unread", "star").unwrap_or(0)
        );
        assert!(
            fuzzy_score("Mark as Unread", "unread").unwrap()
                > fuzzy_score("Move to Folder…", "unread").unwrap_or(0)
        );
        assert!(
            fuzzy_score("Move to Folder…", "mtf").unwrap()
                > fuzzy_score("Mark as Unread", "mtf").unwrap_or(0)
        );
    }

    #[test]
    fn filter_catalog_omits_no_match() {
        let rows = filter_catalog(
            "zzznomatch",
            command_catalog().iter().filter(|e| e.in_palette),
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn empty_query_palette_category_order() {
        let rows = palette_static_commands("");
        let categories: Vec<_> = rows.iter().map(|row| row.category).collect();
        let mut last = 0u8;
        for category in categories {
            let order = category.palette_order();
            assert!(order >= last);
            last = order;
        }
        assert!(!rows.is_empty());
    }

    #[test]
    fn encode_decode_shortcut_override_round_trip() {
        let mut map = HashMap::new();
        map.insert(
            "app.command-palette".to_string(),
            "<Primary><Shift>p".to_string(),
        );
        let encoded = encode_shortcut_overrides(&map);
        let decoded = decode_shortcut_overrides(encoded);
        assert_eq!(decoded, map);
    }

    #[test]
    fn decode_skips_malformed_entries() {
        let decoded = decode_shortcut_overrides([
            "app.compose\u{1f}<Primary>n",
            "no-separator",
            "\u{1f}trigger-only",
            "empty-trigger\u{1f}",
        ]);
        assert_eq!(decoded.len(), 1);
        assert_eq!(
            decoded.get("app.compose").map(String::as_str),
            Some("<Primary>n")
        );
    }

    #[test]
    fn tooltip_with_shortcut_appends_hint() {
        assert_eq!(
            tooltip_with_shortcut("Reply", Some("Ctrl+R")),
            "Reply (Ctrl+R)"
        );
        assert_eq!(tooltip_with_shortcut("Reply", None), "Reply");
        assert_eq!(tooltip_with_shortcut("Reply", Some("  ")), "Reply");
        assert_eq!(tooltip_with_shortcut("Reply", Some("")), "Reply");
    }

    #[test]
    fn command_tooltip_uses_catalog_accel() {
        // Do not call `command_tooltip` here. It formats through GTK, and the
        // message-list tests own the GTK thread.
        assert_eq!(
            shortcut_tooltip("Reply", Some("<Primary>r"), |accel| format!("[{accel}]")),
            "Reply ([<Primary>r])"
        );
        assert_eq!(
            shortcut_tooltip("Sync Folder", None, |_| unreachable!()),
            "Sync Folder"
        );
        assert_eq!(
            shortcut_tooltip("Reply", Some("   "), str::to_string),
            "Reply"
        );
        assert_eq!(
            catalog_entry(ACTION_REPLY).and_then(|entry| entry.default_accel),
            Some("<Primary>r")
        );
        assert_eq!(
            catalog_entry(ACTION_SYNC_FOLDER).and_then(|entry| entry.default_accel),
            Some("F5")
        );
    }

    #[test]
    fn archive_default_accel_is_e() {
        assert_eq!(
            catalog_entry(ACTION_ARCHIVE).and_then(|entry| entry.default_accel),
            Some("e")
        );
        assert_eq!(
            catalog_entry(ACTION_MARK_READ).and_then(|entry| entry.default_accel),
            Some("<Shift>i")
        );
        assert_eq!(
            catalog_entry(ACTION_MARK_UNREAD).and_then(|entry| entry.default_accel),
            Some("u")
        );
        assert_eq!(
            catalog_entry(ACTION_KEYBOARD_SHORTCUTS).and_then(|entry| entry.default_accel),
            Some("<Primary>question")
        );
    }

    #[test]
    fn default_accelerators_unique_unless_same_action() {
        let mut owner: HashMap<&str, Option<&str>> = HashMap::new();
        for entry in command_catalog() {
            let Some(accel) = entry.default_accel else {
                continue;
            };
            if let Some(previous) = owner.get(accel) {
                assert_eq!(
                    *previous, entry.gio_action,
                    "default accelerator {accel} is shared by different actions ({})",
                    entry.id
                );
            } else {
                owner.insert(accel, entry.gio_action);
            }
        }
    }
}
