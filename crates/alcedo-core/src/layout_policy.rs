//! Breakpoint and window-control placement for the mail shell.
//!
//! Wide windows keep the resizable panes. As the window narrows, toolbars drop
//! their labels, then the rail hides and folders become an overlay, and finally
//! the window shows a single pane. Each limit is the narrowest width at which
//! the wider layout still fits its minimum content.

/// Main window is at most this wide (sp): toolbars drop their text labels.
pub const COMPACT_MAX_WIDTH_SP: f64 = 1040.0;

/// Main window is at most this wide (sp): hide the rail, overlay the folders.
pub const MEDIUM_MAX_WIDTH_SP: f64 = 975.0;

/// Main window is at most this wide (sp): one pane at a time.
pub const NARROW_MAX_WIDTH_SP: f64 = 750.0;

const _: () = assert!(
    NARROW_MAX_WIDTH_SP < MEDIUM_MAX_WIDTH_SP && MEDIUM_MAX_WIDTH_SP < COMPACT_MAX_WIDTH_SP
);

/// Smallest useful main-window size, in pixels.
pub const MIN_WINDOW_WIDTH: i32 = 360;
pub const MIN_WINDOW_HEIGHT: i32 = 480;

/// Where the reading pane sits relative to the message list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadingPanePlacement {
    Right,
    Bottom,
}

/// The window-width breakpoints currently applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WidthBreakpoints {
    pub compact: bool,
    pub medium: bool,
    pub narrow: bool,
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

/// Only the narrowest matching window breakpoint is applied, so each breakpoint
/// implies the layouts wider than it.
pub fn adaptive_mail_layout(applied: WidthBreakpoints) -> AdaptiveMailLayout {
    let single_pane = applied.narrow;
    let medium = applied.medium || single_pane;
    let compact = applied.compact || medium;
    AdaptiveMailLayout {
        hide_rail: medium,
        folders_overlay: medium,
        single_pane,
        list_compose: medium,
        reply_icon_only: compact,
        compact_composer: compact,
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
    fn wide_window_keeps_every_pane_and_label() {
        let wide = adaptive_mail_layout(WidthBreakpoints::default());
        assert!(!wide.hide_rail);
        assert!(!wide.folders_overlay);
        assert!(!wide.single_pane);
        assert!(!wide.list_compose);
        assert!(!wide.reply_icon_only);
        assert!(!wide.compact_composer);
    }

    #[test]
    fn compact_window_drops_toolbar_labels_but_keeps_the_panes() {
        let compact = adaptive_mail_layout(WidthBreakpoints {
            compact: true,
            ..WidthBreakpoints::default()
        });
        assert!(compact.reply_icon_only && compact.compact_composer);
        assert!(!compact.hide_rail && !compact.folders_overlay && !compact.single_pane);
        assert!(!compact.list_compose);
    }

    #[test]
    fn medium_window_hides_the_rail_and_stays_compact() {
        let medium = adaptive_mail_layout(WidthBreakpoints {
            medium: true,
            ..WidthBreakpoints::default()
        });
        assert!(medium.hide_rail && medium.folders_overlay && medium.list_compose);
        assert!(medium.reply_icon_only && medium.compact_composer);
        assert!(!medium.single_pane);
    }

    #[test]
    fn narrow_window_shows_one_pane_and_every_wider_change() {
        let narrow = adaptive_mail_layout(WidthBreakpoints {
            narrow: true,
            ..WidthBreakpoints::default()
        });
        assert!(narrow.hide_rail && narrow.folders_overlay && narrow.single_pane);
        assert!(narrow.list_compose && narrow.reply_icon_only);
        assert!(narrow.compact_composer);
    }
}
