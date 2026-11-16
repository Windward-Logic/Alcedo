//! Dedicated message reader window (double-click / Enter).

use alcedo_core::{Address, MessageId};
use alcedo_widgets::CidCache;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;

use crate::reader::{
    ReaderWidgets, build_reader_page, clear_reader, reload_reader_document,
    set_reader_reply_icon_only,
};
use crate::settings::{ListDensity, apply_list_density, list_density};

pub struct MessageWindow {
    window: adw::ApplicationWindow,
    pub reader: ReaderWidgets,
}

impl MessageWindow {
    pub fn present(&self) {
        self.window.present();
    }

    pub fn close(&self) {
        self.window.close();
    }

    pub fn dialog_parent(&self) -> gtk::Widget {
        self.window.clone().upcast()
    }

    pub fn apply_density(&self, density: ListDensity) {
        apply_list_density(&self.window, density);
        reload_reader_document(&self.reader);
    }
}

pub fn open_message_window(
    subject: &str,
    cid_cache: CidCache,
    on_mailto: impl Fn(String) + 'static,
    on_load_remote: impl Fn() + 'static,
    on_always_from_sender: impl Fn() + 'static,
    on_reply: impl Fn() + 'static,
    on_reply_all: impl Fn() + 'static,
    on_reply_in_window: impl Fn() + 'static,
    on_reply_all_in_window: impl Fn() + 'static,
    on_reply_list: impl Fn() + 'static,
    on_forward: impl Fn() + 'static,
    on_forward_in_window: impl Fn() + 'static,
    on_view_source: impl Fn() + 'static,
    on_save_to_file: impl Fn() + 'static,
    on_unsubscribe: impl Fn() + 'static,
    on_otp_copied: impl Fn(MessageId) + 'static,
    on_react: impl Fn() + 'static,
    on_react_emoji: impl Fn(String) + 'static,
    on_address_click: impl Fn(&relm4::gtk::Widget, Address) + Clone + 'static,
    on_close: impl Fn() + 'static,
) -> MessageWindow {
    let app = relm4::main_application();
    let window = adw::ApplicationWindow::builder()
        .application(&app)
        .title(subject)
        .icon_name("dev.alcedo.Mail")
        .default_width(900)
        .default_height(700)
        .build();
    window.add_css_class(crate::theme::APPLICATION_FONT_WINDOW_CLASS);

    let toolbar_view = adw::ToolbarView::new();

    let reader_page = build_reader_page(
        on_mailto,
        on_load_remote,
        on_always_from_sender,
        on_reply,
        on_reply_all,
        on_reply_in_window,
        on_reply_all_in_window,
        on_reply_list,
        on_forward,
        on_forward_in_window,
        || {},
        || {},
        || {},
        || {},
        on_view_source,
        on_save_to_file,
        on_unsubscribe,
        on_otp_copied,
        Vec::new(),
        on_react,
        on_react_emoji,
        {
            let on_address_click = on_address_click.clone();
            move |anchor, addr| on_address_click(anchor, addr)
        },
        cid_cache,
    );
    reader_page.widgets.archive_btn.set_visible(false);
    reader_page.widgets.star_btn.set_visible(false);
    reader_page.widgets.delete_btn.set_visible(false);
    reader_page.widgets.back_btn.set_visible(false);
    set_reader_reply_icon_only(&reader_page.widgets, false);
    let header = reader_page.widgets.header_bar.clone();
    header.set_show_start_title_buttons(true);
    header.set_show_end_title_buttons(true);
    toolbar_view.add_top_bar(&header);
    clear_reader(&reader_page.widgets);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&reader_page.root));

    toolbar_view.set_content(Some(&toast_overlay));
    window.set_content(Some(&toolbar_view));
    apply_list_density(&window, list_density());
    window.connect_map(|window| {
        crate::theme::apply_toplevel_icon(window);
    });

    window.connect_destroy(move |_| on_close());

    MessageWindow {
        window,
        reader: reader_page.widgets,
    }
}
