//! The four stacked lists of the folder pane.
//!
//! Only `middle` sits in a scroller. `top`, `pinned` and `bottom` keep their
//! place while the account folders scroll.

use relm4::gtk;
use relm4::gtk::prelude::*;

/// Folder-pane lists from top to bottom of the pane.
#[derive(Clone)]
pub struct FolderPaneLists {
    /// Everything and extension views listed after it.
    pub top: gtk::ListBox,
    /// Account folders and the Hidden Accounts area; the only list that scrolls.
    pub middle: gtk::ListBox,
    /// Add Account row.
    pub pinned: gtk::ListBox,
    /// Extension views grouped in sections at the bottom.
    pub bottom: gtk::ListBox,
}

impl FolderPaneLists {
    pub fn all(&self) -> [&gtk::ListBox; 4] {
        [&self.top, &self.middle, &self.pinned, &self.bottom]
    }

    pub fn remove_all_rows(&self) {
        for list in self.all() {
            while let Some(row) = list.row_at_index(0) {
                list.remove(&row);
            }
        }
    }

    pub fn selected_row(&self) -> Option<gtk::ListBoxRow> {
        self.all().into_iter().find_map(|list| list.selected_row())
    }

    pub fn unselect_all(&self) {
        for list in self.all() {
            list.unselect_all();
        }
    }

    /// The pinned list takes no space when it has no rows.
    pub fn sync_pinned_visibility(&self) {
        self.pinned
            .set_visible(self.pinned.row_at_index(0).is_some());
    }

    /// Selecting a row in one list clears the highlight in the others, so the
    /// four lists behave like one.
    pub fn keep_single_selection(&self) {
        for list in self.all() {
            let lists = self.clone();
            list.connect_row_selected(move |source, row| {
                if row.is_none() {
                    return;
                }
                for other in lists.all() {
                    if other != source {
                        other.unselect_all();
                    }
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lists() -> FolderPaneLists {
        FolderPaneLists {
            top: gtk::ListBox::new(),
            middle: gtk::ListBox::new(),
            pinned: gtk::ListBox::new(),
            bottom: gtk::ListBox::new(),
        }
    }

    #[test]
    fn selecting_in_one_list_clears_the_others() {
        crate::test_helpers::gtk_test_run(|| {
            let lists = lists();
            lists.keep_single_selection();
            let top_row = gtk::ListBoxRow::new();
            let bottom_row = gtk::ListBoxRow::new();
            lists.top.append(&top_row);
            lists.bottom.append(&bottom_row);

            lists.top.select_row(Some(&top_row));
            lists.bottom.select_row(Some(&bottom_row));

            assert!(lists.top.selected_row().is_none());
            assert_eq!(lists.bottom.selected_row(), Some(bottom_row));
        });
    }

    #[test]
    fn pinned_list_is_hidden_until_it_has_rows() {
        crate::test_helpers::gtk_test_run(|| {
            let lists = lists();
            lists.sync_pinned_visibility();
            assert!(!lists.pinned.is_visible());

            lists.pinned.append(&gtk::ListBoxRow::new());
            lists.sync_pinned_visibility();
            assert!(lists.pinned.is_visible());
        });
    }

    #[test]
    fn remove_all_rows_empties_every_list() {
        crate::test_helpers::gtk_test_run(|| {
            let lists = lists();
            for list in lists.all() {
                list.append(&gtk::ListBoxRow::new());
            }
            lists.remove_all_rows();
            assert!(
                lists
                    .all()
                    .iter()
                    .all(|list| list.row_at_index(0).is_none())
            );
        });
    }
}
