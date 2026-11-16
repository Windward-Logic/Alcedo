//! Breakpoint and window-control placement for the mail shell.
//!
//! Wide windows keep the resizable panes. At 860sp the rail hides and folders
//! become an overlay. At 560sp the window is a single pane.

/// Main window is at most this wide (sp): hide the rail, overlay the folders.
pub const MEDIUM_MAX_WIDTH_SP: f64 = 860.0;

/// Main window is at most this wide (sp): one pane at a time.
pub const NARROW_MAX_WIDTH_SP: f64 = 560.0;

/// Smallest useful main-window size, in pixels.
pub const MIN_WINDOW_WIDTH: i32 = 360;
pub const MIN_WINDOW_HEIGHT: i32 = 480;

/// Where the reading pane sits relative to the message list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadingPanePlacement {
    Right,
    Bottom,
}

/// What a width breakpoint does to the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdaptiveMailLayout {
    pub hide_rail: bool,
    pub folders_overlay: bool,
    pub single_pane: bool,
    /// Compose stays reachable from the list header once the rail is hidden.
    pub list_compose: bool,
    /// Reply and Forward drop their text labels.
    pub reply_icon_only: bool,
    /// The inline composer header drops its title and labels.
    pub compact_composer: bool,
}

/// `max_width_860` / `max_width_560` are the applied breakpoint flags.
/// The narrow flag implies the medium layout even if the wider breakpoint
/// has not been reported yet.
pub fn adaptive_mail_layout(max_width_860: bool, max_width_560: bool) -> AdaptiveMailLayout {
    let single_pane = max_width_560;
    let medium = max_width_860 || single_pane;
    AdaptiveMailLayout {
        hide_rail: medium,
        folders_overlay: medium,
        single_pane,
        list_compose: medium,
        reply_icon_only: single_pane,
        compact_composer: single_pane,
    }
}

/// Which edge of a header bar draws window controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderTitleButtons {
    pub start: bool,
    pub end: bool,
}

/// Window controls for each mail header. Only the outermost visible headers
/// get them: start on the leftmost column, end on the topmost header of the
/// rightmost column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TitleButtonPlacement {
    pub folder: HeaderTitleButtons,
    pub list: HeaderTitleButtons,
    pub reader: HeaderTitleButtons,
    pub composer: HeaderTitleButtons,
}

/// `folders_visible` means the folder header is the leftmost visible surface:
/// a real column, or the overlay sidebar while it is open. In the single-pane
/// layout that open sidebar owns both edges. When it is closed, the list,
/// reader, and composer headers do.
///
/// When the reading pane is below the list, the list header is the top of
/// that column, so it takes the end controls. The reader and the inline
/// composer sit under it and do not.
pub fn title_button_placement(
    folders_visible: bool,
    reading_pane: ReadingPanePlacement,
    compact: bool,
) -> TitleButtonPlacement {
    if compact {
        let both = HeaderTitleButtons {
            start: true,
            end: true,
        };
        let none = HeaderTitleButtons {
            start: false,
            end: false,
        };
        if folders_visible {
            return TitleButtonPlacement {
                folder: both,
                list: none,
                reader: none,
                composer: none,
            };
        }
        return TitleButtonPlacement {
            folder: none,
            list: both,
            reader: both,
            composer: both,
        };
    }

    let bottom = reading_pane == ReadingPanePlacement::Bottom;
    TitleButtonPlacement {
        folder: HeaderTitleButtons {
            start: folders_visible,
            end: false,
        },
        list: HeaderTitleButtons {
            start: !folders_visible,
            end: bottom,
        },
        reader: HeaderTitleButtons {
            start: false,
            end: !bottom,
        },
        composer: HeaderTitleButtons {
            start: false,
            end: !bottom,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_right_puts_controls_on_the_outer_headers() {
        let placed = title_button_placement(true, ReadingPanePlacement::Right, false);
        assert!(placed.folder.start && !placed.folder.end);
        assert!(!placed.list.start && !placed.list.end);
        assert!(!placed.reader.start && placed.reader.end);
        assert!(!placed.composer.start && placed.composer.end);
    }

    #[test]
    fn wide_bottom_gives_the_list_the_end_controls() {
        let placed = title_button_placement(true, ReadingPanePlacement::Bottom, false);
        assert!(placed.folder.start && !placed.folder.end);
        assert!(!placed.list.start && placed.list.end);
        assert!(!placed.reader.end && !placed.composer.end);
    }

    #[test]
    fn hidden_folders_give_the_list_the_start_controls() {
        let placed = title_button_placement(false, ReadingPanePlacement::Right, false);
        assert!(!placed.folder.start && !placed.folder.end);
        assert!(placed.list.start && !placed.list.end);
        assert!(placed.reader.end);
        assert!(placed.composer.end);
    }

    #[test]
    fn hidden_folders_and_bottom_pane_put_both_edges_on_the_list() {
        let placed = title_button_placement(false, ReadingPanePlacement::Bottom, false);
        assert!(placed.list.start && placed.list.end);
        assert!(!placed.reader.end && !placed.composer.end);
    }

    #[test]
    fn compact_single_pane_gives_each_surface_both_sides() {
        let placed = title_button_placement(false, ReadingPanePlacement::Bottom, true);
        assert!(!placed.folder.start && !placed.folder.end);
        assert!(placed.list.start && placed.list.end);
        assert!(placed.reader.start && placed.reader.end);
        assert!(placed.composer.start && placed.composer.end);
    }

    #[test]
    fn compact_open_sidebar_puts_both_edges_on_the_folder_header() {
        let placed = title_button_placement(true, ReadingPanePlacement::Bottom, true);
        assert!(placed.folder.start && placed.folder.end);
        assert!(!placed.list.start && !placed.list.end);
        assert!(!placed.reader.start && !placed.reader.end);
        assert!(!placed.composer.start && !placed.composer.end);
    }

    #[test]
    fn breakpoints_collapse_the_rail_then_the_panes() {
        let wide = adaptive_mail_layout(false, false);
        assert!(!wide.hide_rail);
        assert!(!wide.folders_overlay);
        assert!(!wide.single_pane);
        assert!(!wide.list_compose);
        assert!(!wide.reply_icon_only);
        assert!(!wide.compact_composer);

        let medium = adaptive_mail_layout(true, false);
        assert!(medium.hide_rail && medium.folders_overlay && medium.list_compose);
        assert!(!medium.single_pane && !medium.reply_icon_only);
        assert!(!medium.compact_composer);

        let narrow = adaptive_mail_layout(false, true);
        assert!(narrow.hide_rail && narrow.folders_overlay && narrow.single_pane);
        assert!(narrow.list_compose && narrow.reply_icon_only);
        assert!(narrow.compact_composer);
    }
}
