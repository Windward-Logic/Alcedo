//! Widget checks shared by this crate's tests.

pub(crate) use crate::test_support::gtk_test_run;

/// True when `widget` opens on drag hover (has a `DropControllerMotion`).
#[cfg(test)]
pub(crate) fn expands_on_drag_hover(
    widget: &impl relm4::gtk::prelude::IsA<relm4::gtk::Widget>,
) -> bool {
    use relm4::gtk::prelude::*;
    let controllers = widget.as_ref().observe_controllers();
    (0..controllers.n_items()).any(|i| {
        controllers
            .item(i)
            .is_some_and(|item| item.is::<relm4::gtk::DropControllerMotion>())
    })
}

/// The text every `GtkLabel` under `widget` displays, after any markup is
/// parsed. A label whose markup failed to parse displays nothing.
#[cfg(test)]
pub(crate) fn displayed_label_texts(
    widget: &impl relm4::gtk::prelude::IsA<relm4::gtk::Widget>,
) -> Vec<String> {
    use relm4::gtk;
    use relm4::gtk::prelude::*;

    fn collect(widget: &gtk::Widget, out: &mut Vec<String>) {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            out.push(label.text().to_string());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            collect(&current, out);
            child = current.next_sibling();
        }
    }

    let mut out = Vec::new();
    collect(widget.as_ref(), &mut out);
    out
}

/// True when a `ListBoxRow` is finalized once removed from its list, with the
/// `Rc` sentinel that `attach` captured in the row's controller callbacks
/// released too. A callback that holds the row strongly forms a cycle
/// (row -> controller -> closure -> row) that keeps both alive forever.
#[cfg(test)]
pub(crate) fn row_is_freed_after_removal(
    attach: impl FnOnce(&relm4::gtk::ListBoxRow, std::rc::Rc<()>),
) -> bool {
    use relm4::gtk;
    use relm4::gtk::prelude::*;

    let list = gtk::ListBox::new();
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&gtk::Label::new(Some("row"))));
    let sentinel = std::rc::Rc::new(());
    attach(&row, sentinel.clone());
    list.append(&row);
    let weak_row = row.downgrade();
    list.remove(&row);
    drop(row);
    weak_row.upgrade().is_none() && std::rc::Rc::strong_count(&sentinel) == 1
}
