//! Run blocking file I/O off the GTK thread.
//!
//! Reading a large attachment or writing a file to a slow disk or network
//! mount can take seconds; doing it in a signal handler freezes the window.
//! Await [`run`] from `glib::spawn_future_local` instead: the work happens on
//! GLib's worker pool and the result comes back on the GTK thread.

use relm4::gtk::gio;

/// Run `work` on a worker thread. `None` means the work panicked.
pub(crate) async fn run<T, F>(work: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    match gio::spawn_blocking(work).await {
        Ok(value) => Some(value),
        Err(_) => {
            tracing::error!("blocking I/O task panicked");
            None
        }
    }
}
