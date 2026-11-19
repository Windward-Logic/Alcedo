//! Window status bar: sync, folder, counts, and network.

use alcedo_i18n::{ntr, tr, trf, trs};
use std::time::Duration;

use chrono::{DateTime, Utc};
use relm4::adw;
use relm4::gtk;
use relm4::gtk::gio;
use relm4::gtk::prelude::*;

use crate::folder_bar::everything_title;

/// Which collection the status bar describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusFolder {
    None,
    UnifiedInbox,
    Search {
        scope: String,
    },
    /// An extension view: its title, then optional detail.
    Extension {
        title: String,
        detail: String,
    },
    Folder {
        name: String,
        account: Option<String>,
    },
}

/// Message counts for the current view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderStats {
    pub total: u32,
    pub unread: u32,
    pub starred: u32,
}

/// Selection summary for the status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionInfo {
    Position { index: u32, total: u32 },
    Count(u32),
}

/// Bottom status bar widgets.
#[derive(Clone)]
pub struct StatusBar {
    pub root: gtk::Box,
    sync_spinner: adw::Spinner,
    sync_label: gtk::Label,
    sync_btn: gtk::Button,
    folder_label: gtk::Label,
    stats_label: gtk::Label,
    selection_box: gtk::Box,
    selection_label: gtk::Label,
    last_sync_label: gtk::Label,
    network_icon: gtk::Image,
}

/// Build the main-window status bar.
pub fn build_status_bar() -> StatusBar {
    let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    root.add_css_class("alcedo-status-bar");
    root.add_css_class("toolbar");
    root.set_hexpand(true);

    let sync_spinner = adw::Spinner::builder()
        .tooltip_text(tr!("Syncing"))
        .visible(false)
        .build();
    crate::a11y::set_label(&sync_spinner, &tr!("Syncing"));
    sync_spinner.add_css_class("alcedo-sync-spinner");

    let sync_label = gtk::Label::builder()
        .css_classes(["caption", "dim-label"])
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(28)
        .build();

    let sync_inner = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    sync_inner.set_valign(gtk::Align::Center);
    sync_inner.append(&sync_spinner);
    sync_inner.append(&sync_label);
    let sync_btn = gtk::Button::builder()
        .css_classes(["flat", "alcedo-status-activity"])
        .has_frame(false)
        .focus_on_click(false)
        .tooltip_text(tr!("Activity log"))
        .action_name("win.activity-log")
        .child(&sync_inner)
        .build();

    let folder_label = gtk::Label::builder()
        .css_classes(["caption"])
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(32)
        .build();

    let stats_label = gtk::Label::builder()
        .css_classes(["caption", "dim-label"])
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .hexpand(true)
        .build();

    let selection_label = gtk::Label::builder()
        .css_classes(["caption", "dim-label"])
        .build();
    let selection_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    selection_box.set_visible(false);
    selection_box.append(&selection_label);
    selection_box.append(&separator());

    let last_sync_label = gtk::Label::builder()
        .css_classes(["caption", "dim-label"])
        .build();
    let network_icon = gtk::Image::from_icon_name("network-offline-symbolic");
    network_icon.set_pixel_size(12);

    let start = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    start.set_valign(gtk::Align::Center);
    start.set_hexpand(true);
    start.append(&sync_btn);
    start.append(&separator());
    let folder_icon = gtk::Image::from_icon_name("folder-symbolic");
    folder_icon.set_pixel_size(12);
    folder_icon.add_css_class("dim-label");
    start.append(&folder_icon);
    start.append(&folder_label);
    start.append(&separator());
    start.append(&stats_label);

    // Syncing is a list-header button; here the last sync is plain text.
    // The network state is an icon whose words live in its tooltip.
    let end = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    end.set_halign(gtk::Align::End);
    end.set_valign(gtk::Align::Center);
    end.append(&selection_box);
    end.append(&last_sync_label);
    end.append(&network_icon);

    root.append(&start);
    root.append(&end);

    let bar = StatusBar {
        root,
        sync_spinner,
        sync_label,
        sync_btn,
        folder_label,
        stats_label,
        selection_box,
        selection_label,
        last_sync_label,
        network_icon,
    };
    bar.apply_network(&gio::NetworkMonitor::default());
    bar
}

fn separator() -> gtk::Separator {
    let sep = gtk::Separator::new(gtk::Orientation::Vertical);
    sep.add_css_class("alcedo-status-sep");
    sep
}

impl StatusBar {
    /// Update the sync spinner and status text.
    pub fn set_sync(&self, raw: Option<&str>, busy: bool, account_email: Option<&str>) {
        self.sync_spinner.set_visible(busy);
        let status = format_sync_status(raw);
        self.sync_label.set_text(&status);
        self.sync_label.set_tooltip_text(Some(&status));
        self.sync_btn
            .set_tooltip_text(Some(&format_sync_activity_tooltip(account_email, raw)));
    }

    /// Update the current-folder label.
    pub fn set_folder(&self, folder: &StatusFolder) {
        let text = format_folder(folder);
        self.folder_label.set_text(&text);
        self.folder_label.set_tooltip_text(Some(&text));
    }

    /// Update message counts for the current view.
    pub fn set_stats(&self, stats: FolderStats) {
        let text = format_stats(stats);
        self.stats_label.set_text(&text);
        self.stats_label.set_tooltip_text(Some(&text));
    }

    /// Update selection summary for the current list selection.
    pub fn set_selection(&self, info: Option<SelectionInfo>) {
        match info.and_then(format_selection_info) {
            Some(text) => {
                self.selection_label.set_text(&text);
                self.selection_box.set_visible(true);
            }
            None => {
                self.selection_label.set_text("");
                self.selection_box.set_visible(false);
            }
        }
    }

    /// Update relative last-sync time.
    pub fn set_last_sync(&self, at: Option<DateTime<Utc>>, now: DateTime<Utc>) {
        let text = match at {
            Some(at) => format_last_sync(at, now),
            None => tr!("Never synced"),
        };
        self.last_sync_label.set_text(&text);
    }

    /// Apply GIO network monitor state.
    pub fn apply_network(&self, monitor: &gio::NetworkMonitor) {
        let (icon, text) = format_network(monitor.is_network_available(), monitor.connectivity());
        self.network_icon.set_icon_name(Some(icon));
        crate::a11y::label_icon_control(&self.network_icon, text);
    }
}

/// Activity-log button tooltip when the selected account has sync status.
pub fn format_sync_activity_tooltip(email: Option<&str>, raw: Option<&str>) -> String {
    let status = format_sync_status(raw);
    if let Some(email) = email.filter(|value| !value.is_empty())
        && !status.is_empty()
    {
        // TRANSLATORS: tooltip; {status} is a sync state such as "Up to date".
        return trf!(
            "{email} — {status}. Activity log",
            email = email,
            status = status
        );
    }
    tr!("Activity log")
}

/// Human-readable sync status (IDLE becomes Up to date).
///
/// The raw status is a fixed English token from the sync worker (or the UI's
/// own "Mock data" / "No accounts"), which `sync_is_busy` also compares, so it
/// is translated only here. Progress text the worker writes is already
/// translated where it is made.
pub fn format_sync_status(raw: Option<&str>) -> String {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => tr!("Ready"),
        Some(s) if s.eq_ignore_ascii_case("IDLE") => tr!("Up to date"),
        Some(s) if s.eq_ignore_ascii_case("Polling") => tr!("Polling"),
        Some(s) if s.eq_ignore_ascii_case("Sign-in required") => tr!("Sign-in required"),
        Some(s) if s.eq_ignore_ascii_case("Mock data") => tr!("Mock data"),
        Some(s) if s.eq_ignore_ascii_case("No accounts") => tr!("No accounts"),
        Some(s) => s.to_string(),
    }
}

/// Minimum time the sync icon and last-sync caption stay animating.
pub const MIN_SYNC_ANIMATION: Duration = Duration::from_secs(1);

/// True when the worker is busy or the minimum animation window is still open.
pub fn sync_chrome_busy(raw: Option<&str>, anim_hold: bool) -> bool {
    anim_hold || sync_is_busy(raw)
}

/// True when the worker is actively connecting or fetching.
pub fn sync_is_busy(raw: Option<&str>) -> bool {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s)
            if s.eq_ignore_ascii_case("IDLE")
                || s.eq_ignore_ascii_case("Polling")
                || s.eq_ignore_ascii_case("Sign-in required")
                || s.eq_ignore_ascii_case("Mock data")
                || s.eq_ignore_ascii_case("No accounts")
                || s.eq_ignore_ascii_case("Ready")
                || s.eq_ignore_ascii_case("Up to date") =>
        {
            false
        }
        Some(_) => true,
        None => false,
    }
}

/// Folder title for the status bar.
pub fn format_folder(folder: &StatusFolder) -> String {
    match folder {
        StatusFolder::None => String::new(),
        StatusFolder::UnifiedInbox => everything_title(),
        // TRANSLATORS: status bar; {scope} is a folder name or "All mail".
        StatusFolder::Search { scope } => trf!("Search · {scope}", scope = scope),
        StatusFolder::Extension { title, detail } if detail.is_empty() => title.clone(),
        StatusFolder::Extension { title, detail } => format!("{title} · {detail}"),
        StatusFolder::Folder {
            name,
            account: Some(account),
        } => format!("{name} · {account}"),
        StatusFolder::Folder {
            name,
            account: None,
        } => name.clone(),
    }
}

/// Counts line: messages, unread, optional starred.
pub fn format_stats(stats: FolderStats) -> String {
    if stats.total == 0 {
        return tr!("No messages");
    }
    let messages = ntr!("{n} message", "{n} messages", stats.total);
    let unread = match stats.unread {
        0 => tr!("no unread"),
        n => ntr!("{n} unread", "{n} unread", n),
    };
    if stats.starred == 0 {
        format!("{messages} · {unread}")
    } else {
        let starred = ntr!("{n} starred", "{n} starred", stats.starred);
        format!("{messages} · {unread} · {starred}")
    }
}

/// 1-based "n of m" when a single message is selected.
pub fn format_selection(index: u32, total: u32) -> Option<String> {
    if total == 0 || index == 0 || index > total {
        None
    } else {
        // TRANSLATORS: position of the selected message, e.g. "3 of 40".
        Some(trf!("{index} of {total}", index = index, total = total))
    }
}

/// Human-readable selection summary for the status bar.
pub fn format_selection_info(info: SelectionInfo) -> Option<String> {
    match info {
        SelectionInfo::Position { index, total } => format_selection(index, total),
        SelectionInfo::Count(0) => None,
        SelectionInfo::Count(count) => Some(ntr!("{n} selected", "{n} selected", count)),
    }
}

/// Relative last-sync caption.
pub fn format_last_sync(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - at).num_seconds().max(0);
    if secs < 60 {
        tr!("Last sync just now")
    } else if secs < 3600 {
        // TRANSLATORS: {n} minutes; keep it short, it sits in the status bar.
        ntr!("Last sync {n}m ago", "Last sync {n}m ago", secs / 60)
    } else if secs < 86_400 {
        ntr!("Last sync {n}h ago", "Last sync {n}h ago", secs / 3600)
    } else {
        ntr!("Last sync {n}d ago", "Last sync {n}d ago", secs / 86_400)
    }
}

/// Icon name and caption for network connectivity.
pub fn format_network(
    available: bool,
    connectivity: gio::NetworkConnectivity,
) -> (&'static str, &'static str) {
    if !available {
        return ("network-offline-symbolic", trs!("Offline"));
    }
    match connectivity {
        gio::NetworkConnectivity::Full => ("network-transmit-receive-symbolic", trs!("Online")),
        gio::NetworkConnectivity::Limited => ("network-error-symbolic", trs!("Limited")),
        gio::NetworkConnectivity::Portal => (
            "network-cellular-acquiring-symbolic",
            trs!("Captive portal"),
        ),
        gio::NetworkConnectivity::Local => ("network-offline-symbolic", trs!("Local only")),
        _ => ("network-offline-symbolic", trs!("Offline")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn format_sync_activity_tooltip_includes_email_and_status() {
        assert_eq!(
            format_sync_activity_tooltip(Some("ada@example.com"), Some("IDLE")),
            "ada@example.com — Up to date. Activity log"
        );
        assert_eq!(
            format_sync_activity_tooltip(None, Some("IDLE")),
            "Activity log"
        );
        assert_eq!(
            format_sync_activity_tooltip(Some(""), Some("IDLE")),
            "Activity log"
        );
    }

    #[test]
    fn format_sync_status_maps_idle_and_empty() {
        assert_eq!(format_sync_status(None), "Ready");
        assert_eq!(format_sync_status(Some("")), "Ready");
        assert_eq!(format_sync_status(Some("IDLE")), "Up to date");
        assert_eq!(format_sync_status(Some("Polling")), "Polling");
        assert_eq!(format_sync_status(Some("Syncing Inbox…")), "Syncing Inbox…");
        assert!(!sync_is_busy(Some("IDLE")));
        assert!(!sync_is_busy(Some("Sign-in required")));
        assert!(!sync_is_busy(Some("Mock data")));
        assert!(!sync_is_busy(Some("No accounts")));
        assert!(sync_is_busy(Some("Connecting…")));
        assert!(sync_chrome_busy(Some("IDLE"), true));
        assert!(!sync_chrome_busy(Some("IDLE"), false));
        assert!(sync_chrome_busy(Some("Connecting…"), false));
        assert_eq!(MIN_SYNC_ANIMATION.as_secs(), 1);
    }

    #[test]
    fn format_folder_extension_title_and_detail() {
        assert_eq!(
            format_folder(&StatusFolder::Extension {
                title: "Related".into(),
                detail: "Hello".into()
            }),
            "Related · Hello"
        );
        assert_eq!(
            format_folder(&StatusFolder::Extension {
                title: "Related".into(),
                detail: String::new()
            }),
            "Related"
        );
    }

    #[test]
    fn format_folder_includes_account_when_present() {
        assert_eq!(format_folder(&StatusFolder::UnifiedInbox), "Everything");
        assert_eq!(
            format_folder(&StatusFolder::Search {
                scope: "Inbox".into()
            }),
            "Search · Inbox"
        );
        assert_eq!(
            format_folder(&StatusFolder::Folder {
                name: "Inbox".into(),
                account: Some("me@example.com".into()),
            }),
            "Inbox · me@example.com"
        );
        assert_eq!(
            format_folder(&StatusFolder::Folder {
                name: "Sent".into(),
                account: None,
            }),
            "Sent"
        );
    }

    #[test]
    fn format_stats_pluralizes_and_omits_starred_when_zero() {
        assert_eq!(
            format_stats(FolderStats {
                total: 0,
                unread: 0,
                starred: 0
            }),
            "No messages"
        );
        assert_eq!(
            format_stats(FolderStats {
                total: 1,
                unread: 1,
                starred: 0
            }),
            "1 message · 1 unread"
        );
        assert_eq!(
            format_stats(FolderStats {
                total: 12,
                unread: 3,
                starred: 2
            }),
            "12 messages · 3 unread · 2 starred"
        );
    }

    #[test]
    fn format_selection_is_one_based_and_bounded() {
        assert_eq!(format_selection(1, 12), Some("1 of 12".into()));
        assert_eq!(format_selection(0, 12), None);
        assert_eq!(format_selection(13, 12), None);
        assert_eq!(format_selection(1, 0), None);
    }

    #[test]
    fn format_selection_info_handles_single_and_multi() {
        assert_eq!(
            format_selection_info(SelectionInfo::Position {
                index: 2,
                total: 10
            }),
            Some("2 of 10".into())
        );
        assert_eq!(
            format_selection_info(SelectionInfo::Count(3)),
            Some("3 selected".into())
        );
        assert_eq!(
            format_selection_info(SelectionInfo::Count(1)),
            Some("1 selected".into())
        );
        assert_eq!(format_selection_info(SelectionInfo::Count(0)), None);
    }

    #[test]
    fn format_last_sync_uses_relative_buckets() {
        let now = Utc.with_ymd_and_hms(2026, 8, 15, 12, 0, 0).unwrap();
        assert_eq!(
            format_last_sync(now - chrono::Duration::seconds(10), now),
            "Last sync just now"
        );
        assert_eq!(
            format_last_sync(now - chrono::Duration::minutes(12), now),
            "Last sync 12m ago"
        );
        assert_eq!(
            format_last_sync(now - chrono::Duration::hours(3), now),
            "Last sync 3h ago"
        );
        assert_eq!(
            format_last_sync(now - chrono::Duration::days(2), now),
            "Last sync 2d ago"
        );
    }
}
