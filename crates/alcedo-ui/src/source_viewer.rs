//! View raw RFC822 source and save messages to `.eml` files.

use alcedo_i18n::{tr, trs};
use relm4::adw;

use crate::attachment_save::sanitize_filename;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::gio;

pub fn suggested_eml_filename(subject: &str) -> String {
    let trimmed = subject.trim();
    let base = if trimmed.is_empty() {
        "message".to_string()
    } else {
        sanitize_filename(trimmed, "message")
    };
    if base.ends_with(".eml") {
        base
    } else {
        format!("{base}.eml")
    }
}

fn dialog_parent(parent: &gtk::Widget) -> Option<gtk::Window> {
    parent
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok())
}

pub fn save_message_to_file(parent: &impl IsA<gtk::Widget>, raw: &[u8], suggested_filename: &str) {
    let dialog = gtk::FileDialog::new();
    dialog.set_title(&tr!("Save message"));
    dialog.set_initial_name(Some(suggested_filename));
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(trs!("Email message")));
    filter.add_mime_type("message/rfc822");
    filter.add_pattern("*.eml");
    dialog.set_default_filter(Some(&filter));
    let parent_window = dialog_parent(parent.as_ref());
    let raw = raw.to_vec();

    dialog.save(
        parent_window.as_ref(),
        None::<&gio::Cancellable>,
        move |result| match result {
            Ok(file) => {
                let Some(path) = file.path() else {
                    return;
                };
                // A message can be tens of MiB and the target a slow mount.
                gtk::glib::spawn_future_local(async move {
                    let written = crate::blocking_io::run(move || std::fs::write(&path, &raw));
                    if let Some(Err(err)) = written.await {
                        tracing::warn!(%err, "failed to save message to file");
                    }
                });
            }
            Err(err) if err.matches(gio::IOErrorEnum::Cancelled) => {}
            Err(err) => {
                tracing::warn!(%err, "save message dialog failed");
            }
        },
    );
}

pub fn open_source_viewer(parent: &impl IsA<gtk::Widget>, raw: Vec<u8>, suggested_filename: &str) {
    let text = String::from_utf8_lossy(&raw).into_owned();

    let text_view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(true)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::None)
        .left_margin(12)
        .right_margin(12)
        .top_margin(12)
        .bottom_margin(12)
        .build();
    text_view.buffer().set_text(&text);

    let scroller = gtk::ScrolledWindow::builder()
        .child(&text_view)
        .vexpand(true)
        .hexpand(true)
        .build();

    let close_btn = gtk::Button::builder().label(tr!("Close")).build();
    let save_btn = gtk::Button::builder()
        .label(tr!("Save to File…"))
        .css_classes(["suggested-action"])
        .build();

    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let title_label = gtk::Label::builder()
        .label(tr!("Message Source"))
        .css_classes(["title"])
        .build();
    header.set_title_widget(Some(&title_label));
    header.pack_start(&close_btn);
    header.pack_end(&save_btn);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder()
        .title(tr!("Message Source"))
        .content_width(720)
        .content_height(560)
        .default_widget(&close_btn)
        .focus_widget(&text_view)
        .child(&toolbar)
        .build();

    let parent_widget = parent.as_ref().clone();
    let filename = suggested_filename.to_string();
    save_btn.connect_clicked({
        let parent_widget = parent_widget.clone();
        let raw = raw.clone();
        let filename = filename.clone();
        move |_| save_message_to_file(&parent_widget, &raw, &filename)
    });

    close_btn.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });

    dialog.present(Some(parent.as_ref()));
}
