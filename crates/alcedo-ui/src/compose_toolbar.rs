//! Compose formatting toolbar (plain / HTML / Markdown).

use alcedo_i18n::{tr, trs};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use alcedo_core::{ComposeFormat, ComposeInline, Snippet, SnippetId};
use alcedo_html::{
    convert_compose_format, looks_like_markdown, markdown_to_html, sanitize_compose_html,
};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::gdk::Key;
use relm4::gtk::glib;
use relm4::gtk::prelude::{ButtonExt, Cast, TextBufferExt, ToggleButtonExt, WidgetExt};

use crate::compose_attachments::drain_html_inline_images;
use crate::compose_html::{ComposeHtmlEditor, ComposeViewMode, pin_ancestor_vscroll_to_top};
use crate::compose_snippets::build_snippets_popover;
use crate::settings;

#[derive(Clone)]
pub struct ComposeToolbar {
    pub root: adw::WrapBox,
    pub format_dropdown: gtk::DropDown,
    html_editor: Rc<ComposeHtmlEditor>,
    inlines: Rc<RefCell<HashMap<String, ComposeInline>>>,
    format: Rc<Cell<ComposeFormat>>,
    source_btn: gtk::ToggleButton,
    preview_btn: gtk::ToggleButton,
    style_sep: gtk::Separator,
    block_sep: gtk::Separator,
    html_block_buttons: Vec<gtk::Widget>,
    html_insert_buttons: Vec<gtk::Button>,
    style_toggles: Vec<gtk::ToggleButton>,
    md_buttons: Vec<gtk::Button>,
    link_btn: gtk::Button,
    insert_image_btn: gtk::Button,
    body_view: gtk::TextView,
    pub include_signature: gtk::ToggleButton,
    pub snippets_btn: gtk::MenuButton,
    snippets_refresh: Rc<RefCell<Option<Rc<dyn Fn(&[Snippet])>>>>,
}

impl ComposeToolbar {
    pub fn current_format(&self) -> ComposeFormat {
        self.format.get()
    }

    pub fn html_editor(&self) -> &ComposeHtmlEditor {
        &self.html_editor
    }

    pub fn set_format(&self, body_frame: &gtk::Frame, format: ComposeFormat) {
        // Set the cell before the dropdown so notify::selected sees previous == fmt
        // and skips body conversion (callers like apply_body supply the text).
        self.format.set(format);
        self.format_dropdown.set_selected(format.dropdown_index());
        self.apply_format_ui(body_frame, format);
    }

    /// Switch format like the toolbar dropdown, converting the current body when needed.
    pub fn select_format(&self, format: ComposeFormat) {
        self.format_dropdown.set_selected(format.dropdown_index());
    }

    fn apply_format_ui(&self, body_frame: &gtk::Frame, format: ComposeFormat) {
        self.format.set(format);
        update_body_frame_class(body_frame, format);
        self.update_button_visibility();
    }

    pub fn body_text_for_send(&self, body_view: &gtk::TextView) -> String {
        match self.format.get() {
            ComposeFormat::Plain | ComposeFormat::Markdown => body_view
                .buffer()
                .text(
                    &body_view.buffer().start_iter(),
                    &body_view.buffer().end_iter(),
                    false,
                )
                .to_string(),
            ComposeFormat::Html => {
                if self.html_editor.is_source_visible() {
                    sanitize_compose_html(&self.html_editor.source_text())
                } else {
                    sanitize_compose_html(&self.html_editor.snapshot().borrow())
                }
            }
        }
    }

    pub fn apply_body(
        &self,
        body_view: &gtk::TextView,
        body_frame: &gtk::Frame,
        format: ComposeFormat,
        text: &str,
        focus_at_start: bool,
    ) {
        self.source_btn.set_active(false);
        self.preview_btn.set_active(false);
        self.set_format(body_frame, format);
        let buffer = body_view.buffer();
        buffer.delete(&mut buffer.start_iter(), &mut buffer.end_iter());
        if format == ComposeFormat::Html {
            let inlines = self.inlines.borrow();
            self.html_editor.sync_cid_cache(&inlines);
            let cid_map = self.html_editor.cid_to_sha256_map(&inlines);
            let sanitized = sanitize_compose_html(text);
            self.html_editor
                .set_html_display(&sanitized, &cid_map, focus_at_start);
            self.html_editor.show_html();
        } else {
            buffer.set_text(text);
            if focus_at_start {
                let iter = buffer.start_iter();
                buffer.place_cursor(&iter);
                let body_view = body_view.clone();
                glib::idle_add_local(move || {
                    body_view.grab_focus();
                    glib::ControlFlow::Break
                });
            }
            self.html_editor.show_text();
        }
        if focus_at_start {
            let anchor: gtk::Widget = if format == ComposeFormat::Html {
                self.html_editor.web_view.clone().upcast()
            } else {
                body_view.clone().upcast()
            };
            let html_editor = self.html_editor.clone();
            pin_ancestor_vscroll_to_top(&anchor, move || {
                if format == ComposeFormat::Html {
                    html_editor.scroll_document_to_start();
                }
            });
        }
    }

    fn update_button_visibility(&self) {
        let fmt = self.format.get();
        let style_enabled = fmt != ComposeFormat::Plain;
        let html = fmt == ComposeFormat::Html;
        let md = fmt == ComposeFormat::Markdown;

        for btn in &self.style_toggles {
            btn.set_sensitive(style_enabled);
            btn.set_visible(html || md);
        }
        self.link_btn.set_sensitive(style_enabled);
        self.link_btn.set_visible(html || md);
        self.insert_image_btn.set_sensitive(style_enabled);
        self.insert_image_btn.set_visible(html || md);

        for btn in &self.html_block_buttons {
            btn.set_visible(html);
        }
        for btn in &self.html_insert_buttons {
            btn.set_visible(html);
        }
        self.source_btn.set_visible(html);

        for btn in &self.md_buttons {
            btn.set_visible(md);
        }

        let style_visible = html || md;
        self.style_sep.set_visible(style_visible);
        self.block_sep.set_visible(html);

        self.update_preview_visibility();
    }

    fn update_preview_visibility(&self) {
        let fmt = self.format.get();
        let show = match fmt {
            ComposeFormat::Markdown => true,
            ComposeFormat::Plain => {
                let buffer = self.body_view.buffer();
                let text = buffer
                    .text(&buffer.start_iter(), &buffer.end_iter(), false)
                    .to_string();
                looks_like_markdown(&text)
            }
            ComposeFormat::Html => false,
        };
        self.preview_btn.set_visible(show);
        if fmt == ComposeFormat::Plain && show {
            self.preview_btn.set_tooltip_text(Some(&tr!(
                "Preview only. Switch the format dropdown to Markdown to send a formatted message."
            )));
        } else if show {
            self.preview_btn
                .set_tooltip_text(Some(&tr!("Preview (Ctrl+Shift+P)")));
        }
    }

    fn set_preview_active(&self, active: bool) {
        if active {
            self.source_btn.set_active(false);
            let buffer = self.body_view.buffer();
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
            self.html_editor.load_preview(&markdown_to_html(&text));
            self.html_editor.set_view_mode(ComposeViewMode::Preview);
        } else if self.format.get() == ComposeFormat::Html {
            self.html_editor.show_html();
            self.html_editor.focus_editor();
        } else {
            self.html_editor.show_text();
            self.body_view.grab_focus();
        }
        self.update_formatting_sensitivity();
    }

    fn update_formatting_sensitivity(&self) {
        let source_on = self.source_btn.is_active();
        let preview_on = self.preview_btn.is_active();
        let disabled = source_on || preview_on;
        let fmt = self.format.get();

        for btn in &self.style_toggles {
            btn.set_sensitive(!disabled && fmt != ComposeFormat::Plain);
        }
        self.link_btn
            .set_sensitive(!disabled && fmt != ComposeFormat::Plain);
        self.insert_image_btn
            .set_sensitive(!disabled && fmt != ComposeFormat::Plain);
        for btn in &self.html_block_buttons {
            btn.set_sensitive(!disabled);
        }
        for btn in &self.html_insert_buttons {
            btn.set_sensitive(!disabled);
        }
        for btn in &self.md_buttons {
            btn.set_sensitive(!disabled);
        }
        self.include_signature.set_sensitive(!disabled);
        self.snippets_btn.set_sensitive(!disabled);
        self.source_btn
            .set_sensitive(!preview_on && fmt == ComposeFormat::Html);
        self.preview_btn.set_sensitive(!source_on);
    }

    pub fn refresh_snippets(&self, snippets: &[Snippet]) {
        if let Some(refresh) = self.snippets_refresh.borrow().as_ref() {
            refresh(snippets);
        }
    }
}

pub(crate) struct HtmlFormatHandles {
    pub bold_btn: gtk::ToggleButton,
    pub italic_btn: gtk::ToggleButton,
    pub underline_btn: gtk::ToggleButton,
    pub strike_btn: gtk::ToggleButton,
    pub style_sep: gtk::Separator,
    pub bullet_btn: gtk::Button,
    pub number_btn: gtk::Button,
    pub quote_btn: gtk::Button,
    pub heading_btn: gtk::MenuButton,
    pub spacing_btn: gtk::MenuButton,
    pub block_sep: gtk::Separator,
    pub link_btn: gtk::Button,
    pub insert_image_btn: gtk::Button,
    pub table_btn: gtk::Button,
    pub hr_btn: gtk::Button,
    pub code_btn: gtk::Button,
    pub clear_btn: gtk::Button,
    pub source_btn: gtk::ToggleButton,
}

impl Clone for HtmlFormatHandles {
    fn clone(&self) -> Self {
        Self {
            bold_btn: self.bold_btn.clone(),
            italic_btn: self.italic_btn.clone(),
            underline_btn: self.underline_btn.clone(),
            strike_btn: self.strike_btn.clone(),
            style_sep: self.style_sep.clone(),
            bullet_btn: self.bullet_btn.clone(),
            number_btn: self.number_btn.clone(),
            quote_btn: self.quote_btn.clone(),
            heading_btn: self.heading_btn.clone(),
            spacing_btn: self.spacing_btn.clone(),
            block_sep: self.block_sep.clone(),
            link_btn: self.link_btn.clone(),
            insert_image_btn: self.insert_image_btn.clone(),
            table_btn: self.table_btn.clone(),
            hr_btn: self.hr_btn.clone(),
            code_btn: self.code_btn.clone(),
            clear_btn: self.clear_btn.clone(),
            source_btn: self.source_btn.clone(),
        }
    }
}

pub(crate) fn build_html_format_handles() -> HtmlFormatHandles {
    HtmlFormatHandles {
        bold_btn: style_toggle("format-text-bold-symbolic", &tr!("Bold (Ctrl+B)")),
        italic_btn: style_toggle("format-text-italic-symbolic", &tr!("Italic (Ctrl+I)")),
        underline_btn: style_toggle("format-text-underline-symbolic", &tr!("Underline (Ctrl+U)")),
        strike_btn: style_toggle("format-text-strikethrough-symbolic", &tr!("Strikethrough")),
        style_sep: gtk::Separator::new(gtk::Orientation::Vertical),
        bullet_btn: style_button("view-list-symbolic", &tr!("Bulleted list (Ctrl+Shift+8)")),
        number_btn: style_button(
            "view-list-ordered-symbolic",
            &tr!("Numbered list (Ctrl+Shift+7)"),
        ),
        quote_btn: style_button("format-indent-more-symbolic", &tr!("Quote (Ctrl+Shift+9)")),
        heading_btn: build_heading_menu_button(),
        spacing_btn: build_line_spacing_button(),
        block_sep: gtk::Separator::new(gtk::Orientation::Vertical),
        link_btn: style_button("insert-link-symbolic", &tr!("Insert link (Ctrl+K)")),
        insert_image_btn: style_button("insert-image-symbolic", &tr!("Insert image")),
        table_btn: style_button("x-office-spreadsheet-symbolic", &tr!("Insert table")),
        hr_btn: style_button("list-remove-symbolic", &tr!("Horizontal rule")),
        code_btn: style_button("insert-text-symbolic", &tr!("Inline code")),
        clear_btn: style_button("edit-clear-symbolic", &tr!("Clear formatting")),
        source_btn: style_toggle("document-edit-symbolic", &tr!("HTML source (Ctrl+Shift+H)")),
    }
}

pub(crate) fn append_html_format_handles(
    root: &adw::WrapBox,
    handles: &HtmlFormatHandles,
    include_source: bool,
) {
    root.append(&handles.bold_btn);
    root.append(&handles.italic_btn);
    root.append(&handles.underline_btn);
    root.append(&handles.strike_btn);
    root.append(&handles.style_sep);
    root.append(&handles.bullet_btn);
    root.append(&handles.number_btn);
    root.append(&handles.quote_btn);
    root.append(&handles.heading_btn);
    root.append(&handles.spacing_btn);
    root.append(&handles.block_sep);
    root.append(&handles.link_btn);
    root.append(&handles.insert_image_btn);
    root.append(&handles.table_btn);
    root.append(&handles.hr_btn);
    root.append(&handles.code_btn);
    root.append(&handles.clear_btn);
    if include_source {
        root.append(&handles.source_btn);
    }
}

pub(crate) fn set_html_format_sensitive(handles: &HtmlFormatHandles, enabled: bool) {
    handles.bold_btn.set_sensitive(enabled);
    handles.italic_btn.set_sensitive(enabled);
    handles.underline_btn.set_sensitive(enabled);
    handles.strike_btn.set_sensitive(enabled);
    handles.style_sep.set_sensitive(enabled);
    handles.bullet_btn.set_sensitive(enabled);
    handles.number_btn.set_sensitive(enabled);
    handles.quote_btn.set_sensitive(enabled);
    handles.heading_btn.set_sensitive(enabled);
    handles.spacing_btn.set_sensitive(enabled);
    handles.block_sep.set_sensitive(enabled);
    handles.link_btn.set_sensitive(enabled);
    handles.insert_image_btn.set_sensitive(enabled);
    handles.table_btn.set_sensitive(enabled);
    handles.hr_btn.set_sensitive(enabled);
    handles.code_btn.set_sensitive(enabled);
    handles.clear_btn.set_sensitive(enabled);
}

pub(crate) fn apply_html_source_mode(
    active: bool,
    source_btn: &gtk::ToggleButton,
    html_editor: &ComposeHtmlEditor,
    inlines: &Rc<RefCell<HashMap<String, ComposeInline>>>,
    sanitize_leaving_source: impl Fn(String) -> String + 'static,
) {
    if active {
        let editor = html_editor.clone();
        let source_btn = source_btn.clone();
        html_editor.flush_html(move || {
            if !source_btn.is_active() {
                return;
            }
            let html = editor.snapshot().borrow().clone();
            editor.set_source_text(&html);
            editor.set_view_mode(ComposeViewMode::Source);
        });
    } else if html_editor.is_source_visible() {
        let source = html_editor.source_text();
        let cid_map = {
            let inlines = inlines.borrow();
            html_editor.cid_to_sha256_map(&inlines)
        };
        let sanitized = sanitize_leaving_source(source);
        html_editor.set_html_display(&sanitized, &cid_map, false);
        html_editor.show_html();
        html_editor.focus_editor();
    }
}

pub(crate) fn install_signature_shortcuts(
    widget: &impl IsA<gtk::Widget>,
    html_editor: &ComposeHtmlEditor,
    source_btn: &gtk::ToggleButton,
) {
    let key_ctrl = gtk::EventControllerKey::new();
    key_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
    key_ctrl.connect_key_pressed({
        let html_editor = html_editor.clone();
        let source_btn = source_btn.clone();
        move |_, key, _, modifier| {
            let shift = modifier.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let ctrl = modifier.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            if ctrl && shift {
                match key {
                    Key::h | Key::H => {
                        source_btn.set_active(!source_btn.is_active());
                        return glib::Propagation::Stop;
                    }
                    Key::_7 | Key::_8 | Key::_9 | Key::v | Key::V if source_btn.is_active() => {
                        return glib::Propagation::Proceed;
                    }
                    Key::_7 => {
                        html_editor.exec("insertOrderedList", None);
                        html_editor.focus_editor();
                        return glib::Propagation::Stop;
                    }
                    Key::_8 => {
                        html_editor.exec("insertUnorderedList", None);
                        html_editor.focus_editor();
                        return glib::Propagation::Stop;
                    }
                    Key::_9 => {
                        html_editor.exec("formatBlock", Some("blockquote"));
                        html_editor.focus_editor();
                        return glib::Propagation::Stop;
                    }
                    Key::v | Key::V => {
                        html_editor.paste_as_plain_text();
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
            }
            if source_btn.is_active() {
                return glib::Propagation::Proceed;
            }
            if !ctrl {
                return glib::Propagation::Proceed;
            }
            match key {
                Key::b => {
                    html_editor.exec("bold", None);
                    html_editor.focus_editor();
                    glib::Propagation::Stop
                }
                Key::i => {
                    html_editor.exec("italic", None);
                    html_editor.focus_editor();
                    glib::Propagation::Stop
                }
                Key::u => {
                    html_editor.exec("underline", None);
                    html_editor.focus_editor();
                    glib::Propagation::Stop
                }
                Key::k => {
                    let editor = html_editor.clone();
                    let link_parent = html_editor.source_view.clone();
                    prompt_link_url(&link_parent, move |url| {
                        editor.exec("createLink", Some(&url));
                        editor.focus_editor();
                    });
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        }
    });
    widget.add_controller(key_ctrl);
}

pub(crate) fn wire_html_format_actions(
    handles: &HtmlFormatHandles,
    body_view: &gtk::TextView,
    html_editor: Rc<ComposeHtmlEditor>,
    inlines: Rc<RefCell<HashMap<String, ComposeInline>>>,
    format: Rc<Cell<ComposeFormat>>,
    on_insert_image: Rc<dyn Fn()>,
    sanitize_leaving_source: Rc<dyn Fn(String) -> String>,
    on_source_changed: Rc<dyn Fn()>,
) {
    wire_heading_actions(&handles.heading_btn, html_editor.clone());
    wire_line_spacing_actions(&handles.spacing_btn, html_editor.clone());

    handles.bold_btn.connect_clicked({
        let format = format.clone();
        let html_editor = html_editor.clone();
        let body_view = body_view.clone();
        move |btn| {
            let fmt = format.get();
            if fmt == ComposeFormat::Html {
                html_editor.exec("bold", None);
                html_editor.focus_editor();
                btn.set_active(false);
            } else if fmt == ComposeFormat::Markdown {
                wrap_selection(&body_view, "**", "**");
                btn.set_active(false);
            }
        }
    });
    handles.italic_btn.connect_clicked({
        let format = format.clone();
        let html_editor = html_editor.clone();
        let body_view = body_view.clone();
        move |btn| {
            let fmt = format.get();
            if fmt == ComposeFormat::Html {
                html_editor.exec("italic", None);
                html_editor.focus_editor();
                btn.set_active(false);
            } else if fmt == ComposeFormat::Markdown {
                wrap_selection(&body_view, "*", "*");
                btn.set_active(false);
            }
        }
    });
    handles.underline_btn.connect_clicked({
        let format = format.clone();
        let html_editor = html_editor.clone();
        move |btn| {
            if format.get() == ComposeFormat::Html {
                html_editor.exec("underline", None);
                html_editor.focus_editor();
            }
            btn.set_active(false);
        }
    });
    handles.strike_btn.connect_clicked({
        let format = format.clone();
        let html_editor = html_editor.clone();
        let body_view = body_view.clone();
        move |btn| {
            let fmt = format.get();
            if fmt == ComposeFormat::Html {
                html_editor.exec("strikethrough", None);
                html_editor.focus_editor();
                btn.set_active(false);
            } else if fmt == ComposeFormat::Markdown {
                wrap_selection(&body_view, "~~", "~~");
                btn.set_active(false);
            }
        }
    });
    handles.bullet_btn.connect_clicked({
        let html_editor = html_editor.clone();
        move |_| {
            html_editor.exec("insertUnorderedList", None);
            html_editor.focus_editor();
        }
    });
    handles.number_btn.connect_clicked({
        let html_editor = html_editor.clone();
        move |_| {
            html_editor.exec("insertOrderedList", None);
            html_editor.focus_editor();
        }
    });
    handles.quote_btn.connect_clicked({
        let html_editor = html_editor.clone();
        move |_| {
            html_editor.exec("formatBlock", Some("blockquote"));
            html_editor.focus_editor();
        }
    });
    handles.link_btn.connect_clicked({
        let body_view = body_view.clone();
        let format = format.clone();
        let html_editor = html_editor.clone();
        move |_| {
            let fmt = format.get();
            if fmt == ComposeFormat::Html {
                let editor = html_editor.clone();
                prompt_link_url(&body_view, move |url| {
                    editor.exec("createLink", Some(&url));
                    editor.focus_editor();
                });
            } else if fmt == ComposeFormat::Markdown {
                insert_markdown_link(&body_view);
            }
        }
    });
    handles.insert_image_btn.connect_clicked({
        let on_insert_image = on_insert_image.clone();
        move |_| on_insert_image()
    });
    handles.table_btn.connect_clicked({
        let body_view = body_view.clone();
        let html_editor = html_editor.clone();
        let format = format.clone();
        move |_| {
            let fmt = format.get();
            if fmt == ComposeFormat::Html {
                let editor = html_editor.clone();
                prompt_insert_table(&body_view, move |fragment| {
                    editor.insert_html(&fragment);
                    editor.focus_editor();
                });
            } else if fmt == ComposeFormat::Markdown {
                prompt_insert_markdown_table(&body_view);
            }
        }
    });
    handles.hr_btn.connect_clicked({
        let html_editor = html_editor.clone();
        move |_| {
            html_editor.exec("insertHorizontalRule", None);
            html_editor.focus_editor();
        }
    });
    handles.code_btn.connect_clicked({
        let html_editor = html_editor.clone();
        move |_| {
            html_editor.wrap_tag("code");
            html_editor.focus_editor();
        }
    });
    handles.clear_btn.connect_clicked({
        let html_editor = html_editor.clone();
        move |_| {
            html_editor.exec("removeFormat", None);
            html_editor.focus_editor();
        }
    });
    handles.source_btn.connect_toggled({
        let html_editor = html_editor.clone();
        let inlines = inlines.clone();
        let sanitize = sanitize_leaving_source.clone();
        let on_source_changed = on_source_changed.clone();
        move |btn| {
            apply_html_source_mode(btn.is_active(), btn, &html_editor, &inlines, {
                let sanitize = sanitize.clone();
                move |text| sanitize(text)
            });
            on_source_changed();
        }
    });
}

/// An extension button bound to one composer.
#[derive(Clone)]
pub struct ToolbarTool {
    pub icon_name: &'static str,
    pub tooltip: String,
    pub ctrl_shift_key: Option<Key>,
    pub run: Rc<dyn Fn()>,
}

/// Run the tool bound to Ctrl+Shift+`key`, if any. Returns whether one ran.
fn run_tool_for_key(tools: &[ToolbarTool], key: Key) -> bool {
    let key = key.to_lower();
    let Some(tool) = tools
        .iter()
        .find(|tool| tool.ctrl_shift_key.is_some_and(|k| k.to_lower() == key))
    else {
        return false;
    };
    (tool.run)();
    true
}

pub fn build_compose_toolbar(
    body_view: gtk::TextView,
    html_editor: Rc<ComposeHtmlEditor>,
    body_frame: gtk::Frame,
    inlines: Rc<RefCell<HashMap<String, ComposeInline>>>,
    on_format_changed: impl Fn(ComposeFormat) + Clone + 'static,
    on_attach: impl Fn() + Clone + 'static,
    tools: Rc<[ToolbarTool]>,
    on_insert_image: impl Fn() + Clone + 'static,
    on_html_images_drained: impl Fn(Vec<ComposeInline>) + Clone + 'static,
    on_include_signature_toggled: Rc<dyn Fn(bool)>,
    on_snippet_chosen: Rc<dyn Fn(SnippetId)>,
    on_manage_snippets: Rc<dyn Fn()>,
    initial_snippets: &[Snippet],
    lifetime_owner: &gtk::Widget,
) -> ComposeToolbar {
    let format = Rc::new(Cell::new(settings::compose_format()));

    let root = adw::WrapBox::builder()
        .hexpand(true)
        .halign(gtk::Align::Fill)
        .child_spacing(2)
        .line_spacing(2)
        .line_homogeneous(false)
        .justify(adw::JustifyMode::None)
        .wrap_policy(adw::WrapPolicy::Minimum)
        .css_classes(["alcedo-compose-toolbar"])
        .build();
    // AdwWrapBox bindings omit Accessible; the GtkWidget parent implements it.
    root.upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Label(&tr!("Formatting toolbar"))]);

    let handles = build_html_format_handles();
    handles.source_btn.set_visible(false);

    let attach_btn = style_button("mail-attachment-symbolic", &tr!("Attach files"));
    let tool_buttons: Vec<gtk::Button> = tools
        .iter()
        .map(|tool| style_button(tool.icon_name, &tool.tooltip))
        .collect();
    let signature_sep = gtk::Separator::new(gtk::Orientation::Vertical);
    let include_signature = style_toggle("user-info-symbolic", &tr!("Include signature"));
    include_signature.set_active(true);
    include_signature
        .update_property(&[gtk::accessible::Property::Label(&tr!("Include signature"))]);

    let snippets_popover = build_snippets_popover(
        initial_snippets,
        on_snippet_chosen.clone(),
        on_manage_snippets.clone(),
    );
    let snippets_btn = snippets_popover.button;
    let snippets_refresh = Rc::new(RefCell::new(Some(snippets_popover.refresh)));

    let md_quote_btn = md_button("format-indent-more-symbolic", &tr!("Quote"));
    let md_bullet_btn = md_button("view-list-symbolic", &tr!("Bulleted list"));
    let md_number_btn = md_button("view-list-ordered-symbolic", &tr!("Numbered list"));
    let md_code_btn = md_button("insert-text-symbolic", &tr!("Inline code"));

    let source_btn = handles.source_btn.clone();
    let preview_btn = style_toggle("view-reveal-symbolic", &tr!("Preview (Ctrl+Shift+P)"));
    preview_btn.set_visible(false);

    let format_model = gtk::StringList::new(&[trs!("Plain text"), "HTML", trs!("Markdown")]);
    let format_dropdown = gtk::DropDown::builder()
        .model(&format_model)
        .tooltip_text(tr!("Message format"))
        .can_focus(false)
        .build();
    format_dropdown.set_selected(settings::compose_format().dropdown_index());

    append_html_format_handles(&root, &handles, false);
    root.append(&attach_btn);
    if !tool_buttons.is_empty() {
        root.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    }
    for button in &tool_buttons {
        root.append(button);
    }
    root.append(&signature_sep);
    root.append(&include_signature);
    root.append(&snippets_btn);
    root.append(&md_quote_btn);
    root.append(&md_bullet_btn);
    root.append(&md_number_btn);
    root.append(&md_code_btn);

    root.append(&source_btn);
    root.append(&preview_btn);
    root.append(&format_dropdown);

    let style_toggles = vec![
        handles.bold_btn.clone(),
        handles.italic_btn.clone(),
        handles.underline_btn.clone(),
        handles.strike_btn.clone(),
    ];
    let html_block_buttons: Vec<gtk::Widget> = vec![
        handles.bullet_btn.clone().upcast(),
        handles.number_btn.clone().upcast(),
        handles.quote_btn.clone().upcast(),
        handles.heading_btn.clone().upcast(),
        handles.spacing_btn.clone().upcast(),
    ];
    let html_insert_buttons = vec![
        handles.table_btn.clone(),
        handles.hr_btn.clone(),
        handles.code_btn.clone(),
        handles.clear_btn.clone(),
    ];
    let md_buttons = vec![
        md_quote_btn.clone(),
        md_bullet_btn.clone(),
        md_number_btn.clone(),
        md_code_btn.clone(),
    ];

    let toolbar = ComposeToolbar {
        root: root.clone(),
        format_dropdown: format_dropdown.clone(),
        html_editor: html_editor.clone(),
        inlines: inlines.clone(),
        format: format.clone(),
        source_btn: source_btn.clone(),
        preview_btn: preview_btn.clone(),
        style_sep: handles.style_sep.clone(),
        block_sep: handles.block_sep.clone(),
        html_block_buttons: html_block_buttons.clone(),
        html_insert_buttons: html_insert_buttons.clone(),
        style_toggles: style_toggles.clone(),
        md_buttons: md_buttons.clone(),
        link_btn: handles.link_btn.clone(),
        insert_image_btn: handles.insert_image_btn.clone(),
        body_view: body_view.clone(),
        include_signature: include_signature.clone(),
        snippets_btn: snippets_btn.clone(),
        snippets_refresh: snippets_refresh.clone(),
    };
    toolbar.apply_format_ui(&body_frame, format.get());
    // Callbacks below are owned by the toolbar's widgets (or the body buffer),
    // so they hold the toolbar weakly. `lifetime_owner` (an ancestor) owns it:
    // the toolbar holds `root`, so `root` itself cannot.
    let weak_toolbar = crate::widget_lifetime::weak_owned_by(lifetime_owner, toolbar.clone());

    let on_insert_image = Rc::new(on_insert_image);
    let sanitize_leaving = Rc::new(|text: String| sanitize_compose_html(&text));
    let on_source_changed = {
        let weak_toolbar = weak_toolbar.clone();
        Rc::new(move || {
            let Some(toolbar) = weak_toolbar.upgrade() else {
                return;
            };
            if toolbar.source_btn.is_active() {
                toolbar.preview_btn.set_active(false);
            }
            toolbar.update_formatting_sensitivity();
        })
    };
    wire_html_format_actions(
        &handles,
        &body_view,
        html_editor.clone(),
        inlines.clone(),
        format.clone(),
        on_insert_image.clone(),
        sanitize_leaving,
        on_source_changed,
    );

    let update_preview = {
        let weak_toolbar = weak_toolbar.clone();
        move || {
            if let Some(toolbar) = weak_toolbar.upgrade() {
                toolbar.update_preview_visibility();
            }
        }
    };
    update_preview();
    body_view.buffer().connect_changed({
        let update_preview = update_preview.clone();
        move |_| update_preview()
    });

    format_dropdown.connect_notify_local(Some("selected"), {
        let body_frame = body_frame.clone();
        let body_view = body_view.clone();
        let on_format_changed = on_format_changed.clone();
        let on_html_images_drained = on_html_images_drained.clone();
        let format = format.clone();
        let inlines = inlines.clone();
        let html_editor = html_editor.clone();
        let weak_toolbar = weak_toolbar.clone();
        move |dropdown: &gtk::DropDown, _| {
            let Some(toolbar) = weak_toolbar.upgrade() else {
                return;
            };
            let previous = format.get();
            let fmt = ComposeFormat::from_dropdown_index(dropdown.selected());
            if previous == fmt {
                return;
            }
            toolbar.source_btn.set_active(false);
            toolbar.preview_btn.set_active(false);
            let previous_body = match previous {
                ComposeFormat::Plain | ComposeFormat::Markdown => {
                    let buffer = body_view.buffer();
                    buffer
                        .text(&buffer.start_iter(), &buffer.end_iter(), false)
                        .to_string()
                }
                ComposeFormat::Html => {
                    if html_editor.is_source_visible() {
                        html_editor.source_text()
                    } else {
                        html_editor.snapshot().borrow().clone()
                    }
                }
            };
            if previous == ComposeFormat::Html && fmt != ComposeFormat::Html {
                let drained = drain_html_inline_images(&html_editor, &mut inlines.borrow_mut());
                if !drained.is_empty() {
                    on_html_images_drained(drained);
                }
            }
            let converted = convert_compose_format(previous, fmt, &previous_body);
            format.set(fmt);
            update_body_frame_class(&body_frame, fmt);
            toolbar.update_button_visibility();
            toolbar.update_formatting_sensitivity();
            if fmt == ComposeFormat::Html {
                let inlines = inlines.borrow();
                html_editor.sync_cid_cache(&inlines);
                let cid_map = html_editor.cid_to_sha256_map(&inlines);
                let sanitized = sanitize_compose_html(&converted);
                html_editor.set_html_display(&sanitized, &cid_map, false);
                html_editor.show_html();
            } else {
                let buffer = body_view.buffer();
                buffer.delete(&mut buffer.start_iter(), &mut buffer.end_iter());
                buffer.set_text(&converted);
                html_editor.show_text();
            }
            on_format_changed(fmt);
        }
    });

    attach_btn.connect_clicked(move |_| on_attach());
    for (button, tool) in tool_buttons.iter().zip(tools.iter()) {
        let run = tool.run.clone();
        button.connect_clicked(move |_| run());
    }

    include_signature.connect_toggled({
        let on_include_signature_toggled = on_include_signature_toggled.clone();
        move |btn| on_include_signature_toggled(btn.is_active())
    });

    md_quote_btn.connect_clicked({
        let body_view = body_view.clone();
        let format = format.clone();
        move |_| {
            if format.get() == ComposeFormat::Markdown {
                apply_markdown_prefix(&body_view, "> ", false);
            }
        }
    });
    md_bullet_btn.connect_clicked({
        let body_view = body_view.clone();
        let format = format.clone();
        move |_| {
            if format.get() == ComposeFormat::Markdown {
                apply_markdown_prefix(&body_view, "- ", false);
            }
        }
    });
    md_number_btn.connect_clicked({
        let body_view = body_view.clone();
        let format = format.clone();
        move |_| {
            if format.get() == ComposeFormat::Markdown {
                apply_markdown_prefix(&body_view, "", true);
            }
        }
    });
    md_code_btn.connect_clicked({
        let body_view = body_view.clone();
        let format = format.clone();
        move |_| {
            if format.get() == ComposeFormat::Markdown {
                wrap_selection(&body_view, "`", "`");
            }
        }
    });

    preview_btn.connect_toggled({
        let weak_toolbar = weak_toolbar.clone();
        move |btn| {
            if let Some(toolbar) = weak_toolbar.upgrade() {
                toolbar.set_preview_active(btn.is_active());
            }
        }
    });

    install_shortcuts(
        &body_view,
        &body_view,
        &html_editor,
        &format,
        &source_btn,
        &preview_btn,
        &snippets_btn,
        tools.clone(),
        lifetime_owner,
    );
    install_shortcuts(
        &html_editor.web_view,
        &body_view,
        &html_editor,
        &format,
        &source_btn,
        &preview_btn,
        &snippets_btn,
        tools,
        lifetime_owner,
    );

    toolbar
}

fn build_heading_menu_button() -> gtk::MenuButton {
    let menu = gtk::gio::Menu::new();
    menu.append(Some(&tr!("Paragraph")), Some("heading.p"));
    menu.append(Some(&tr!("Heading 1")), Some("heading.h1"));
    menu.append(Some(&tr!("Heading 2")), Some("heading.h2"));
    menu.append(Some(&tr!("Heading 3")), Some("heading.h3"));

    gtk::MenuButton::builder()
        .icon_name("format-text-rich-symbolic")
        .tooltip_text(tr!("Heading"))
        .css_classes(["flat"])
        .can_focus(false)
        .focus_on_click(false)
        .menu_model(&menu)
        .build()
}

fn wire_heading_actions(heading_btn: &gtk::MenuButton, html_editor: Rc<ComposeHtmlEditor>) {
    let actions = gtk::gio::SimpleActionGroup::new();
    for (name, tag) in [("p", "p"), ("h1", "h1"), ("h2", "h2"), ("h3", "h3")] {
        let action = gtk::gio::SimpleAction::new(name, None);
        let editor = html_editor.clone();
        let tag = tag.to_string();
        action.connect_activate(move |_, _| {
            editor.exec("formatBlock", Some(&tag));
            editor.focus_editor();
        });
        actions.add_action(&action);
    }
    heading_btn.insert_action_group("heading", Some(&actions));
}

fn build_line_spacing_button() -> gtk::MenuButton {
    let menu = gtk::gio::Menu::new();
    menu.append(Some(&tr!("Single")), Some("linespacing.single"));
    menu.append(Some("1.15"), Some("linespacing.one"));
    menu.append(Some("1.5"), Some("linespacing.onehalf"));
    menu.append(Some(&tr!("Double")), Some("linespacing.double"));

    let btn = gtk::MenuButton::builder()
        .label("1.5")
        .tooltip_text(tr!("Line spacing"))
        .css_classes(["flat", "alcedo-compose-line-spacing"])
        .can_focus(false)
        .focus_on_click(false)
        .menu_model(&menu)
        .build();
    btn.update_property(&[gtk::accessible::Property::Label(&tr!("Line spacing"))]);

    btn
}

fn wire_line_spacing_actions(spacing_btn: &gtk::MenuButton, html_editor: Rc<ComposeHtmlEditor>) {
    let actions = gtk::gio::SimpleActionGroup::new();
    for (name, mult) in [
        ("single", "1"),
        ("one", "1.15"),
        ("onehalf", "1.5"),
        ("double", "2"),
    ] {
        let action = gtk::gio::SimpleAction::new(name, None);
        let editor = html_editor.clone();
        let mult = mult.to_string();
        action.connect_activate(move |_, _| {
            editor.apply_line_spacing(&mult);
            editor.focus_editor();
        });
        actions.add_action(&action);
    }
    spacing_btn.insert_action_group("linespacing", Some(&actions));
}

fn install_shortcuts(
    widget: &impl IsA<gtk::Widget>,
    body_view: &gtk::TextView,
    html_editor: &ComposeHtmlEditor,
    format: &Rc<Cell<ComposeFormat>>,
    source_btn: &gtk::ToggleButton,
    preview_btn: &gtk::ToggleButton,
    snippets_btn: &gtk::MenuButton,
    tools: Rc<[ToolbarTool]>,
    lifetime_owner: &gtk::Widget,
) {
    // Everything below is owned by `widget` (the body view or the editor
    // WebView, both inside the editor), so it holds the editor and the
    // toolbar buttons weakly.
    let insert_snippet = gtk::gio::SimpleAction::new("insert-snippet", None);
    let snippets_btn = snippets_btn.downgrade();
    insert_snippet.connect_activate(move |_, _| {
        if let Some(snippets_btn) = snippets_btn.upgrade() {
            snippets_btn.popup();
        }
    });
    let actions = gtk::gio::SimpleActionGroup::new();
    actions.add_action(&insert_snippet);
    widget.insert_action_group("compose", Some(&actions));

    let key_ctrl = gtk::EventControllerKey::new();
    key_ctrl.connect_key_pressed({
        let body_view = body_view.downgrade();
        let html_editor =
            crate::widget_lifetime::weak_owned_by(lifetime_owner, html_editor.clone());
        let format = format.clone();
        let source_btn = source_btn.downgrade();
        let preview_btn = preview_btn.downgrade();
        move |_, key, _, modifier| {
            let (Some(body_view), Some(html_editor), Some(source_btn), Some(preview_btn)) = (
                body_view.upgrade(),
                html_editor.upgrade(),
                source_btn.upgrade(),
                preview_btn.upgrade(),
            ) else {
                return glib::Propagation::Proceed;
            };
            let shift = modifier.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let ctrl = modifier.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let fmt = format.get();

            if ctrl && shift && run_tool_for_key(&tools, key) {
                return glib::Propagation::Stop;
            }
            if ctrl && shift {
                match key {
                    Key::_7 if fmt == ComposeFormat::Html => {
                        html_editor.exec("insertOrderedList", None);
                        html_editor.focus_editor();
                        return glib::Propagation::Stop;
                    }
                    Key::_8 if fmt == ComposeFormat::Html => {
                        html_editor.exec("insertUnorderedList", None);
                        html_editor.focus_editor();
                        return glib::Propagation::Stop;
                    }
                    Key::_9 if fmt == ComposeFormat::Html => {
                        html_editor.exec("formatBlock", Some("blockquote"));
                        html_editor.focus_editor();
                        return glib::Propagation::Stop;
                    }
                    Key::h | Key::H if fmt == ComposeFormat::Html => {
                        source_btn.set_active(!source_btn.is_active());
                        return glib::Propagation::Stop;
                    }
                    Key::p | Key::P if preview_btn.is_visible() => {
                        preview_btn.set_active(!preview_btn.is_active());
                        return glib::Propagation::Stop;
                    }
                    Key::v | Key::V if fmt == ComposeFormat::Html => {
                        html_editor.paste_as_plain_text();
                        return glib::Propagation::Stop;
                    }
                    _ => {}
                }
            }

            if !ctrl {
                return glib::Propagation::Proceed;
            }
            if fmt == ComposeFormat::Plain {
                return glib::Propagation::Proceed;
            }
            match key {
                Key::b => {
                    if fmt == ComposeFormat::Html {
                        html_editor.exec("bold", None);
                        html_editor.focus_editor();
                    } else {
                        wrap_selection(&body_view, "**", "**");
                    }
                    glib::Propagation::Stop
                }
                Key::i => {
                    if fmt == ComposeFormat::Html {
                        html_editor.exec("italic", None);
                        html_editor.focus_editor();
                    } else {
                        wrap_selection(&body_view, "*", "*");
                    }
                    glib::Propagation::Stop
                }
                Key::u => {
                    if fmt == ComposeFormat::Html {
                        html_editor.exec("underline", None);
                        html_editor.focus_editor();
                    }
                    glib::Propagation::Stop
                }
                Key::k => {
                    if fmt == ComposeFormat::Html {
                        let editor = Rc::clone(&html_editor);
                        prompt_link_url(&body_view, move |url| {
                            editor.exec("createLink", Some(&url));
                            editor.focus_editor();
                        });
                    } else {
                        insert_markdown_link(&body_view);
                    }
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        }
    });
    widget.add_controller(key_ctrl);
}

fn style_toggle(icon: &str, tooltip: &str) -> gtk::ToggleButton {
    gtk::ToggleButton::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .css_classes(["flat"])
        .can_focus(false)
        .focus_on_click(false)
        .build()
}

fn style_button(icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .css_classes(["flat"])
        .can_focus(false)
        .focus_on_click(false)
        .build()
}

fn md_button(icon: &str, tooltip: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .css_classes(["flat"])
        .can_focus(false)
        .focus_on_click(false)
        .visible(false)
        .build()
}

fn update_body_frame_class(frame: &gtk::Frame, format: ComposeFormat) {
    if format == ComposeFormat::Markdown {
        frame.add_css_class("alcedo-composer-markdown");
    } else {
        frame.remove_css_class("alcedo-composer-markdown");
    }
}

fn wrap_selection(body_view: &gtk::TextView, before: &str, after: &str) {
    let buffer = body_view.buffer();
    let Some((mut start, mut end)) = buffer.selection_bounds() else {
        return;
    };
    if start.offset() == end.offset() {
        return;
    }
    let selected = buffer.text(&start, &end, false).to_string();
    let wrapped = wrap_with(&selected, before, after);
    buffer.delete(&mut start, &mut end);
    buffer.insert(&mut start, &wrapped);
}

pub fn wrap_with(selection: &str, before: &str, after: &str) -> String {
    format!("{before}{selection}{after}")
}

fn apply_markdown_prefix(body_view: &gtk::TextView, prefix: &str, numbered: bool) {
    let buffer = body_view.buffer();
    let Some((mut start, mut end)) = buffer.selection_bounds() else {
        return;
    };
    if start.offset() == end.offset() {
        return;
    }
    let selected = buffer.text(&start, &end, false).to_string();
    let prefixed = prefix_lines(&selected, prefix, numbered);
    buffer.delete(&mut start, &mut end);
    buffer.insert(&mut start, &prefixed);
}

pub fn prefix_lines(selection: &str, prefix: &str, numbered: bool) -> String {
    selection
        .lines()
        .enumerate()
        .map(|(index, line)| {
            if numbered {
                let num_prefix = format!("{}. ", index + 1);
                if let Some(rest) = line.strip_prefix(&num_prefix) {
                    return rest.to_string();
                }
                if let Some(rest) = line.strip_prefix("- ") {
                    return format!("{num_prefix}{rest}");
                }
                format!("{num_prefix}{line}")
            } else if let Some(rest) = line.strip_prefix(prefix) {
                rest.to_string()
            } else {
                format!("{prefix}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn prompt_link_url(parent: &gtk::TextView, on_insert: impl Fn(String) + 'static) {
    let entry = gtk::Entry::builder()
        .placeholder_text("https://")
        .activates_default(true)
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading(tr!("Insert link"))
        .extra_child(&entry)
        .build();
    dialog.add_response("cancel", &tr!("Cancel"));
    dialog.add_response("insert", &tr!("Insert"));
    dialog.set_response_appearance("insert", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("insert"));
    dialog.set_close_response("cancel");

    let parent = parent.clone();
    dialog.choose(
        Some(parent.upcast_ref::<gtk::Widget>()),
        None::<&gtk::gio::Cancellable>,
        move |response| {
            if response.as_str() != "insert" {
                return;
            }
            let url = entry.text().to_string();
            let trimmed = url.trim();
            if trimmed.is_empty() {
                return;
            }
            on_insert(trimmed.to_string());
        },
    );
}

fn insert_markdown_link(body_view: &gtk::TextView) {
    let view = body_view.clone();
    prompt_link_url(body_view, move |trimmed| {
        let buffer = view.buffer();
        let Some((mut start, mut end)) = buffer.selection_bounds() else {
            return;
        };
        if start.offset() == end.offset() {
            return;
        }
        let selected = buffer.text(&start, &end, false).to_string();
        let linked = format!("[{selected}]({trimmed})");
        buffer.delete(&mut start, &mut end);
        buffer.insert(&mut start, &linked);
    });
}

fn prompt_insert_table(parent: &gtk::TextView, on_insert: impl Fn(String) + 'static) {
    let rows = gtk::SpinButton::with_range(1.0, 10.0, 1.0);
    rows.set_value(3.0);
    let cols = gtk::SpinButton::with_range(1.0, 8.0, 1.0);
    cols.set_value(3.0);
    let header = gtk::CheckButton::builder()
        .label(tr!("Header row"))
        .active(true)
        .build();

    let rows_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    rows_box.append(&gtk::Label::new(Some(&tr!("Rows"))));
    rows_box.append(&rows);
    let cols_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    cols_box.append(&gtk::Label::new(Some(&tr!("Columns"))));
    cols_box.append(&cols);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&rows_box);
    content.append(&cols_box);
    content.append(&header);

    let dialog = adw::AlertDialog::builder()
        .heading(tr!("Insert table"))
        .extra_child(&content)
        .build();
    dialog.add_response("cancel", &tr!("Cancel"));
    dialog.add_response("insert", &tr!("Insert"));
    dialog.set_response_appearance("insert", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("insert"));
    dialog.set_close_response("cancel");

    let parent = parent.clone();
    dialog.choose(
        Some(parent.upcast_ref::<gtk::Widget>()),
        None::<&gtk::gio::Cancellable>,
        move |response| {
            if response.as_str() != "insert" {
                return;
            }
            let row_count = rows.value() as u32;
            let col_count = cols.value() as u32;
            let with_header = header.is_active();
            let table = build_html_table(row_count, col_count, with_header);
            on_insert(table);
        },
    );
}

fn prompt_insert_markdown_table(body_view: &gtk::TextView) {
    let rows = gtk::SpinButton::with_range(1.0, 10.0, 1.0);
    rows.set_value(3.0);
    let cols = gtk::SpinButton::with_range(1.0, 8.0, 1.0);
    cols.set_value(3.0);
    let header = gtk::CheckButton::builder()
        .label(tr!("Header row"))
        .active(true)
        .build();

    let rows_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    rows_box.append(&gtk::Label::new(Some(&tr!("Rows"))));
    rows_box.append(&rows);
    let cols_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    cols_box.append(&gtk::Label::new(Some(&tr!("Columns"))));
    cols_box.append(&cols);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.append(&rows_box);
    content.append(&cols_box);
    content.append(&header);

    let dialog = adw::AlertDialog::builder()
        .heading(tr!("Insert table"))
        .extra_child(&content)
        .build();
    dialog.add_response("cancel", &tr!("Cancel"));
    dialog.add_response("insert", &tr!("Insert"));
    dialog.set_response_appearance("insert", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("insert"));
    dialog.set_close_response("cancel");

    let body_view = body_view.clone();
    dialog.choose(
        Some(body_view.clone().upcast_ref::<gtk::Widget>()),
        None::<&gtk::gio::Cancellable>,
        move |response| {
            if response.as_str() != "insert" {
                return;
            }
            let row_count = rows.value() as u32;
            let col_count = cols.value() as u32;
            let with_header = header.is_active();
            insert_markdown_table(&body_view, row_count, col_count, with_header);
        },
    );
}

fn build_html_table(rows: u32, cols: u32, header: bool) -> String {
    let mut out = String::from("<table border=\"1\" cellpadding=\"4\" cellspacing=\"0\">");
    let body_rows = if header {
        rows.saturating_sub(1).max(1)
    } else {
        rows
    };
    if header {
        out.push_str("<thead><tr>");
        for _ in 0..cols {
            out.push_str("<th><br></th>");
        }
        out.push_str("</tr></thead>");
    }
    out.push_str("<tbody>");
    for _ in 0..body_rows {
        out.push_str("<tr>");
        for _ in 0..cols {
            out.push_str("<td><br></td>");
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table>");
    out
}

fn insert_markdown_table(body_view: &gtk::TextView, rows: u32, cols: u32, header_row: bool) {
    let buffer = body_view.buffer();
    let (mut start, mut end) = match buffer.selection_bounds() {
        Some(bounds) => bounds,
        None => {
            let insert = buffer.get_insert();
            let iter = buffer.iter_at_mark(&insert);
            (iter, iter)
        }
    };
    let table = build_markdown_table(rows, cols, header_row);
    if start.offset() != end.offset() {
        buffer.delete(&mut start, &mut end);
    }
    buffer.insert(&mut start, &table);
}

fn build_markdown_table(rows: u32, cols: u32, header_row: bool) -> String {
    let cols = cols.max(1);
    let rows = rows.max(1);
    let cell = "|   ";
    let mut out = String::new();
    for row in 0..rows {
        out.push('|');
        for _ in 0..cols {
            out.push_str(cell);
        }
        out.push_str("|\n");
        if header_row && row == 0 {
            out.push('|');
            for _ in 0..cols {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{prefix_lines, wrap_with};

    #[test]
    fn wrap_with_adds_delimiters() {
        assert_eq!(wrap_with("text", "**", "**"), "**text**");
    }

    #[test]
    fn prefix_lines_quote_unwrap() {
        let input = "> line\n> other";
        let out = prefix_lines(input, "> ", false);
        assert_eq!(out, "line\nother");
    }

    #[test]
    fn prefix_lines_numbered_wrap() {
        let out = prefix_lines("one\ntwo", "", true);
        assert_eq!(out, "1. one\n2. two");
    }
}
