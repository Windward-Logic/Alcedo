//! OpenPGP status banner for the message reader.

use alcedo_i18n::{tr, trf};
use std::cell::RefCell;

use alcedo_core::pgp_copy::{PgpDetails, pgp_banner_copy};
use alcedo_core::{MessageId, PgpSignatureState, PgpStatus};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::glib;

pub struct PgpBanner {
    pub root: gtk::Box,
    icon: gtk::Image,
    title: gtk::Label,
    details_btn: gtk::Button,
    accept_btn: gtk::Button,
    details_handler: RefCell<Option<glib::SignalHandlerId>>,
    accept_handler: RefCell<Option<glib::SignalHandlerId>>,
}

pub fn build_pgp_banner() -> PgpBanner {
    let icon = gtk::Image::from_icon_name("security-high-symbolic");
    icon.add_css_class("alcedo-remote-banner-icon");
    icon.set_valign(gtk::Align::Start);

    let title = gtk::Label::builder()
        .css_classes(["alcedo-pgp-banner-title"])
        .xalign(0.0)
        .hexpand(true)
        .wrap(true)
        .build();

    let details_btn = gtk::Button::builder()
        .label(tr!("Show details"))
        .css_classes(["flat"])
        .halign(gtk::Align::End)
        .valign(gtk::Align::Center)
        .build();

    let accept_btn = gtk::Button::builder()
        .label(tr!("Use key"))
        .css_classes(["suggested-action", "pill"])
        .halign(gtk::Align::End)
        .valign(gtk::Align::Center)
        .visible(false)
        .build();

    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .visible(false)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    root.append(&icon);
    root.append(&title);
    root.append(&accept_btn);
    root.append(&details_btn);

    PgpBanner {
        root,
        icon,
        title,
        details_btn,
        accept_btn,
        details_handler: RefCell::new(None),
        accept_handler: RefCell::new(None),
    }
}

pub fn hide_pgp_banner(banner: &PgpBanner) {
    banner.root.set_visible(false);
    banner.accept_btn.set_visible(false);
}

pub fn update_autocrypt_offer_banner(
    banner: &PgpBanner,
    email: &str,
    on_accept: impl FnOnce() + 'static,
) {
    let title = trf!(
        "{email} shared an OpenPGP key via Autocrypt.",
        email = email
    );
    banner.title.set_label(&title);
    banner
        .root
        .update_property(&[gtk::accessible::Property::Label(&title)]);
    banner.root.add_css_class("alcedo-pgp-banner");
    banner.root.remove_css_class("alcedo-pgp-banner-warning");
    banner.icon.set_icon_name(Some("security-high-symbolic"));
    banner.details_btn.set_visible(false);
    if let Some(handler) = banner.details_handler.borrow_mut().take() {
        banner.details_btn.disconnect(handler);
    }
    if let Some(handler) = banner.accept_handler.borrow_mut().take() {
        banner.accept_btn.disconnect(handler);
    }
    let accept = RefCell::new(Some(on_accept));
    let handler = banner.accept_btn.connect_clicked(move |_| {
        if let Some(callback) = accept.borrow_mut().take() {
            callback();
        }
    });
    *banner.accept_handler.borrow_mut() = Some(handler);
    banner.accept_btn.set_visible(true);
    banner.root.set_visible(true);
}

pub fn update_pgp_banner(banner: &PgpBanner, status: &PgpStatus) {
    let Some(copy) = pgp_banner_copy(status) else {
        hide_pgp_banner(banner);
        return;
    };
    let title = copy.title;
    let warning = copy.warning;
    banner.title.set_label(&title);
    banner
        .root
        .update_property(&[gtk::accessible::Property::Label(&title)]);
    banner.root.remove_css_class("alcedo-pgp-banner");
    banner.root.remove_css_class("alcedo-pgp-banner-warning");
    if warning {
        banner.root.add_css_class("alcedo-pgp-banner-warning");
        banner.icon.set_icon_name(Some("dialog-warning-symbolic"));
    } else {
        banner.root.add_css_class("alcedo-pgp-banner");
        banner.icon.set_icon_name(Some("security-high-symbolic"));
    }
    clear_pgp_banner_accept_handler(banner);
    if let Some(handler) = banner.details_handler.borrow_mut().take() {
        banner.details_btn.disconnect(handler);
    }
    banner.accept_btn.set_visible(false);
    if let Some(details) = copy.details {
        let handler = banner.details_btn.connect_clicked(move |_| {
            show_details_dialog(&details);
        });
        *banner.details_handler.borrow_mut() = Some(handler);
        banner.details_btn.set_visible(true);
    } else {
        banner.details_btn.set_visible(false);
    }
    banner.root.set_visible(true);
}

pub fn wire_pgp_banner_accept(banner: &PgpBanner, on_accept: impl FnOnce() + 'static) {
    clear_pgp_banner_accept_handler(banner);
    let accept = RefCell::new(Some(on_accept));
    let handler = banner.accept_btn.connect_clicked(move |_| {
        if let Some(callback) = accept.borrow_mut().take() {
            callback();
        }
    });
    *banner.accept_handler.borrow_mut() = Some(handler);
    banner.accept_btn.set_visible(true);
}

fn clear_pgp_banner_accept_handler(banner: &PgpBanner) {
    if let Some(handler) = banner.accept_handler.borrow_mut().take() {
        banner.accept_btn.disconnect(handler);
    }
}

fn show_details_dialog(details: &PgpDetails) {
    let rows = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    rows.append(&details_row(&tr!("Verdict"), details.verdict, false));
    rows.append(&details_row(&tr!("Signer"), &details.signer, false));
    rows.append(&details_row(
        &tr!("Fingerprint"),
        &details.fingerprint,
        true,
    ));
    let dialog = adw::AlertDialog::builder()
        .heading(tr!("OpenPGP Details"))
        .extra_child(&rows)
        .close_response("close")
        .build();
    dialog.add_response("close", &tr!("Close"));
    if let Some(root) = relm4::main_application().active_window() {
        dialog.choose(Some(&root), None::<&gtk::gio::Cancellable>, |_| {});
    }
}

/// Property-style row: small caption title, selectable value. Values come
/// from the key, so markup stays off.
fn details_row(title: &str, value: &str, monospace: bool) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(value)
        .use_markup(false)
        .subtitle_selectable(true)
        .css_classes(["property"])
        .build();
    if monospace {
        row.add_css_class("monospace");
    }
    row
}

pub fn update_pgp_banner_for_message(
    banner: &PgpBanner,
    _message_id: MessageId,
    status: Option<&PgpStatus>,
) {
    match status {
        Some(status) if !status_is_empty(status) => update_pgp_banner(banner, status),
        _ => hide_pgp_banner(banner),
    }
}

fn status_is_empty(status: &PgpStatus) -> bool {
    !status.encrypted && !status.decrypted && matches!(status.signature, PgpSignatureState::None)
}
