//! Adaptive mail chrome: overlay folders, single pane, and title buttons.

use std::cell::Cell;
use std::rc::Rc;

use alcedo_core::layout_policy::{
    HeaderTitleButtons, ReadingPanePlacement, WidthBreakpoints, adaptive_mail_layout,
    title_button_placement,
};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;

use crate::a11y;
use crate::commands;
use crate::composer::ComposerHeaderChrome;
use crate::icons;
use crate::menu::ACTION_COMPOSE;
use crate::reader::{refresh_reader_shortcut_tooltips, set_reader_reply_icon_only};
use crate::settings::{ReadingPane, folder_pane_width, reading_pane};

use super::{MailContentPane, MailModel};

pub(super) struct MailLayout {
    pub mail_layout: gtk::Stack,
    pub folder_overlay: adw::OverlaySplitView,
    pub folder_header: adw::HeaderBar,
    pub folder_start: gtk::Box,
    pub folder_compose: gtk::Button,
    pub list_header: adw::HeaderBar,
    pub list_start: gtk::Box,
    pub reader_header: adw::HeaderBar,
    pub reader_back: gtk::Button,
    pub composer_header: adw::HeaderBar,
    pub composer_chrome: ComposerHeaderChrome,
    pub folder_toggle: gtk::ToggleButton,
    pub list_compose: gtk::Button,
    pub menu_button: gtk::MenuButton,
    pub messages_placeholder: gtk::Box,
    pub content_placeholder: gtk::Box,
    pub overlay_mode: bool,
    /// Set while panes are reparented so a sidebar notification cannot hide them.
    pub sidebar_notify_suppress: Rc<Cell<bool>>,
    pub single_pane: bool,
    pub folders_user_visible: bool,
    pub single_show_detail: bool,
}

pub(super) fn build_folder_chrome() -> (adw::HeaderBar, gtk::Box, gtk::Button, gtk::MenuButton) {
    let header = adw::HeaderBar::new();
    header.set_centering_policy(adw::CenteringPolicy::Loose);
    header.set_title_widget(Some(&adw::WindowTitle::new("Alcedo", "")));
    header.set_show_start_title_buttons(true);
    header.set_show_end_title_buttons(false);

    let menu = crate::menu::build_primary_menu_button();
    let compose = gtk::Button::builder()
        .css_classes(["flat"])
        .action_name(ACTION_COMPOSE)
        .build();
    // Icon theme lookup misses this bundled name; paint it from gresource.
    icons::set_button_icon(&compose, icons::COMPOSE_SYMBOLIC, icons::TOOLBAR_PIXEL_SIZE);
    a11y::name_icon_control(
        &compose,
        "New Message",
        &commands::command_tooltip(ACTION_COMPOSE, "New Message"),
    );
    let start = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    start.append(&menu);
    start.append(&compose);
    header.pack_start(&start);
    (header, start, compose, menu)
}

impl MailModel {
    pub(super) fn apply_adaptive_layout(&mut self, applied: WidthBreakpoints) {
        let spec = adaptive_mail_layout(applied);
        self.layout.single_pane = spec.single_pane;
        self.layout.list_compose.set_visible(spec.list_compose);
        set_reader_reply_icon_only(&self.reader, spec.reply_icon_only);
        self.layout
            .composer_chrome
            .set_compact(spec.compact_composer);
        if spec.folders_overlay != self.layout.overlay_mode {
            self.set_overlay_mode(spec.folders_overlay);
        }
        if !spec.single_pane {
            self.layout.single_show_detail = false;
        }
        self.sync_single_pane();
        self.place_menu_button();
        self.apply_title_buttons();
    }

    pub(super) fn set_folders_visible(&mut self, visible: bool) {
        self.layout.folders_user_visible = visible;
        if self.layout.folder_toggle.is_active() != visible {
            self.layout.folder_toggle.set_active(visible);
        }
        a11y::label_icon_control(
            &self.layout.folder_toggle,
            if visible {
                "Hide Folders"
            } else {
                "Show Folders"
            },
        );
        if self.layout.overlay_mode {
            if self.layout.folder_overlay.shows_sidebar() != visible {
                self.layout.folder_overlay.set_show_sidebar(visible);
            }
        } else if let Some(start) = self.folder_split.start_child() {
            start.set_visible(visible);
            if visible {
                self.folder_split.set_position(folder_pane_width());
            }
        }
        self.place_menu_button();
        self.apply_title_buttons();
    }

    pub(super) fn note_single_pane_detail(&mut self) {
        if !self.layout.single_pane {
            return;
        }
        self.layout.single_show_detail = true;
        self.sync_single_pane();
        self.apply_title_buttons();
    }

    pub(super) fn back_to_messages(&mut self) {
        self.layout.single_show_detail = false;
        self.sync_single_pane();
        self.apply_title_buttons();
    }

    /// After the composer closes, stay on the reader when a message is open.
    pub(super) fn sync_detail_after_compose(&mut self) {
        if !self.layout.single_pane {
            self.sync_single_pane();
            return;
        }
        self.layout.single_show_detail = self.selected.is_some();
        self.sync_single_pane();
        self.apply_title_buttons();
    }

    pub(super) fn apply_title_buttons(&self) {
        let folders_column = if self.layout.overlay_mode {
            self.layout.folder_overlay.shows_sidebar()
        } else {
            self.layout.folders_user_visible
        };
        let pane = match reading_pane() {
            ReadingPane::Bottom => ReadingPanePlacement::Bottom,
            ReadingPane::Right => ReadingPanePlacement::Right,
        };
        let placed = title_button_placement(folders_column, pane, self.layout.single_pane);
        apply_header_buttons(&self.layout.folder_header, placed.folder);
        apply_header_buttons(&self.layout.list_header, placed.list);
        apply_header_buttons(&self.layout.reader_header, placed.reader);
        apply_header_buttons(&self.layout.composer_header, placed.composer);
    }

    pub(super) fn refresh_shortcut_tooltips(&self) {
        a11y::name_icon_control(
            &self.sync_btn,
            "Sync Folder",
            &commands::command_tooltip(crate::menu::ACTION_SYNC_FOLDER, "Sync Folder"),
        );
        a11y::name_icon_control(
            &self.layout.list_compose,
            "New Message",
            &commands::command_tooltip(ACTION_COMPOSE, "New Message"),
        );
        a11y::name_icon_control(
            &self.layout.folder_compose,
            "New Message",
            &commands::command_tooltip(ACTION_COMPOSE, "New Message"),
        );
        refresh_reader_shortcut_tooltips(&self.reader);
        for window in self.message_windows.values() {
            refresh_reader_shortcut_tooltips(&window.reader);
        }
    }

    pub(super) fn sync_single_pane(&self) {
        let single = self.layout.single_pane;
        let detail = single && self.layout.single_show_detail;
        self.layout.messages_placeholder.set_visible(!detail);
        self.layout
            .content_placeholder
            .set_visible(!single || detail);
        let show_back = detail && self.mail_content_pane == MailContentPane::Reader;
        self.layout.reader_back.set_visible(show_back);
    }

    fn set_overlay_mode(&mut self, overlay: bool) {
        self.layout.sidebar_notify_suppress.set(true);
        if overlay {
            let Some(folder) = self.folder_split.start_child() else {
                self.layout.sidebar_notify_suppress.set(false);
                return;
            };
            let Some(content) = self.folder_split.end_child() else {
                self.layout.sidebar_notify_suppress.set(false);
                return;
            };
            self.folder_split.set_start_child(None::<&gtk::Widget>);
            self.folder_split.set_end_child(None::<&gtk::Widget>);
            folder.set_visible(true);
            self.layout.folder_overlay.set_sidebar(Some(&folder));
            self.layout.folder_overlay.set_content(Some(&content));
            self.layout
                .folder_overlay
                .set_show_sidebar(self.layout.folders_user_visible);
            self.layout.mail_layout.set_visible_child_name("overlay");
        } else {
            let Some(folder) = self.layout.folder_overlay.sidebar() else {
                self.layout.sidebar_notify_suppress.set(false);
                return;
            };
            let Some(content) = self.layout.folder_overlay.content() else {
                self.layout.sidebar_notify_suppress.set(false);
                return;
            };
            self.layout.folder_overlay.set_sidebar(None::<&gtk::Widget>);
            self.layout.folder_overlay.set_content(None::<&gtk::Widget>);
            self.folder_split.set_start_child(Some(&folder));
            self.folder_split.set_end_child(Some(&content));
            folder.set_visible(self.layout.folders_user_visible);
            if self.layout.folders_user_visible {
                self.folder_split.set_position(folder_pane_width());
            }
            self.layout.mail_layout.set_visible_child_name("paned");
        }
        self.layout.overlay_mode = overlay;
        self.layout.sidebar_notify_suppress.set(false);
    }

    fn place_menu_button(&self) {
        let button = &self.layout.menu_button;
        button.unparent();
        let in_folder = if self.layout.overlay_mode {
            self.layout.folder_overlay.shows_sidebar()
        } else {
            self.layout.folders_user_visible
        };
        if in_folder {
            self.layout.folder_start.prepend(button);
        } else {
            self.layout.list_start.prepend(button);
        }
    }
}

fn apply_header_buttons(header: &adw::HeaderBar, buttons: HeaderTitleButtons) {
    header.set_show_start_title_buttons(buttons.start);
    header.set_show_end_title_buttons(buttons.end);
}
