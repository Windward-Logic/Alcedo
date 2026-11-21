//! Adaptive mail chrome: overlay folders, single pane, and title buttons.

use alcedo_i18n::{tr, trs};
use std::cell::Cell;
use std::rc::Rc;

use alcedo_core::layout_policy::{
    HeaderTitleButtons, ReadingPanePlacement, WidthBreakpoints, adaptive_mail_layout,
    folders_as_overlay, title_button_placement,
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
use crate::settings::{ReadingPane, folder_pane_width, reading_pane, set_folders_docked};

use super::folder_peek::FolderPeek;
use super::{MailContentPane, MailModel};

// libadwaita's `OverlaySplitView` defaults, restored when the window (not the
// user's undocking) is what makes the folders an overlay.
const DEFAULT_MIN_SIDEBAR_WIDTH: f64 = 180.0;
const DEFAULT_MAX_SIDEBAR_WIDTH: f64 = 280.0;

pub(super) struct MailLayout {
    pub mail_layout: gtk::Stack,
    pub folder_overlay: adw::OverlaySplitView,
    pub folder_header: adw::HeaderBar,
    pub folder_start: gtk::Box,
    pub folder_compose: gtk::Button,
    /// Hides the overlay sidebar; only shown while folders are an overlay.
    pub folder_close: gtk::Button,
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
    /// The window is too narrow for a folder column.
    pub overlay_forced: bool,
    pub folder_peek: FolderPeek,
    /// Set while panes are reparented so a sidebar notification cannot hide them.
    pub sidebar_notify_suppress: Rc<Cell<bool>>,
    pub single_pane: bool,
    /// The user wants the folders as a column beside the list (saved).
    pub folders_docked: bool,
    pub single_show_detail: bool,
}

pub(super) struct FolderChrome {
    pub header: adw::HeaderBar,
    pub start: gtk::Box,
    pub compose: gtk::Button,
    pub menu: gtk::MenuButton,
    pub close: gtk::Button,
}

pub(super) fn build_folder_chrome(on_close: impl Fn() + 'static) -> FolderChrome {
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
        &tr!("New Message"),
        &commands::command_tooltip(ACTION_COMPOSE, &tr!("New Message")),
    );
    let start = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    start.append(&menu);
    start.append(&compose);
    header.pack_start(&start);

    // The list header's toggle sits under the open sidebar, so the sidebar
    // carries its own way out.
    let close = gtk::Button::builder()
        .css_classes(["flat"])
        .visible(false)
        .build();
    icons::set_button_icon(&close, icons::FOLDER_PANE, icons::TOOLBAR_PIXEL_SIZE);
    a11y::label_icon_control(&close, &tr!("Hide Folders"));
    close.connect_clicked(move |_| on_close());
    header.pack_end(&close);
    FolderChrome {
        header,
        start,
        compose,
        menu,
        close,
    }
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
        self.layout.overlay_forced = spec.folders_overlay;
        self.sync_folder_placement();
        if !spec.single_pane {
            self.layout.single_show_detail = false;
        }
        self.sync_single_pane();
        self.sync_folder_chrome();
    }

    /// The folder toggle was used. In a wide window it docks or undocks the
    /// column; where the folders are always an overlay it opens or closes it.
    pub(super) fn set_folders_visible(&mut self, visible: bool) {
        if self.layout.overlay_forced {
            self.layout.folder_overlay.set_show_sidebar(visible);
        } else {
            self.layout.folders_docked = visible;
            set_folders_docked(visible);
            self.sync_folder_placement();
        }
        self.sync_folder_chrome();
    }

    /// Slide the overlay sidebar away. Does nothing for a docked column.
    pub(super) fn dismiss_folder_overlay(&self) {
        if self.layout.overlay_mode {
            self.layout.folder_overlay.set_show_sidebar(false);
        }
    }

    /// Put the folders in a column or an overlay, whichever the window and the
    /// user's choice call for.
    pub(super) fn sync_folder_placement(&mut self) {
        let overlay = folders_as_overlay(self.layout.overlay_forced, self.layout.folders_docked);
        if overlay != self.layout.overlay_mode {
            self.set_overlay_mode(overlay);
        }
        self.apply_overlay_width();
    }

    /// Bring the toggle, the sidebar's close button, the menu button and the
    /// window controls in line with where the folders are now.
    pub(super) fn sync_folder_chrome(&self) {
        let shown = if self.layout.overlay_forced {
            self.layout.folder_overlay.shows_sidebar()
        } else {
            self.layout.folders_docked
        };
        if self.layout.folder_toggle.is_active() != shown {
            self.layout.folder_toggle.set_active(shown);
        }
        a11y::label_icon_control(
            &self.layout.folder_toggle,
            if shown {
                trs!("Hide Folders")
            } else {
                trs!("Show Folders")
            },
        );
        self.layout
            .folder_close
            .set_visible(self.layout.overlay_mode);
        self.place_menu_button();
        self.apply_title_buttons();
    }

    /// The folders are on screen: a column, or an overlay that is open.
    fn folders_open(&self) -> bool {
        !self.layout.overlay_mode || self.layout.folder_overlay.shows_sidebar()
    }

    /// A peeking sidebar keeps the docked column's width. When the window
    /// forces the overlay, the sidebar follows libadwaita's default sizing.
    fn apply_overlay_width(&self) {
        let overlay = &self.layout.folder_overlay;
        if self.layout.overlay_forced {
            overlay.set_min_sidebar_width(DEFAULT_MIN_SIDEBAR_WIDTH);
            overlay.set_max_sidebar_width(DEFAULT_MAX_SIDEBAR_WIDTH);
        } else {
            let width = f64::from(folder_pane_width());
            overlay.set_min_sidebar_width(width);
            overlay.set_max_sidebar_width(width);
        }
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
        let folders_column = self.folders_open();
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
            &tr!("Sync Folder"),
            &commands::command_tooltip(crate::menu::ACTION_SYNC_FOLDER, &tr!("Sync Folder")),
        );
        a11y::name_icon_control(
            &self.layout.list_compose,
            &tr!("New Message"),
            &commands::command_tooltip(ACTION_COMPOSE, &tr!("New Message")),
        );
        a11y::name_icon_control(
            &self.layout.folder_compose,
            &tr!("New Message"),
            &commands::command_tooltip(ACTION_COMPOSE, &tr!("New Message")),
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
            // The overlay always starts closed: a window that narrows should
            // land on the list, and the toggle or the edge opens the folders.
            self.layout.folder_overlay.set_show_sidebar(false);
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
            self.layout.folder_overlay.set_show_sidebar(false);
            self.layout.folder_overlay.set_sidebar(None::<&gtk::Widget>);
            self.layout.folder_overlay.set_content(None::<&gtk::Widget>);
            self.folder_split.set_start_child(Some(&folder));
            self.folder_split.set_end_child(Some(&content));
            folder.set_visible(true);
            self.folder_split.set_position(folder_pane_width());
            self.layout.mail_layout.set_visible_child_name("paned");
        }
        self.layout.overlay_mode = overlay;
        self.layout.folder_peek.reset();
        self.layout.folder_peek.set_enabled(overlay);
        self.layout.sidebar_notify_suppress.set(false);
    }

    fn place_menu_button(&self) {
        let button = &self.layout.menu_button;
        button.unparent();
        if self.folders_open() {
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
