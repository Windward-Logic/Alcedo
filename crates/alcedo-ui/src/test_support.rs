//! Test support for this crate and for crates that extend it, available to
//! other crates through the `test-support` feature.

/// WebKitGTK’s bubblewrap sandbox cannot create user namespaces in GitHub
/// Actions job containers (`bwrap: Creating new namespace failed`). Tests that
/// construct a `WebView` abort unless the sandbox is off.
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

/// Run `f` on the one thread that owns GTK, so `gtk::init` runs exactly once.
/// A panic in `f` fails the calling test.
pub fn gtk_test_run<F: FnOnce() + Send + 'static>(f: F) {
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
