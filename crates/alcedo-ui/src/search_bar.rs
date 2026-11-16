//! Gmail-style search filter bar under the messages header.

use std::cell::Cell;
use std::rc::Rc;

use alcedo_core::{apply_is_token, parse_search_query};
use relm4::adw;
use relm4::gtk;
use relm4::gtk::gio;
use relm4::gtk::prelude::*;

use crate::help::{self, TOPIC_SEARCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchScopeKind {
    Current,
    All,
}

pub struct SearchFilterBar {
    pub revealer: gtk::Revealer,
    save_btn: gtk::MenuButton,
    save_actions: gio::SimpleActionGroup,
    scope_here: gtk::ToggleButton,
    scope_all: gtk::ToggleButton,
    unread_btn: gtk::ToggleButton,
    star_btn: gtk::ToggleButton,
    attachment_btn: gtk::ToggleButton,
    syncing: Rc<Cell<bool>>,
}

impl SearchFilterBar {
    pub fn new(
        on_scope: Rc<dyn Fn(SearchScopeKind)>,
        on_query_rewrite: Rc<dyn Fn(String)>,
    ) -> Self {
        let syncing = Rc::new(Cell::new(false));

        let scope_here = gtk::ToggleButton::with_label("This folder");
        scope_here.set_tooltip_text(Some("Search this folder"));
        let scope_all = gtk::ToggleButton::with_label("All mail");
        scope_all.set_tooltip_text(Some("Search all folders"));
        scope_here.set_active(true);
        scope_here.add_css_class("flat");
        scope_all.add_css_class("flat");

        let scope_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        scope_box.add_css_class("linked");
        scope_box.append(&scope_here);
        scope_box.append(&scope_all);

        let unread_btn = icon_toggle("mail-unread-symbolic", "Unread");
        let star_btn = icon_toggle("non-starred-symbolic", "Starred");
        let attachment_btn = icon_toggle("mail-attachment-symbolic", "Has attachment");

        let flags_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        flags_box.add_css_class("linked");
        flags_box.append(&unread_btn);
        flags_box.append(&star_btn);
        flags_box.append(&attachment_btn);

        // The revealer reports this bar's width even while hidden, so a
        // single row would set the list column's minimum width and the
        // window would clip its title buttons. Wrap the groups instead.
        let filters = adw::WrapBox::builder()
            .child_spacing(8)
            .line_spacing(4)
            .build();
        filters.append(&scope_box);
        filters.append(&flags_box);

        let action_bar = gtk::ActionBar::builder()
            .css_classes(["alcedo-search-filter-bar"])
            .build();
        action_bar.pack_start(&filters);

        let save_actions = gio::SimpleActionGroup::new();
        let save_menu = gio::Menu::new();
        let save_btn = gtk::MenuButton::builder()
            .label("Save")
            .tooltip_text("Save search")
            .css_classes(["flat"])
            .menu_model(&save_menu)
            .sensitive(false)
            .build();
        save_btn.insert_action_group("save", Some(&save_actions));
        action_bar.pack_end(&save_btn);
        action_bar.pack_end(&help::help_button(TOPIC_SEARCH));

        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(180)
            .reveal_child(false)
            .child(&action_bar)
            .build();

        {
            let on_scope_here = on_scope.clone();
            let syncing_here = syncing.clone();
            scope_here.connect_toggled({
                let scope_here = scope_here.clone();
                let scope_all = scope_all.clone();
                move |btn| {
                    if syncing_here.get() {
                        return;
                    }
                    if btn.is_active() {
                        scope_all.set_active(false);
                        on_scope_here(SearchScopeKind::Current);
                    } else if !scope_all.is_active() {
                        scope_here.set_active(true);
                    }
                }
            });
            scope_all.connect_toggled({
                let scope_here = scope_here.clone();
                let scope_all = scope_all.clone();
                let on_scope = on_scope.clone();
                let syncing_all = syncing.clone();
                move |btn| {
                    if syncing_all.get() {
                        return;
                    }
                    if btn.is_active() {
                        scope_here.set_active(false);
                        on_scope(SearchScopeKind::All);
                    } else if !scope_here.is_active() {
                        scope_all.set_active(true);
                    }
                }
            });
        }

        connect_flag_toggle(&unread_btn, &syncing, "is:unread", on_query_rewrite.clone());
        connect_flag_toggle(&star_btn, &syncing, "is:starred", on_query_rewrite.clone());
        connect_flag_toggle(
            &attachment_btn,
            &syncing,
            "has:attachment",
            on_query_rewrite,
        );

        Self {
            revealer,
            save_btn,
            save_actions,
            scope_here,
            scope_all,
            unread_btn,
            star_btn,
            attachment_btn,
            syncing,
        }
    }

    pub fn set_revealed(&self, on: bool) {
        self.revealer.set_reveal_child(on);
        self.save_btn.set_sensitive(on);
    }

    /// Views backed by a saved query always search all mail; lock "Here".
    pub fn set_scope_locked(&self, on: bool) {
        if on {
            self.scope_here.set_sensitive(false);
            self.scope_here.set_tooltip_text(Some(
                "This view searches all mail unless the query has folder: or in:",
            ));
        }
    }

    /// Fill the save menu with `labels`; choosing item `i` calls `on_item(i)`.
    /// No labels hides the menu.
    pub fn wire_save_menu(&self, labels: &[String], on_item: impl Fn(usize) + Clone + 'static) {
        while let Some(action) = self.save_actions.list_actions().first() {
            self.save_actions.remove_action(action);
        }
        if labels.is_empty() {
            self.save_btn.set_menu_model(None::<&gio::Menu>);
            return;
        }
        let menu = gio::Menu::new();
        for (index, label) in labels.iter().enumerate() {
            let name = format!("item{index}");
            menu.append(Some(label), Some(&format!("save.{name}")));
            let action = gio::SimpleAction::new(&name, None);
            let on_item = on_item.clone();
            action.connect_activate(move |_, _| on_item(index));
            self.save_actions.add_action(&action);
        }
        self.save_btn.set_menu_model(Some(&menu));
    }

    pub fn set_current_label(&self, label: &str) {
        self.scope_here.set_label(label);
    }

    pub fn set_scope(&self, kind: SearchScopeKind) {
        self.syncing.set(true);
        match kind {
            SearchScopeKind::Current => {
                self.scope_here.set_active(true);
                self.scope_all.set_active(false);
            }
            SearchScopeKind::All => {
                self.scope_all.set_active(true);
                self.scope_here.set_active(false);
            }
        }
        self.syncing.set(false);
    }

    pub fn set_folder_operator_active(&self, on: bool) {
        self.scope_here.set_sensitive(!on);
        self.scope_all.set_sensitive(!on);
        let tooltip = if on {
            "Folder is set in the search query"
        } else {
            "Search all folders"
        };
        self.scope_all.set_tooltip_text(Some(tooltip));
        if on {
            self.scope_here
                .set_tooltip_text(Some("Folder is set in the search query"));
        } else {
            self.scope_here.set_tooltip_text(Some("Search this folder"));
        }
    }

    pub fn sync_toggles_from_query(&self, raw: &str) {
        let parsed = parse_search_query(raw);
        self.syncing.set(true);
        self.unread_btn.set_active(parsed.unread == Some(true));
        self.star_btn.set_active(parsed.starred == Some(true));
        self.attachment_btn.set_active(parsed.has_attachment);
        self.syncing.set(false);
    }
}

fn icon_toggle(icon_name: &str, tooltip: &str) -> gtk::ToggleButton {
    gtk::ToggleButton::builder()
        .icon_name(icon_name)
        .tooltip_text(tooltip)
        .css_classes(["flat"])
        .focus_on_click(false)
        .build()
}

fn connect_flag_toggle(
    button: &gtk::ToggleButton,
    syncing: &Rc<Cell<bool>>,
    token: &'static str,
    on_query_rewrite: Rc<dyn Fn(String)>,
) {
    let syncing = syncing.clone();
    button.connect_toggled(move |btn| {
        if syncing.get() {
            return;
        }
        let raw = current_query_text();
        let rewritten = apply_is_token(&raw, token, btn.is_active());
        set_last_query_for_toggles(&rewritten);
        on_query_rewrite(rewritten);
    });
}

fn current_query_text() -> String {
    LAST_QUERY.with(|slot| slot.borrow().clone())
}

pub fn set_last_query_for_toggles(raw: &str) {
    LAST_QUERY.with(|slot| *slot.borrow_mut() = raw.to_string());
}

pub fn filter_toggle_states(raw: &str) -> (bool, bool, bool) {
    let parsed = parse_search_query(raw);
    (
        parsed.unread == Some(true),
        parsed.starred == Some(true),
        parsed.has_attachment,
    )
}

thread_local! {
    static LAST_QUERY: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_toggle_states_reads_operators() {
        let (unread, starred, attachment) =
            filter_toggle_states("michael is:unread has:attachment");
        assert!(unread);
        assert!(attachment);
        assert!(!starred);
    }

    #[test]
    fn hidden_filter_bar_fits_the_narrowest_window() {
        crate::test_helpers::gtk_test_run(|| {
            let bar = SearchFilterBar::new(Rc::new(|_| {}), Rc::new(|_| {}));
            let (min, _, _, _) = bar.revealer.measure(gtk::Orientation::Horizontal, -1);
            assert!(
                min <= alcedo_core::layout_policy::MIN_WINDOW_WIDTH,
                "filter bar needs {min}px"
            );
        });
    }
}
