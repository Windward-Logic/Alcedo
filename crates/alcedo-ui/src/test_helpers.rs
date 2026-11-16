//! Shared GTK test thread so `gtk::init` runs exactly once.

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

/// WebKitGTK’s bubblewrap sandbox cannot create user namespaces in GitHub
/// Actions job containers (`bwrap: Creating new namespace failed`). Tests that
/// construct a `WebView` abort unless the sandbox is off.
#[cfg(test)]
fn disable_webkit_sandbox_for_tests() {
    const KEY: &str = "WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS";
    if std::env::var_os(KEY).is_none() {
        // SAFETY: gtk_test_run’s Once runs this before any WebView is created.
        // WebKit child processes inherit the variable.
        unsafe {
            std::env::set_var(KEY, "1");
        }
    }
}

#[cfg(test)]
pub(crate) fn gtk_test_run<F: FnOnce() + Send + 'static>(f: F) {
    use std::sync::{OnceLock, mpsc};

    static GTK_TX: OnceLock<mpsc::Sender<Box<dyn FnOnce() + Send>>> = OnceLock::new();
    GTK_TX.get_or_init(|| {
        disable_webkit_sandbox_for_tests();
        let (tx, rx) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        std::thread::Builder::new()
            .name("alcedo-gtk-test".into())
            .spawn(move || {
                relm4::gtk::init().expect("gtk init");
                ready_tx.send(()).expect("gtk test thread ready");
                for f in rx {
                    f();
                }
            })
            .expect("spawn gtk test thread");
        ready_rx.recv().expect("gtk test thread init");
        tx
    });
    let tx = GTK_TX.get().expect("gtk test thread");
    let (done_tx, done_rx) = mpsc::sync_channel(0);
    tx.send(Box::new(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        let _ = done_tx.send(result);
    }))
    .expect("gtk test thread died");
    match done_rx.recv().expect("gtk test thread dropped result") {
        Ok(()) => {}
        Err(payload) => std::panic::resume_unwind(payload),
    }
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
