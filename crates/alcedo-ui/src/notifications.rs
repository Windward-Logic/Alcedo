//! Desktop and shell indicators for new inbox mail.

use alcedo_i18n::{trf, trs};
use std::collections::{HashMap, HashSet};

use alcedo_core::{Contact, Envelope, FolderId, MessageId, sender_is_blocked};

/// How unread count changes should affect shell indicators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreadAttention {
    SilentUpdate,
    DemandAttention,
    Clear,
}

pub fn classify_unread_attention(
    previous: Option<u32>,
    current: u32,
    attending: bool,
) -> UnreadAttention {
    match previous {
        None => UnreadAttention::SilentUpdate,
        Some(prev) if current == 0 && prev > 0 => UnreadAttention::Clear,
        Some(prev) if current > prev && !attending => UnreadAttention::DemandAttention,
        _ => UnreadAttention::SilentUpdate,
    }
}

pub fn newest_new_unseen<'a>(
    known: &HashSet<MessageId>,
    incoming: &'a [Envelope],
) -> Option<&'a Envelope> {
    incoming
        .iter()
        .filter(|env| !known.contains(&env.id) && !env.seen)
        .max_by(|a, b| a.date.cmp(&b.date).then_with(|| a.id.0.cmp(&b.id.0)))
}

/// Whether a new-mail desktop notification should fire: always for a
/// verification code, never for a blocked sender, otherwise unless an
/// extension holds it back.
pub fn should_notify(envelope: &Envelope, otp: Option<&str>, contacts: &[Contact]) -> bool {
    if otp.is_some() {
        return true;
    }
    let from = envelope
        .from
        .first()
        .map(|addr| addr.email.as_str())
        .unwrap_or("");
    if sender_is_blocked(from, contacts) {
        return false;
    }
    crate::extension::registered()
        .iter()
        .all(|ext| ext.allow_notification(envelope, contacts))
}

/// Record inbox envelope ids and return newly seen-unseen messages to notify for.
///
/// While a folder is not yet ready, ids are baselined without notifying. The
/// folder is marked ready only when `complete_folder` is true (a full folder
/// sync). Unified-inbox subsets may omit older messages and must not mark ready.
pub fn observe_inbox_batch<'a>(
    known: &mut HashSet<MessageId>,
    ready_folders: &mut HashSet<FolderId>,
    folder: FolderId,
    incoming: &'a [Envelope],
    complete_folder: bool,
) -> Vec<&'a Envelope> {
    if !ready_folders.contains(&folder) {
        for env in incoming {
            known.insert(env.id);
        }
        if complete_folder {
            ready_folders.insert(folder);
        }
        return Vec::new();
    }
    let mut new_unseen: Vec<&Envelope> = incoming
        .iter()
        .filter(|env| !known.contains(&env.id) && !env.seen)
        .collect();
    new_unseen.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.id.0.cmp(&b.id.0)));
    for env in incoming {
        known.insert(env.id);
    }
    new_unseen
}

pub fn mail_notification_id(id: MessageId) -> String {
    format!("new-mail-{}", id.0)
}

pub fn notification_title(envelope: &Envelope) -> String {
    envelope
        .from
        .first()
        .map(|addr| addr.display_name())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "New mail".to_string())
}

pub fn notification_subject_body(envelope: &Envelope) -> String {
    let subject = envelope.display_subject();
    let trimmed = subject.trim();
    if trimmed.is_empty() {
        alcedo_core::NO_SUBJECT.to_string()
    } else {
        subject
    }
}

pub fn notification_otp_code(subject: &str, snippet: &str) -> Option<String> {
    alcedo_html::extract_verification_code(subject, snippet, None).map(|v| v.code)
}

pub fn notification_body(envelope: &Envelope, otp: Option<&str>) -> String {
    match otp {
        Some(code) => trf!(
            "Verification code\n{code}",
            code = alcedo_html::format_code_display(code)
        ),
        None => notification_subject_body(envelope),
    }
}

pub fn notification_copy_label() -> &'static str {
    trs!("Copy")
}

pub fn notification_mark_read_label() -> &'static str {
    trs!("Mark as read")
}

pub fn notification_delete_label() -> &'static str {
    trs!("Delete")
}

/// Action run when the notification banner itself is clicked.
///
/// GNOME Shell draws the banner as one button. That click runs the default
/// action, and Copy / Mark as read / Delete only appear as their own buttons
/// after the banner expands. Those buttons must not be the default action, or
/// a click to read the message copies a code and can delete the mail.
pub fn mail_notification_default_action() -> &'static str {
    crate::menu::ACTION_OPEN_MESSAGE
}

thread_local! {
    static PENDING_NOTIFICATION_OTP: std::cell::RefCell<HashMap<i64, String>> =
        std::cell::RefCell::new(HashMap::new());
    static PENDING_MAIL_NOTIFICATIONS: std::cell::RefCell<HashSet<i64>> =
        std::cell::RefCell::new(HashSet::new());
}

fn set_pending_notification_otp(id: i64, code: &str) {
    PENDING_NOTIFICATION_OTP.with(|slot| {
        slot.borrow_mut().insert(id, code.to_string());
    });
}

/// Stored verification code for a live OTP notification, if any.
pub fn pending_notification_otp(id: i64) -> Option<String> {
    PENDING_NOTIFICATION_OTP.with(|slot| slot.borrow().get(&id).cloned())
}

pub fn clear_pending_notification_otp() {
    PENDING_NOTIFICATION_OTP.with(|slot| slot.borrow_mut().clear());
}

pub fn clear_pending_notification_otp_if(id: i64) -> bool {
    PENDING_NOTIFICATION_OTP.with(|slot| slot.borrow_mut().remove(&id).is_some())
}

/// Copy a notified verification code immediately (notification action handler).
pub fn copy_pending_notification_otp(id: i64) -> bool {
    let Some(code) = pending_notification_otp(id) else {
        return false;
    };
    copy_text_to_clipboard(&code);
    true
}

#[cfg(target_os = "linux")]
mod imp {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::time::Duration;

    use relm4::adw;
    use relm4::adw::prelude::*;
    use relm4::gtk;
    use relm4::gtk::gdk;
    use relm4::gtk::gdk::prelude::{DisplayExt, DisplayExtManual, ToplevelExt};
    use relm4::gtk::gio;
    use relm4::gtk::glib;
    use relm4::gtk::prelude::{GtkApplicationExt, GtkWindowExt, NativeExt, WidgetExt};

    use alcedo_core::{Envelope, MessageId};

    use crate::menu::{
        ACTION_COPY_VERIFICATION_CODE, ACTION_DELETE_NOTIFIED_MESSAGE,
        ACTION_MARK_NOTIFIED_MESSAGE_READ, ACTION_OPEN_MESSAGE,
    };

    use super::{
        UnreadAttention, classify_unread_attention, notification_body, notification_title,
    };

    const NOTIFICATION_ID: &str = "new-mail";
    const DESKTOP_URIS: &[&str] = &[
        "application://dev.alcedo.Mail.desktop",
        "application://dev.alcedo.Mail",
    ];
    const LAUNCHER_PATH: &str = "/com/canonical/unity/launcherentry/dev_alcedo_Mail";
    const LAUNCHER_INTERFACE: &str = "com.canonical.Unity.LauncherEntry";

    fn add_new_mail_action_buttons(notification: &gio::Notification, message_id: i64, otp: bool) {
        let target = message_id.to_variant();
        if otp {
            notification.add_button_with_target_value(
                super::notification_copy_label(),
                ACTION_COPY_VERIFICATION_CODE,
                Some(&target),
            );
        }
        notification.add_button_with_target_value(
            super::notification_mark_read_label(),
            ACTION_MARK_NOTIFIED_MESSAGE_READ,
            Some(&target),
        );
        notification.add_button_with_target_value(
            super::notification_delete_label(),
            ACTION_DELETE_NOTIFIED_MESSAGE,
            Some(&target),
        );
    }

    pub fn window_is_minimized(window: &impl IsA<gtk::Native>) -> bool {
        let Some(surface) = window.surface() else {
            return false;
        };
        let Ok(toplevel) = surface.dynamic_cast::<gdk::Toplevel>() else {
            return false;
        };
        toplevel.state().contains(gdk::ToplevelState::MINIMIZED)
    }

    pub fn is_attending_main(window: &adw::ApplicationWindow) -> bool {
        window.is_visible()
            && window.is_mapped()
            && !window_is_minimized(window.upcast_ref::<gtk::Native>())
            && window.is_active()
    }

    fn set_urgency_hint(window: &adw::ApplicationWindow, urgent: bool) {
        use glib::translate::IntoGlib;

        let Some(display) = relm4::gtk::gdk::Display::default() else {
            return;
        };
        if !display.backend().is_x11() {
            return;
        }
        let Some(surface) = window.surface() else {
            return;
        };

        unsafe extern "C" {
            fn gdk_x11_surface_set_urgency_hint(
                surface: *mut std::ffi::c_void,
                urgency: glib::ffi::gboolean,
            );
        }

        // SAFETY: the display backend was checked to be X11 above, so `surface`
        // is a live GdkX11Surface for as long as `window` holds it.
        unsafe {
            gdk_x11_surface_set_urgency_hint(surface.as_ptr() as *mut _, urgent.into_glib());
        }
    }

    pub fn apply_unread(
        window: &adw::ApplicationWindow,
        previous: Option<u32>,
        current: u32,
    ) -> UnreadAttention {
        let attending = is_attending_main(window);
        let result = classify_unread_attention(previous, current, attending);
        let urgent = result == UnreadAttention::DemandAttention;

        update_launcher(current, urgent);

        match result {
            UnreadAttention::DemandAttention => {
                set_urgency_hint(window, true);
            }
            UnreadAttention::Clear => clear_all(window),
            UnreadAttention::SilentUpdate => {}
        }

        result
    }

    pub fn clear_attention(window: &adw::ApplicationWindow, current: u32) {
        set_urgency_hint(window, false);
        update_launcher(current, false);
    }

    pub fn clear_all(window: &adw::ApplicationWindow) {
        set_urgency_hint(window, false);
        update_launcher(0, false);
        withdraw_all_mail_notifications();
        super::clear_pending_notification_otp();
    }

    const CLIPBOARD_CLAIM_NAME: &str = "alcedo-clipboard-claim";
    const ACTIVATION_SINK_NAME: &str = "alcedo-activation-sink";

    /// Consume a notification activation token without showing the main window.
    ///
    /// GNOME Shell shows a busy cursor until some surface uses the xdg-activation
    /// token from a notification action. Open presents the main window; Copy
    /// presents the clipboard-claim window. Delete / mark-read must still
    /// complete the token or the spinner lasts until Shell times out.
    pub fn complete_background_activation() {
        let Some(display) = gdk::Display::default() else {
            return;
        };
        #[allow(deprecated)]
        if display.startup_notification_id().is_none() {
            return;
        }

        if display.backend().is_x11() {
            #[allow(deprecated)]
            if let Some(id) = display.startup_notification_id() {
                display.notify_startup_complete(id.as_str());
            }
            return;
        }

        for window in relm4::main_application().windows() {
            let name = window.widget_name();
            if name.as_str() == CLIPBOARD_CLAIM_NAME || name.as_str() == ACTIVATION_SINK_NAME {
                continue;
            }
            if window.is_visible()
                && window.is_mapped()
                && window.is_active()
                && !window_is_minimized(&window)
            {
                if let Some(surface) = window.surface()
                    && let Ok(toplevel) = surface.dynamic_cast::<gdk::Toplevel>()
                {
                    #[allow(deprecated)]
                    if let Some(id) = display.startup_notification_id() {
                        toplevel.set_startup_id(id.as_str());
                    }
                }
                return;
            }
        }

        present_activation_sink();
    }

    fn present_activation_sink() {
        let win = gtk::Window::builder()
            .decorated(false)
            .resizable(false)
            .default_width(1)
            .default_height(1)
            .title("")
            .can_focus(true)
            .build();
        let focus = gtk::Box::builder().can_focus(true).build();
        win.set_child(Some(&focus));
        win.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
        win.set_widget_name(ACTIVATION_SINK_NAME);
        win.set_opacity(0.0);

        win.connect_notify_local(Some("is-active"), {
            let win = win.clone();
            move |window, _| {
                if !window.is_active() {
                    return;
                }
                let win = win.clone();
                glib::idle_add_local_once(move || {
                    win.close();
                });
            }
        });
        win.connect_map({
            let focus = focus.clone();
            move |_| {
                focus.grab_focus();
            }
        });
        glib::timeout_add_local_once(Duration::from_millis(300), {
            let win = win.clone();
            move || {
                win.close();
            }
        });
        win.present();
    }

    pub fn copy_text_to_clipboard(text: &str) {
        if let Some(display) = gdk::Display::default()
            && display.backend().is_x11()
        {
            display.clipboard().set_text(text);
            for window in relm4::main_application().windows() {
                window.clipboard().set_text(text);
            }
            return;
        }
        // Mutter (and other compositors) cancel set_selection from a client
        // that is not the keyboard-focus client. GTK `is-active` fires on
        // present() before that, so claiming here would drop the OTP and
        // leave the previous clipboard contents in place.
        if clipboard_owner_is_focused() {
            if let Some(display) = gdk::Display::default() {
                display.clipboard().set_text(text);
            }
            for window in relm4::main_application().windows() {
                window.clipboard().set_text(text);
            }
            return;
        }
        claim_wayland_clipboard(text);
    }

    fn clipboard_owner_is_focused() -> bool {
        relm4::main_application().windows().iter().any(|window| {
            let name = window.widget_name();
            name.as_str() != CLIPBOARD_CLAIM_NAME
                && name.as_str() != ACTIVATION_SINK_NAME
                && window.is_visible()
                && window.is_mapped()
                && !window_is_minimized(window)
                && clipboard_window_is_focused(window)
        })
    }

    fn clipboard_window_is_focused(window: &gtk::Window) -> bool {
        let Some(surface) = window.surface() else {
            return false;
        };
        let Ok(toplevel) = surface.dynamic_cast::<gdk::Toplevel>() else {
            return false;
        };
        toplevel.state().contains(gdk::ToplevelState::FOCUSED)
    }

    fn take_wayland_activation_token() -> Option<glib::GString> {
        let display = gdk::Display::default()?;
        #[allow(deprecated)]
        display.startup_notification_id()
    }

    fn apply_wayland_activation_token(window: &gtk::Window, token: Option<&str>) {
        let Some(token) = token.filter(|token| !token.is_empty()) else {
            return;
        };
        window.set_startup_id(token);
        if let Some(surface) = window.surface()
            && let Ok(toplevel) = surface.dynamic_cast::<gdk::Toplevel>()
        {
            toplevel.set_startup_id(token);
        }
    }

    thread_local! {
        static WAYLAND_CLIPBOARD_CLAIM: RefCell<Option<gtk::Window>> =
            const { RefCell::new(None) };
        static WAYLAND_CLIPBOARD_TEXT: RefCell<String> = const { RefCell::new(String::new()) };
        static WAYLAND_CLIPBOARD_PUBLISHED: Cell<bool> = const { Cell::new(false) };
    }

    fn publish_wayland_clipboard(window: &gtk::Window) {
        let text = WAYLAND_CLIPBOARD_TEXT.with(|slot| slot.borrow().clone());
        if text.is_empty() {
            return;
        }
        if let Some(display) = gdk::Display::default() {
            display.clipboard().set_text(&text);
        }
        window.clipboard().set_text(&text);
        WAYLAND_CLIPBOARD_PUBLISHED.set(true);
    }

    fn close_wayland_clipboard_claim(window: &gtk::Window) {
        WAYLAND_CLIPBOARD_CLAIM.with(|slot| slot.borrow_mut().take());
        WAYLAND_CLIPBOARD_TEXT.with(|slot| slot.borrow_mut().clear());
        WAYLAND_CLIPBOARD_PUBLISHED.set(false);
        window.close();
    }

    /// Wayland ignores clipboard claims without a focused surface. Use a 1×1
    /// window plus the notification's xdg-activation token so the main UI can
    /// stay in the background.
    fn claim_wayland_clipboard(text: &str) {
        WAYLAND_CLIPBOARD_TEXT.with(|slot| {
            *slot.borrow_mut() = text.to_string();
        });
        let token = take_wayland_activation_token();
        if let Some(win) = WAYLAND_CLIPBOARD_CLAIM.with(|slot| slot.borrow().clone()) {
            apply_wayland_activation_token(&win, token.as_deref());
            win.present();
            if clipboard_window_is_focused(&win) {
                publish_wayland_clipboard(&win);
            }
            return;
        }

        WAYLAND_CLIPBOARD_PUBLISHED.set(false);

        let win = gtk::Window::builder()
            .decorated(false)
            .resizable(false)
            .default_width(1)
            .default_height(1)
            .title("")
            .can_focus(true)
            .focusable(true)
            .build();
        let focus = gtk::Box::builder().can_focus(true).focusable(true).build();
        win.set_child(Some(&focus));
        win.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
        win.set_application(Some(&relm4::main_application()));
        win.set_widget_name(CLIPBOARD_CLAIM_NAME);
        apply_wayland_activation_token(&win, token.as_deref());

        let try_publish = Rc::new({
            let win = win.clone();
            move || {
                if !clipboard_window_is_focused(&win) {
                    return;
                }
                let first = !WAYLAND_CLIPBOARD_PUBLISHED.get();
                publish_wayland_clipboard(&win);
                if !first {
                    return;
                }
                let win = win.clone();
                glib::timeout_add_local_once(Duration::from_millis(300), move || {
                    close_wayland_clipboard_claim(&win);
                });
            }
        });

        win.connect_notify_local(Some("is-active"), {
            let try_publish = Rc::clone(&try_publish);
            move |_, _| try_publish()
        });
        win.connect_map({
            let focus = focus.clone();
            let try_publish = Rc::clone(&try_publish);
            let token = token.clone();
            move |window| {
                apply_wayland_activation_token(window, token.as_deref());
                focus.grab_focus();
                try_publish();
            }
        });
        win.connect_realize({
            let try_publish = Rc::clone(&try_publish);
            let token = token.clone();
            move |window: &gtk::Window| {
                apply_wayland_activation_token(window, token.as_deref());
                if let Some(surface) = window.surface()
                    && let Ok(toplevel) = surface.dynamic_cast::<gdk::Toplevel>()
                {
                    toplevel.connect_notify_local(Some("state"), {
                        let try_publish = Rc::clone(&try_publish);
                        move |_, _| try_publish()
                    });
                }
                try_publish();
            }
        });
        glib::timeout_add_local_once(Duration::from_secs(2), {
            let win = win.clone();
            move || {
                let still_claiming = WAYLAND_CLIPBOARD_CLAIM.with(|slot| {
                    slot.borrow()
                        .as_ref()
                        .is_some_and(|claimed| claimed == &win)
                });
                if still_claiming && !WAYLAND_CLIPBOARD_PUBLISHED.get() {
                    close_wayland_clipboard_claim(&win);
                }
            }
        });

        WAYLAND_CLIPBOARD_CLAIM.with(|slot| {
            *slot.borrow_mut() = Some(win.clone());
        });
        win.present();
        try_publish();
    }

    pub fn send_new_mail_notification(envelope: &Envelope, otp: Option<&str>, prominent: bool) {
        let notification = gio::Notification::new(&notification_title(envelope));
        let body = notification_body(envelope, otp);
        notification.set_body(Some(&body));
        notification.set_icon(&gio::ThemedIcon::new("dev.alcedo.Mail"));
        notification.set_priority(if prominent {
            gio::NotificationPriority::Normal
        } else {
            gio::NotificationPriority::Low
        });
        if let Some(code) = otp {
            super::set_pending_notification_otp(envelope.id.0, code);
        }
        notification.set_default_action_and_target_value(
            super::mail_notification_default_action(),
            Some(&envelope.id.0.to_variant()),
        );
        add_new_mail_action_buttons(&notification, envelope.id.0, otp.is_some());
        let id = super::mail_notification_id(envelope.id);
        super::PENDING_MAIL_NOTIFICATIONS.with(|slot| {
            slot.borrow_mut().insert(envelope.id.0);
        });
        let app = relm4::main_application();
        app.send_notification(Some(&id), &notification);
        app.withdraw_notification(NOTIFICATION_ID);
    }

    pub fn withdraw_message_notification(id: MessageId) {
        super::PENDING_MAIL_NOTIFICATIONS.with(|slot| {
            slot.borrow_mut().remove(&id.0);
        });
        let app = relm4::main_application();
        app.withdraw_notification(&super::mail_notification_id(id));
        if super::PENDING_MAIL_NOTIFICATIONS.with(|slot| slot.borrow().is_empty()) {
            app.withdraw_notification(NOTIFICATION_ID);
        }
    }

    pub fn withdraw_all_mail_notifications() {
        let ids =
            super::PENDING_MAIL_NOTIFICATIONS.with(|slot| std::mem::take(&mut *slot.borrow_mut()));
        let app = relm4::main_application();
        for id in ids {
            app.withdraw_notification(&super::mail_notification_id(MessageId(id)));
        }
        app.withdraw_notification(NOTIFICATION_ID);
    }

    pub fn send_named_notification(
        id: &str,
        title: &str,
        body: &str,
        open_message: Option<MessageId>,
    ) {
        let notification = gio::Notification::new(title);
        notification.set_body(Some(body));
        notification.set_icon(&gio::ThemedIcon::new("dev.alcedo.Mail"));
        notification.set_priority(gio::NotificationPriority::Normal);
        if let Some(message_id) = open_message {
            notification.set_default_action_and_target_value(
                ACTION_OPEN_MESSAGE,
                Some(&message_id.0.to_variant()),
            );
        }
        relm4::main_application().send_notification(Some(id), &notification);
    }

    thread_local! {
        static LAST_LAUNCHER_STATE: Cell<Option<(u32, bool)>> = const { Cell::new(None) };
    }

    fn update_launcher(unread: u32, urgent: bool) {
        let state = Some((unread, urgent));
        if LAST_LAUNCHER_STATE.with(|last| last.replace(state)) == state {
            return;
        }

        let hints = glib::VariantDict::new(None);
        hints.insert("count", i64::from(unread));
        hints.insert("count-visible", unread > 0);
        hints.insert("urgent", urgent);
        let properties = hints.end();

        let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) else {
            tracing::debug!("failed to connect to session bus for Unity launcher badge");
            return;
        };

        for uri in DESKTOP_URIS {
            // Unity expects (sa{sv}); a tuple of Variant would wrap the dict as (sv).
            let params = glib::Variant::tuple_from_iter([uri.to_variant(), properties.clone()]);
            if let Err(error) = bus.emit_signal(
                None,
                LAUNCHER_PATH,
                LAUNCHER_INTERFACE,
                "Update",
                Some(&params),
            ) {
                tracing::debug!(%error, uri, "failed to update Unity launcher badge");
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use relm4::adw;
    use relm4::gtk;
    use relm4::gtk::prelude::{GtkWindowExt, IsA};

    use alcedo_core::{Envelope, MessageId};

    use super::{UnreadAttention, classify_unread_attention};

    pub fn window_is_minimized(_window: &impl IsA<gtk::Native>) -> bool {
        false
    }

    pub fn is_attending_main(window: &adw::ApplicationWindow) -> bool {
        window.is_active()
    }

    pub fn apply_unread(
        window: &adw::ApplicationWindow,
        previous: Option<u32>,
        current: u32,
    ) -> UnreadAttention {
        classify_unread_attention(previous, current, is_attending_main(window))
    }

    pub fn clear_attention(_window: &adw::ApplicationWindow, _current: u32) {}

    pub fn clear_all(_window: &adw::ApplicationWindow) {
        super::clear_pending_notification_otp();
    }

    pub fn copy_text_to_clipboard(_text: &str) {}

    pub fn complete_background_activation() {}

    pub fn send_new_mail_notification(_envelope: &Envelope, _otp: Option<&str>, _prominent: bool) {}

    pub fn withdraw_message_notification(_id: MessageId) {}

    pub fn withdraw_all_mail_notifications() {}

    pub fn send_named_notification(
        _id: &str,
        _title: &str,
        _body: &str,
        _open_message: Option<MessageId>,
    ) {
    }
}

pub use imp::{
    apply_unread, clear_all, clear_attention, complete_background_activation,
    copy_text_to_clipboard, is_attending_main, send_named_notification, send_new_mail_notification,
    window_is_minimized, withdraw_all_mail_notifications, withdraw_message_notification,
};

#[cfg(test)]
mod tests {
    use alcedo_core::{AccountId, Address, FolderId, MessageId, ThreadId};
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    use super::*;

    fn sample_envelope(
        id: i64,
        seen: bool,
        date: chrono::DateTime<Utc>,
        from: Vec<Address>,
        subject: &str,
    ) -> Envelope {
        Envelope {
            id: MessageId(id),
            account_id: AccountId(Uuid::from_u128(1)),
            folder_id: FolderId(1),
            uid: 1,
            thread_id: ThreadId(Uuid::from_u128(2)),
            date,
            subject: subject.to_string(),
            from,
            to: vec![],
            cc: vec![],
            reply_to: Vec::new(),
            sender: Vec::new(),
            routing: None,
            snippet: String::new(),
            seen,
            flagged: false,
            answered: false,
            draft: false,
            has_attachments: false,
            deleted_at: None,
            account_accent: String::new(),
            account_avatar: String::new(),
            size_bytes: 0,
        }
    }

    #[test]
    fn classify_baseline_is_silent_update() {
        assert_eq!(
            classify_unread_attention(None, 7, false),
            UnreadAttention::SilentUpdate
        );
        assert_eq!(
            classify_unread_attention(None, 0, false),
            UnreadAttention::SilentUpdate
        );
    }

    #[test]
    fn classify_increase_while_not_attending_demands_attention() {
        assert_eq!(
            classify_unread_attention(Some(0), 1, false),
            UnreadAttention::DemandAttention
        );
    }

    #[test]
    fn classify_increase_while_attending_is_silent() {
        assert_eq!(
            classify_unread_attention(Some(2), 5, true),
            UnreadAttention::SilentUpdate
        );
    }

    #[test]
    fn classify_decrease_with_remaining_unread_is_silent() {
        assert_eq!(
            classify_unread_attention(Some(5), 3, false),
            UnreadAttention::SilentUpdate
        );
    }

    #[test]
    fn classify_unread_cleared() {
        assert_eq!(
            classify_unread_attention(Some(3), 0, false),
            UnreadAttention::Clear
        );
    }

    #[test]
    fn classify_zero_to_zero_is_silent() {
        assert_eq!(
            classify_unread_attention(Some(0), 0, false),
            UnreadAttention::SilentUpdate
        );
    }

    #[test]
    fn additional_account_baseline_must_pass_none_previous() {
        // Per-account UnreadCounts can raise unified unread from 5 to 8 at
        // startup. Callers must pass previous=None for a first-seen account.
        assert_eq!(
            classify_unread_attention(Some(5), 8, false),
            UnreadAttention::DemandAttention
        );
        assert_eq!(
            classify_unread_attention(None, 8, false),
            UnreadAttention::SilentUpdate
        );
    }

    #[test]
    fn observe_inbox_batch_partial_unified_does_not_mark_ready() {
        let folder = FolderId(1);
        let mut known = HashSet::new();
        let mut ready = HashSet::new();
        let early = Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap();
        let partial = [sample_envelope(1, false, early, vec![], "partial")];

        assert!(observe_inbox_batch(&mut known, &mut ready, folder, &partial, false).is_empty());
        assert!(ready.is_empty());
        assert!(known.contains(&MessageId(1)));

        let older = sample_envelope(2, false, early, vec![], "older unseen");
        assert!(
            observe_inbox_batch(&mut known, &mut ready, folder, &[older], false).is_empty(),
            "still not ready without full folder sync"
        );
    }

    #[test]
    fn observe_inbox_batch_full_folder_sync_baselines_without_notify() {
        let folder = FolderId(1);
        let mut known = HashSet::from([MessageId(1)]);
        let mut ready = HashSet::new();
        let early = Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap();
        let late = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let full = [
            sample_envelope(1, false, early, vec![], "known from unified"),
            sample_envelope(2, false, early, vec![], "older unseen"),
            sample_envelope(3, false, late, vec![], "another unseen"),
        ];

        assert!(observe_inbox_batch(&mut known, &mut ready, folder, &full, true).is_empty());
        assert!(ready.contains(&folder));
        assert!(known.contains(&MessageId(2)));
        assert!(known.contains(&MessageId(3)));
    }

    #[test]
    fn observe_inbox_batch_notifies_after_ready() {
        let folder = FolderId(1);
        let mut known = HashSet::from([MessageId(1), MessageId(2)]);
        let mut ready = HashSet::from([folder]);
        let late = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let incoming = [
            sample_envelope(3, false, late, vec![], "arrived first"),
            sample_envelope(
                4,
                false,
                late + chrono::Duration::seconds(1),
                vec![],
                "arrived second",
            ),
            sample_envelope(
                5,
                false,
                late + chrono::Duration::seconds(2),
                vec![],
                "arrived third",
            ),
            sample_envelope(
                6,
                false,
                late + chrono::Duration::seconds(3),
                vec![],
                "arrived fourth",
            ),
        ];

        let notified = observe_inbox_batch(&mut known, &mut ready, folder, &incoming, true);
        assert_eq!(notified.len(), 4);
        assert_eq!(notified.last().unwrap().id, MessageId(6));
    }

    #[test]
    fn newest_new_unseen_ignores_known_and_seen() {
        let known = HashSet::from([MessageId(1)]);
        let early = Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap();
        let late = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let incoming = [
            sample_envelope(1, false, early, vec![], "old known"),
            sample_envelope(2, true, late, vec![], "seen"),
            sample_envelope(3, false, early, vec![], "new early"),
            sample_envelope(4, false, late, vec![], "new late"),
        ];
        let newest = newest_new_unseen(&known, &incoming).expect("new unseen");
        assert_eq!(newest.id, MessageId(4));
    }

    #[test]
    fn newest_new_unseen_empty_when_none() {
        let known = HashSet::from([MessageId(1)]);
        let incoming = [sample_envelope(1, false, Utc::now(), vec![], "known")];
        assert!(newest_new_unseen(&known, &incoming).is_none());
        assert!(newest_new_unseen(&HashSet::new(), &[]).is_none());
    }

    #[test]
    fn notification_title_uses_display_name() {
        let envelope = sample_envelope(
            1,
            false,
            Utc::now(),
            vec![Address {
                name: Some("Alice".to_string()),
                email: "alice@example.com".to_string(),
            }],
            "Hi",
        );
        assert_eq!(notification_title(&envelope), "Alice");
    }

    #[test]
    fn notification_title_empty_from_is_new_mail() {
        let envelope = sample_envelope(1, false, Utc::now(), vec![], "Hi");
        assert_eq!(notification_title(&envelope), "New mail");
    }

    #[test]
    fn notification_subject_body_empty_is_no_subject() {
        let envelope = sample_envelope(1, false, Utc::now(), vec![], "   ");
        assert_eq!(
            notification_subject_body(&envelope),
            alcedo_core::NO_SUBJECT
        );
    }

    #[test]
    fn notification_subject_body_uses_subject() {
        let envelope = sample_envelope(1, false, Utc::now(), vec![], "Hello");
        assert_eq!(notification_subject_body(&envelope), "Hello");
    }

    #[test]
    fn notification_otp_code_extracts_from_subject_and_snippet() {
        let code = notification_otp_code("GitHub verification code", "Your code is 482193");
        assert_eq!(code.as_deref(), Some("482193"));
        let mut envelope =
            sample_envelope(1, false, Utc::now(), vec![], "GitHub verification code");
        envelope.snippet = "Your code is 482193".to_string();
        assert_eq!(
            notification_body(&envelope, code.as_deref()),
            "Verification code\n482 193"
        );
        assert_eq!(notification_copy_label(), "Copy");
        assert_eq!(notification_mark_read_label(), "Mark as read");
        assert_eq!(notification_delete_label(), "Delete");
        assert_eq!(
            mail_notification_default_action(),
            crate::menu::ACTION_OPEN_MESSAGE
        );
    }

    #[test]
    fn notification_otp_code_none_for_ordinary_mail() {
        let subject = "Invoice attached";
        let snippet = "Hello from accounting, invoice 2024";
        assert!(notification_otp_code(subject, snippet).is_none());
        let envelope = sample_envelope(1, false, Utc::now(), vec![], subject);
        assert_eq!(
            notification_body(&envelope, None),
            notification_subject_body(&envelope)
        );
    }

    #[test]
    fn notification_otp_code_finds_code_past_list_preview() {
        let branding = "Microsoft account privacy and security notice. ".repeat(8);
        assert!(branding.chars().count() > 140);
        let snippet = format!("{branding} Your code is 482193");
        let code = notification_otp_code(
            "Your AJM Software Group LLC account verification code",
            &snippet,
        );
        assert_eq!(code.as_deref(), Some("482193"));
        let mut envelope = sample_envelope(
            1,
            false,
            Utc::now(),
            vec![],
            "Your AJM Software Group LLC account verification code",
        );
        envelope.snippet = snippet;
        assert_eq!(
            notification_body(&envelope, code.as_deref()),
            "Verification code\n482 193"
        );
    }

    #[test]
    fn pending_otp_roundtrip() {
        clear_pending_notification_otp();
        set_pending_notification_otp(7, "482193");
        assert_eq!(pending_notification_otp(7).as_deref(), Some("482193"));
        assert!(!clear_pending_notification_otp_if(8));
        assert_eq!(pending_notification_otp(7).as_deref(), Some("482193"));
        set_pending_notification_otp(9, "111111");
        assert_eq!(pending_notification_otp(7).as_deref(), Some("482193"));
        assert_eq!(pending_notification_otp(9).as_deref(), Some("111111"));
        assert!(clear_pending_notification_otp_if(7));
        assert!(pending_notification_otp(7).is_none());
        assert_eq!(pending_notification_otp(9).as_deref(), Some("111111"));
        clear_pending_notification_otp();
        assert!(pending_notification_otp(9).is_none());
    }
}
