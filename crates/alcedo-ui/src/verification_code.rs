//! Verification code banner for the message reader.

use alcedo_i18n::tr;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use alcedo_core::MessageId;
use alcedo_html::{extract_verification_code, format_code_display};
use relm4::adw::prelude::*;
use relm4::gtk;

use crate::compose_attachments::show_composer_toast;
use crate::settings::cleanup_verification_codes;

/// Reader widgets for a one-time verification code banner.
pub struct OtpBanner {
    pub root: gtk::Box,
    title: gtk::Label,
    code: gtk::Label,
    copy_btn: gtk::Button,
    current: Rc<RefCell<Option<String>>>,
    message_id: Rc<Cell<Option<MessageId>>>,
    copy_gen: Rc<Cell<u32>>,
}

/// Build a hidden OTP banner inserted below the reader header zone.
pub fn build_otp_banner(on_copied: impl Fn(MessageId) + 'static) -> OtpBanner {
    let icon = gtk::Image::from_icon_name("dialog-password-symbolic");
    icon.add_css_class("alcedo-attachment-card-icon");
    icon.set_valign(gtk::Align::Start);

    let title = gtk::Label::builder()
        .css_classes(["alcedo-otp-banner-title"])
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(40)
        .build();

    let code = gtk::Label::builder()
        .css_classes(["alcedo-otp-code"])
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(20)
        .selectable(false)
        .build();

    let copy_btn = gtk::Button::builder()
        .label(tr!("Copy"))
        .css_classes(["suggested-action"])
        .tooltip_text(tr!("Copy verification code to clipboard"))
        .valign(gtk::Align::Center)
        .halign(gtk::Align::End)
        .build();

    let labels = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();
    labels.append(&title);
    labels.append(&code);

    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .css_classes(["alcedo-otp-banner"])
        .visible(false)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    root.update_property(&[gtk::accessible::Property::Label(&tr!("Verification code"))]);
    root.append(&icon);
    root.append(&labels);
    root.append(&copy_btn);

    let current = Rc::new(RefCell::new(None::<String>));
    let message_id = Rc::new(Cell::new(None));
    let copy_gen = Rc::new(Cell::new(0u32));

    let copy_action = Rc::new({
        let current = current.clone();
        let message_id = message_id.clone();
        // Weak: `copy_btn` and `root` own the handlers that hold this action.
        let copy_btn = copy_btn.downgrade();
        let root = root.downgrade();
        let copy_gen = copy_gen.clone();
        let on_copied: Rc<dyn Fn(MessageId)> = Rc::new(on_copied);
        move || {
            let (Some(copy_btn), Some(root)) = (copy_btn.upgrade(), root.upgrade()) else {
                return;
            };
            let Some(code) = current.borrow().clone() else {
                return;
            };
            let already_copied = !copy_btn.is_sensitive();
            copy_btn.clipboard().set_text(&code);
            if already_copied {
                return;
            }
            if !cleanup_verification_codes() {
                show_composer_toast(copy_btn.upcast_ref(), &tr!("Code copied"));
            }
            if let Some(id) = message_id.get() {
                on_copied(id);
            }
            set_copied_state(&copy_btn, &root, &copy_gen);
        }
    });

    copy_btn.connect_clicked({
        let copy_action = copy_action.clone();
        move |_| copy_action()
    });

    let code_gesture = gtk::GestureClick::new();
    code_gesture.set_button(1);
    code_gesture.connect_pressed({
        let copy_action = copy_action.clone();
        move |_, _, _, _| copy_action()
    });
    code.add_controller(code_gesture);

    OtpBanner {
        root,
        title,
        code,
        copy_btn,
        current,
        message_id,
        copy_gen,
    }
}

fn set_copied_state(copy_btn: &gtk::Button, root: &gtk::Box, copy_gen: &Rc<Cell<u32>>) {
    let generation = copy_gen.get() + 1;
    copy_gen.set(generation);
    copy_btn.set_label(&tr!("Copied"));
    root.add_css_class("alcedo-otp-copied");
    copy_btn.set_sensitive(false);
    let copy_btn = copy_btn.clone();
    let root = root.clone();
    let copy_gen = copy_gen.clone();
    gtk::glib::timeout_add_local_once(Duration::from_secs(2), move || {
        if copy_gen.get() != generation {
            return;
        }
        reset_copied_chrome(&copy_btn, &root);
    });
}

fn reset_copied_chrome(copy_btn: &gtk::Button, root: &gtk::Box) {
    copy_btn.set_label(&tr!("Copy"));
    root.remove_css_class("alcedo-otp-copied");
    copy_btn.set_sensitive(true);
}

pub fn update_otp_banner(
    banner: &OtpBanner,
    message_id: MessageId,
    subject: &str,
    text: &str,
    html: Option<&str>,
    enabled: bool,
) {
    banner.message_id.set(Some(message_id));
    if !enabled || text.trim() == "Loading message…" {
        hide_otp_banner(banner);
        return;
    }

    let extracted = extract_verification_code(subject, text, html);
    if let Some(verification) = extracted {
        banner.root.set_visible(true);
        banner.title.set_label(&tr!("Verification code"));
        banner
            .code
            .set_label(&format_code_display(&verification.code));
        *banner.current.borrow_mut() = Some(verification.code);
        reset_copied_chrome(&banner.copy_btn, &banner.root);
        banner.copy_gen.set(banner.copy_gen.get() + 1);
    } else {
        hide_otp_banner(banner);
    }
}

pub fn hide_otp_banner(banner: &OtpBanner) {
    banner.root.set_visible(false);
    *banner.current.borrow_mut() = None;
    banner.message_id.set(None);
    reset_copied_chrome(&banner.copy_btn, &banner.root);
    banner.copy_gen.set(banner.copy_gen.get() + 1);
}
