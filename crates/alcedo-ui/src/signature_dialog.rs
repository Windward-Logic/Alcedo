//! Visual HTML signature editor dialog.

use alcedo_i18n::tr;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use alcedo_core::{AccountId, AccountSignature, ComposeFormat, ComposeInline};
use alcedo_html::{
    markdown_to_html, plaintext_to_fragment, sanitize_signature_html, signature_plain_mail_preview,
};
use relm4::Sender;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::glib;
use relm4::gtk::prelude::{ButtonExt, TextBufferExt, TextViewExt, ToggleButtonExt, WidgetExt};

use crate::compose_attachments::{
    ComposePartsState, inline_image_client_warning, insert_html_inline_at_cursor,
    load_inline_image, open_attach_dialog, referenced_inline_cids_html, show_composer_toast,
};
use crate::compose_html::ComposeHtmlEditor;
use crate::compose_toolbar::{
    append_html_format_handles, build_html_format_handles, install_signature_shortcuts,
    set_html_format_sensitive, wire_html_format_actions,
};
use crate::mail::MailMsg;

fn initial_signature_source(signature: Option<&AccountSignature>) -> String {
    let Some(sig) = signature else {
        return String::new();
    };
    if sig.body.trim().is_empty() {
        return String::new();
    }
    match sig.format {
        ComposeFormat::Plain => plaintext_to_fragment(&sig.body),
        ComposeFormat::Markdown => markdown_to_html(&sig.body),
        ComposeFormat::Html => sig.body.clone(),
    }
}

pub fn open_signature_dialog(
    parent: &adw::PreferencesDialog,
    account_id: AccountId,
    sender: &Sender<MailMsg>,
    signature: Option<AccountSignature>,
) {
    let parts = ComposePartsState::new();
    let order: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(
        signature
            .as_ref()
            .map(|sig| sig.inlines.iter().map(|i| i.content_id.clone()).collect())
            .unwrap_or_default(),
    ));
    if let Some(sig) = signature.as_ref() {
        for inline in &sig.inlines {
            parts
                .inlines
                .borrow_mut()
                .insert(inline.content_id.clone(), inline.clone());
        }
    }

    let format = Rc::new(Cell::new(ComposeFormat::Html));
    let save_in_flight = Rc::new(Cell::new(false));
    let save_discarded = Rc::new(Cell::new(false));
    let preview_source: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));

    let cancel_btn = gtk::Button::builder().label(tr!("Cancel")).build();
    let save_btn = gtk::Button::builder()
        .label(tr!("Save"))
        .css_classes(["suggested-action"])
        .build();
    let header = adw::HeaderBar::new();
    header.pack_start(&cancel_btn);
    header.pack_end(&save_btn);

    let format_bar = adw::WrapBox::builder()
        .hexpand(true)
        .halign(gtk::Align::Fill)
        .child_spacing(2)
        .line_spacing(2)
        .css_classes(["alcedo-compose-toolbar"])
        .build();
    format_bar
        .upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Label(&tr!("Formatting toolbar"))]);
    let handles = Rc::new(build_html_format_handles());
    append_html_format_handles(&format_bar, &handles, true);

    let body_view = gtk::TextView::new();
    let html_editor = Rc::new(ComposeHtmlEditor::new(&body_view));

    let plain_label = gtk::Label::builder()
        .label(tr!("Plain text mail"))
        .xalign(0.0)
        .build();
    let plain_preview = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .wrap_mode(gtk::WrapMode::Word)
        .build();
    plain_preview.update_property(&[gtk::accessible::Property::Label(&tr!(
        "Plain text mail preview"
    ))]);
    let plain_preview_scroll = gtk::ScrolledWindow::builder()
        .min_content_height(80)
        .vexpand(false)
        .child(&plain_preview)
        .build();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    html_editor.stack.set_hexpand(true);
    html_editor.stack.set_vexpand(true);
    content.append(&format_bar);
    content.append(&html_editor.stack);
    content.append(&plain_label);
    content.append(&plain_preview_scroll);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&content));

    let toast_host = toast_overlay.clone().upcast::<gtk::Widget>();

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&toast_overlay));

    let current_allowed_cids = {
        let parts = parts.clone();
        let order = order.clone();
        Rc::new(move || {
            let map = parts.inlines.borrow();
            order
                .borrow()
                .iter()
                .filter(|id| map.contains_key(id.as_str()))
                .cloned()
                .collect::<Vec<_>>()
        })
    };

    let update_plain_preview = {
        let html_editor = html_editor.clone();
        let allowed = current_allowed_cids.clone();
        let plain_preview = plain_preview.clone();
        move || {
            let raw = if html_editor.is_source_visible() {
                html_editor.source_text()
            } else {
                html_editor.snapshot().borrow().clone()
            };
            let plain = signature_plain_mail_preview(&sanitize_signature_html(&raw, &allowed()));
            plain_preview.buffer().set_text(&plain);
        }
    };

    let schedule_previews = {
        let preview_source = preview_source.clone();
        let update_plain_preview = update_plain_preview.clone();
        Rc::new(move || {
            crate::glib_source::cancel_source(&preview_source);
            let update = update_plain_preview.clone();
            let preview_source_cb = preview_source.clone();
            let id = glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
                let _ = preview_source_cb.take();
                update();
                glib::ControlFlow::Break
            });
            preview_source.set(Some(id));
        })
    };

    let on_source_changed = {
        let handles = handles.clone();
        let schedule_previews = schedule_previews.clone();
        let update_plain_preview = update_plain_preview.clone();
        Rc::new(move || {
            set_html_format_sensitive(&handles, !handles.source_btn.is_active());
            schedule_previews();
            update_plain_preview();
        })
    };

    let on_insert_image = {
        let parts = parts.clone();
        let html_editor = html_editor.clone();
        let toast_host = toast_host.clone();
        let order = order.clone();
        let schedule_previews = schedule_previews.clone();
        let body_view = body_view.clone();
        Rc::new(move || {
            open_attach_dialog(&toast_host, false, true, {
                let parts = parts.clone();
                let html_editor = html_editor.clone();
                let toast_host = toast_host.clone();
                let order = order.clone();
                let schedule_previews = schedule_previews.clone();
                let body_view = body_view.clone();
                move |paths| {
                    let Some(path) = paths.into_iter().next() else {
                        return;
                    };
                    let parts = parts.clone();
                    let html_editor = html_editor.clone();
                    let toast_host = toast_host.clone();
                    let order = order.clone();
                    let schedule_previews = schedule_previews.clone();
                    let body_view = body_view.clone();
                    glib::spawn_future_local(async move {
                        let inline = match load_inline_image(path).await {
                            Ok(inline) => inline,
                            Err(message) => {
                                show_composer_toast(&toast_host, message);
                                return;
                            }
                        };
                        let Some(inline) = parts.try_add_inline(
                            inline,
                            &toast_host,
                            &body_view,
                            ComposeFormat::Html,
                            &html_editor,
                        ) else {
                            return;
                        };
                        if !insert_html_inline_at_cursor(&html_editor, &inline) {
                            parts.inlines.borrow_mut().remove(&inline.content_id);
                            show_composer_toast(&toast_host, &tr!("Could not read file"));
                            return;
                        }
                        if let Some(warning) = inline_image_client_warning(&inline.filename) {
                            show_composer_toast(&toast_host, &warning);
                        }
                        {
                            let mut ord = order.borrow_mut();
                            ord.retain(|id| parts.inlines.borrow().contains_key(id));
                            if !ord.contains(&inline.content_id) {
                                ord.push(inline.content_id.clone());
                            }
                        }
                        schedule_previews();
                    });
                }
            });
        })
    };

    let sanitize_leaving = {
        let allowed = current_allowed_cids.clone();
        Rc::new(move |text: String| sanitize_signature_html(&text, &allowed()))
    };

    wire_html_format_actions(
        &handles,
        &body_view,
        html_editor.clone(),
        parts.inlines.clone(),
        format.clone(),
        on_insert_image,
        sanitize_leaving,
        on_source_changed.clone(),
    );

    set_html_format_sensitive(&handles, true);

    install_signature_shortcuts(&toolbar_view, &html_editor, &handles.source_btn);

    html_editor.set_on_input(Rc::new({
        let schedule_previews = schedule_previews.clone();
        move || schedule_previews()
    }));

    let initial = initial_signature_source(signature.as_ref());
    {
        let inlines = parts.inlines.borrow();
        html_editor.sync_cid_cache(&inlines);
        let cid_map = html_editor.cid_to_sha256_map(&inlines);
        html_editor.set_html_display(&initial, &cid_map, true);
        html_editor.show_html();
    }
    update_plain_preview();

    let dialog = adw::Dialog::builder()
        .title(tr!("Signature"))
        .content_width(560)
        .content_height(720)
        .default_widget(&save_btn)
        .child(&toolbar_view)
        .build();

    let commit_save = {
        let html_editor = html_editor.clone();
        let handles = handles.source_btn.clone();
        let parts = parts.clone();
        let order = order.clone();
        let allowed = current_allowed_cids.clone();
        let sender = sender.clone();
        let dialog = dialog.clone();
        let preview_source = preview_source.clone();
        let save_in_flight = save_in_flight.clone();
        let save_discarded = save_discarded.clone();
        let toast_overlay = toast_overlay.clone();
        Rc::new(move || {
            if save_discarded.get() {
                save_in_flight.set(false);
                return;
            }
            let raw = if handles.is_active() {
                html_editor.source_text()
            } else {
                html_editor.snapshot().borrow().clone()
            };
            let sanitized = sanitize_signature_html(&raw, &allowed());
            let referenced = referenced_inline_cids_html(&sanitized);
            let map = parts.inlines.borrow();
            let inlines: Vec<ComposeInline> = order
                .borrow()
                .iter()
                .filter(|id| referenced.contains(id.as_str()))
                .filter_map(|id| map.get(id).cloned())
                .collect();
            let signature = AccountSignature {
                account_id,
                format: ComposeFormat::Html,
                body: sanitized,
                inlines,
            };
            if let Err(message) = signature.validate() {
                save_in_flight.set(false);
                toast_overlay.add_toast(crate::toast::plain(message));
                return;
            }
            let _ = sender.send(MailMsg::UpsertSignature(signature));
            crate::glib_source::cancel_source(&preview_source);
            save_in_flight.set(false);
            dialog.close();
        })
    };

    save_btn.connect_clicked({
        let html_editor = html_editor.clone();
        let handles = handles.source_btn.clone();
        let save_in_flight = save_in_flight.clone();
        let commit_save = commit_save.clone();
        move |_| {
            if save_in_flight.get() {
                return;
            }
            save_in_flight.set(true);
            if handles.is_active() {
                commit_save();
                return;
            }
            let commit = commit_save.clone();
            html_editor.flush_html(move || commit());
        }
    });

    cancel_btn.connect_clicked({
        let dialog = dialog.clone();
        let preview_source = preview_source.clone();
        let save_discarded = save_discarded.clone();
        move |_| {
            save_discarded.set(true);
            crate::glib_source::cancel_source(&preview_source);
            dialog.close();
        }
    });
    dialog.connect_closed({
        let preview_source = preview_source.clone();
        let save_discarded = save_discarded.clone();
        move |_| {
            save_discarded.set(true);
            crate::glib_source::cancel_source(&preview_source);
        }
    });

    dialog.present(Some(parent));
}
