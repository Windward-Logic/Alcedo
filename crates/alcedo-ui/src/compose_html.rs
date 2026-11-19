//! WebKitGTK contenteditable HTML compose editor.

use alcedo_i18n::tr;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use alcedo_core::ComposeInline;
use alcedo_html::{cid_imgs_for_display, cid_imgs_for_send, sanitize_compose_html};
use alcedo_widgets::{
    CidCache, compose_editor_settings, create_isolated_context, ephemeral_network_session,
    mail_settings, register_cid_scheme,
};
use relm4::gtk;
use relm4::gtk::gio;
use relm4::gtk::glib::ControlFlow;
use relm4::gtk::prelude::*;
use sha2::{Digest, Sha256};
use webkit6::prelude::*;
use webkit6::{
    ContextMenu, ContextMenuAction, ContextMenuItem, LoadEvent, NavigationPolicyDecision,
    PolicyDecisionType, UserContentManager, WebContext, WebView,
};

use crate::settings::{clamp_font_size, editor_font_size};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposeViewMode {
    Text,
    Html,
    Source,
    Preview,
}

/// HTML compose body: contenteditable WebView, source TextView, and Markdown preview WebView.
#[derive(Clone)]
pub struct ComposeHtmlEditor {
    pub stack: gtk::Stack,
    pub web_view: WebView,
    pub source_view: gtk::TextView,
    pub preview_view: WebView,
    snapshot: Rc<RefCell<String>>,
    cid_cache: CidCache,
    view_mode: Rc<Cell<ComposeViewMode>>,
    compose_ready: Rc<Cell<bool>>,
    pending_display_html: Rc<RefCell<Option<PendingDisplayHtml>>>,
    preview_fragment: Rc<RefCell<Option<String>>>,
    on_html_input: Rc<RefCell<Rc<dyn Fn()>>>,
    /// Bumped when a compose session is discarded so an in-flight `getHtml`
    /// cannot write that session's body over the next one.
    flush_generation: Rc<Cell<u64>>,
}

#[derive(Clone)]
struct PendingDisplayHtml {
    html: String,
    focus_at_start: bool,
}

impl ComposeHtmlEditor {
    pub fn new(body_view: &gtk::TextView) -> Self {
        let snapshot = Rc::new(RefCell::new(String::new()));
        let cid_cache: CidCache = alcedo_widgets::new_unbounded_cid_cache();
        let view_mode = Rc::new(Cell::new(ComposeViewMode::Text));
        let compose_ready = Rc::new(Cell::new(false));
        let pending_display_html: Rc<RefCell<Option<PendingDisplayHtml>>> =
            Rc::new(RefCell::new(None));
        let preview_fragment = Rc::new(RefCell::new(None));
        let on_html_input: Rc<RefCell<Rc<dyn Fn()>>> =
            Rc::new(RefCell::new(Rc::new(|| {}) as Rc<dyn Fn()>));
        let flush_generation = Rc::new(Cell::new(0));

        let compose_context = build_compose_context(cid_cache.clone());
        let web_view = build_compose_web_view(
            &compose_context,
            snapshot.clone(),
            compose_ready.clone(),
            pending_display_html.clone(),
            on_html_input.clone(),
        );
        wire_compose_navigation_policy(&web_view);
        wire_compose_context_menu(&web_view);

        let preview_context = build_preview_context(cid_cache.clone());
        let preview_view = build_preview_web_view(&preview_context);
        wire_preview_navigation_policy(&preview_view);

        let source_view = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::Word)
            .editable(true)
            .build();
        source_view.update_property(&[gtk::accessible::Property::Label(&tr!("HTML source"))]);
        source_view.buffer().connect_changed({
            let on_html_input = on_html_input.clone();
            move |_| (**on_html_input.borrow())()
        });

        let text_scroll = wrap_min_height(body_view);
        let html_scroll = wrap_min_height(&web_view);
        let source_scroll = wrap_min_height(&source_view);
        source_scroll.add_css_class("alcedo-composer-source");
        let preview_scroll = wrap_min_height(&preview_view);
        preview_scroll.add_css_class("alcedo-composer-preview");
        preview_view.update_property(&[gtk::accessible::Property::Label(&tr!("Markdown preview"))]);

        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        stack.add_named(&text_scroll, Some("text"));
        stack.add_named(&html_scroll, Some("html"));
        stack.add_named(&source_scroll, Some("source"));
        stack.add_named(&preview_scroll, Some("preview"));
        stack.set_visible_child_name("text");

        Self {
            stack,
            web_view,
            source_view,
            preview_view,
            snapshot,
            cid_cache,
            view_mode,
            compose_ready,
            pending_display_html,
            preview_fragment,
            on_html_input,
            flush_generation,
        }
    }

    pub fn set_on_input(&self, cb: Rc<dyn Fn()>) {
        *self.on_html_input.borrow_mut() = cb;
    }

    pub fn snapshot(&self) -> Rc<RefCell<String>> {
        self.snapshot.clone()
    }

    pub fn view_mode(&self) -> ComposeViewMode {
        self.view_mode.get()
    }

    pub fn set_view_mode(&self, mode: ComposeViewMode) {
        self.view_mode.set(mode);
        let page = match mode {
            ComposeViewMode::Text => "text",
            ComposeViewMode::Html => "html",
            ComposeViewMode::Source => "source",
            ComposeViewMode::Preview => "preview",
        };
        self.stack.set_visible_child_name(page);
    }

    pub fn show_text(&self) {
        self.set_view_mode(ComposeViewMode::Text);
    }

    pub fn show_html(&self) {
        self.set_view_mode(ComposeViewMode::Html);
    }

    pub fn is_source_visible(&self) -> bool {
        self.view_mode.get() == ComposeViewMode::Source
    }

    pub fn is_preview_visible(&self) -> bool {
        self.view_mode.get() == ComposeViewMode::Preview
    }

    pub fn html_for_cid_scan(&self) -> String {
        if self.is_source_visible() {
            self.source_text()
        } else {
            self.snapshot.borrow().clone()
        }
    }

    pub fn source_text(&self) -> String {
        let buffer = self.source_view.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    pub fn set_source_text(&self, text: &str) {
        let buffer = self.source_view.buffer();
        buffer.delete(&mut buffer.start_iter(), &mut buffer.end_iter());
        buffer.set_text(text);
    }

    pub fn exec(&self, cmd: &str, value: Option<&str>) {
        let value_lit = value
            .map(js_string_literal)
            .unwrap_or_else(|| "null".to_string());
        let script = format!(
            "window.alcedoCompose.exec({}, {value_lit})",
            js_string_literal(cmd)
        );
        self.run_js(&script);
    }

    pub fn insert_html(&self, html: &str) {
        let script = format!(
            "window.alcedoCompose.insertHTML({})",
            js_string_literal(html)
        );
        self.run_js(&script);
    }

    pub fn wrap_tag(&self, tag: &str) {
        let script = format!("window.alcedoCompose.wrapTag({})", js_string_literal(tag));
        self.run_js(&script);
    }

    pub fn apply_line_spacing(&self, multiplier: &str) {
        let script = format!(
            "window.alcedoCompose.applyLineSpacing({})",
            js_string_literal(multiplier)
        );
        self.run_js(&script);
    }

    pub fn set_html(&self, fragment: &str) {
        let sanitized = sanitize_compose_html(fragment);
        *self.snapshot.borrow_mut() = cid_imgs_for_send(&sanitized);
        self.push_display_html(&sanitized, false);
    }

    pub fn set_html_display(
        &self,
        fragment: &str,
        cid_to_sha256: &HashMap<String, String>,
        focus_at_start: bool,
    ) {
        let sanitized = sanitize_compose_html(fragment);
        let display = cid_imgs_for_display(&sanitized, cid_to_sha256);
        *self.snapshot.borrow_mut() = cid_imgs_for_send(&sanitized);
        self.push_display_html(&display, focus_at_start);
    }

    pub fn focus_editor(&self) {
        self.run_js("window.alcedoCompose.focus()");
    }

    /// Put the document origin at the top of the WebView. Caret focus scrolls
    /// the page; this undoes that after the view has a real allocation.
    pub fn scroll_document_to_start(&self) {
        self.run_js("if (window.alcedoCompose) window.alcedoCompose.scrollToStart()");
    }

    pub fn paste_as_plain_text(&self) {
        self.web_view.execute_editing_command("PasteAsPlainText");
        self.focus_editor();
    }

    /// Drop in-flight HTML flushes. Call after the current draft has been copied
    /// out (queued send, discard) and before the editor is reused.
    pub fn invalidate_pending_flush(&self) {
        self.flush_generation
            .set(self.flush_generation.get().wrapping_add(1));
    }

    pub fn flush_html(&self, on_done: impl FnOnce() + 'static) {
        let generation = self.flush_generation.get();
        let flush_generation = self.flush_generation.clone();
        self.web_view.evaluate_javascript(
            "window.alcedoCompose.getHtml()",
            None,
            None,
            None::<&gio::Cancellable>,
            {
                let snapshot = self.snapshot.clone();
                move |result| {
                    if flush_generation.get() == generation
                        && let Ok(value) = result
                    {
                        *snapshot.borrow_mut() = cid_imgs_for_send(&value.to_str());
                    }
                    on_done();
                }
            },
        );
    }

    pub fn apply_theme(&self, dark: bool) {
        let script = format!("document.body.classList.toggle('dark', {dark});");
        self.web_view
            .evaluate_javascript(&script, None, None, None::<&gio::Cancellable>, |_| {});
    }

    pub fn apply_font_size(&self, px: i32) {
        let px = clamp_font_size(px);
        let script = format!("document.body.style.fontSize = '{px}px';");
        self.web_view
            .evaluate_javascript(&script, None, None, None::<&gio::Cancellable>, |_| {});
        if let Some(fragment) = self.preview_fragment.borrow().clone() {
            self.preview_view
                .load_html(&wrap_preview_document(&fragment), Some("about:blank"));
        }
    }

    pub fn load_preview(&self, fragment: &str) {
        *self.preview_fragment.borrow_mut() = Some(fragment.to_string());
        self.preview_view
            .load_html(&wrap_preview_document(fragment), Some("about:blank"));
    }

    pub fn cache_inline(&self, inline: &ComposeInline) {
        let sha256 = format!("{:x}", Sha256::digest(&inline.bytes));
        self.cid_cache
            .borrow_mut()
            .insert(sha256, (inline.bytes.clone(), inline.mime.clone()));
    }

    pub fn sync_cid_cache(&self, inlines: &HashMap<String, ComposeInline>) {
        self.cid_cache.borrow_mut().clear();
        for inline in inlines.values() {
            self.cache_inline(inline);
        }
    }

    pub fn cid_to_sha256_map(
        &self,
        inlines: &HashMap<String, ComposeInline>,
    ) -> HashMap<String, String> {
        inlines
            .iter()
            .map(|(cid, inline)| {
                let sha256 = format!("{:x}", Sha256::digest(&inline.bytes));
                (cid.clone(), sha256)
            })
            .collect()
    }

    fn push_display_html(&self, display: &str, focus_at_start: bool) {
        if self.compose_ready.get() {
            self.run_set_html_js(display, focus_at_start);
        } else {
            *self.pending_display_html.borrow_mut() = Some(PendingDisplayHtml {
                html: display.to_string(),
                focus_at_start,
            });
        }
    }

    fn run_set_html_js(&self, html: &str, focus_at_start: bool) {
        self.run_js(&set_html_script(html, focus_at_start));
    }

    fn run_js(&self, script: &str) {
        self.web_view.evaluate_javascript(
            script,
            None,
            None,
            None::<&gio::Cancellable>,
            |result| {
                if let Err(err) = result {
                    tracing::debug!(?err, "compose editor script failed");
                }
            },
        );
    }
}

fn wrap_min_height(widget: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let scroll = gtk::ScrolledWindow::builder()
        .min_content_height(200)
        .min_content_width(1)
        .propagate_natural_width(false)
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .child(widget)
        .build();
    disable_viewport_scroll_to_focus(&scroll);
    scroll
}

/// Focusing the body must not slide the composer. On the first map the form is
/// still a few pixels taller than the viewport, and scroll-to-focus jumps to
/// the bottom of that overflow.
pub(crate) fn disable_viewport_scroll_to_focus(scroll: &gtk::ScrolledWindow) {
    let mut child = scroll.first_child();
    while let Some(widget) = child {
        if let Ok(viewport) = widget.clone().downcast::<gtk::Viewport>() {
            viewport.set_scroll_to_focus(false);
        }
        child = widget.next_sibling();
    }
}

/// Hold ancestor scrolled windows at the top for a few frames after open.
/// The first allocation and the caret focus both land before the composer has
/// its final height, and that offset sticks once the pane settles.
pub(crate) fn pin_ancestor_vscroll_to_top(
    anchor: &impl IsA<gtk::Widget>,
    on_frame: impl Fn() + 'static,
) {
    let anchor = anchor.clone().upcast::<gtk::Widget>();
    let left = Rc::new(Cell::new(8u8));
    let on_frame = Rc::new(on_frame);
    anchor.add_tick_callback(move |widget, _| {
        let mut current = Some(widget.clone());
        while let Some(w) = current {
            if let Some(scroll) = w.downcast_ref::<gtk::ScrolledWindow>() {
                let adj = scroll.vadjustment();
                adj.set_value(adj.lower());
            }
            current = w.parent();
        }
        on_frame();
        let n = left.get().saturating_sub(1);
        left.set(n);
        if n == 0 {
            ControlFlow::Break
        } else {
            ControlFlow::Continue
        }
    });
}

fn build_compose_context(cid_cache: CidCache) -> WebContext {
    let context = create_isolated_context();
    register_cid_scheme(&context, cid_cache);
    context
}

fn build_compose_web_view(
    context: &WebContext,
    snapshot: Rc<RefCell<String>>,
    compose_ready: Rc<Cell<bool>>,
    pending_display_html: Rc<RefCell<Option<PendingDisplayHtml>>>,
    on_html_input: Rc<RefCell<Rc<dyn Fn()>>>,
) -> WebView {
    let content_manager = compose_content_manager(snapshot, on_html_input);
    let settings = compose_editor_settings();
    let web_view = WebView::builder()
        .web_context(context)
        .network_session(&ephemeral_network_session())
        .settings(&settings)
        .user_content_manager(&content_manager)
        .default_content_security_policy(
            "default-src 'none'; img-src alcedo-cid:; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'none'; frame-src 'none'",
        )
        .build();
    // WebKitGTK reports a large default size request that would floor the
    // composer window's minimum width.
    web_view.set_hexpand(true);
    web_view.set_vexpand(true);
    web_view.set_size_request(1, 1);

    match gio::resources_lookup_data(
        "/dev/alcedo/Mail/compose/editor.html",
        gio::ResourceLookupFlags::NONE,
    ) {
        Ok(bytes) => {
            let html = String::from_utf8_lossy(bytes.as_ref());
            web_view.load_html(&html, Some("about:blank"));
        }
        Err(error) => {
            tracing::warn!(?error, "compose editor.html missing from gresource");
            web_view.load_html("<div contenteditable></div>", Some("about:blank"));
        }
    }

    web_view.connect_load_changed({
        let compose_ready = compose_ready.clone();
        let pending_display_html = pending_display_html.clone();
        move |view, event| {
            if event != LoadEvent::Finished {
                return;
            }
            compose_ready.set(true);
            let px = editor_font_size();
            let font_script = format!("document.body.style.fontSize = '{px}px';");
            view.evaluate_javascript(&font_script, None, None, None::<&gio::Cancellable>, |_| {});
            let Some(pending) = pending_display_html.borrow_mut().take() else {
                return;
            };
            view.evaluate_javascript(
                &set_html_script(&pending.html, pending.focus_at_start),
                None,
                None,
                None::<&gio::Cancellable>,
                |result| {
                    if let Err(err) = result {
                        tracing::debug!(?err, "compose editor setHtml on load failed");
                    }
                },
            );
        }
    });

    web_view
}

fn compose_content_manager(
    snapshot: Rc<RefCell<String>>,
    on_html_input: Rc<RefCell<Rc<dyn Fn()>>>,
) -> UserContentManager {
    let manager = UserContentManager::new();
    manager.register_script_message_handler("compose", None);
    manager.connect_script_message_received(Some("compose"), move |_manager, value| {
        let text = value.to_str().to_string();
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
            && json.get("type").and_then(|v| v.as_str()) == Some("html")
            && let Some(html) = json.get("html").and_then(|v| v.as_str())
        {
            *snapshot.borrow_mut() = cid_imgs_for_send(html);
            (**on_html_input.borrow())();
        }
    });
    manager
}

fn build_preview_context(cid_cache: CidCache) -> WebContext {
    let context = create_isolated_context();
    register_cid_scheme(&context, cid_cache);
    context
}

fn build_preview_web_view(context: &WebContext) -> WebView {
    let settings = mail_settings();
    let web_view = WebView::builder()
        .web_context(context)
        .network_session(&ephemeral_network_session())
        .settings(&settings)
        .build();
    web_view.set_hexpand(true);
    web_view.set_vexpand(true);
    web_view.set_size_request(1, 1);
    web_view
}

fn wire_compose_context_menu(web_view: &WebView) {
    web_view.connect_context_menu(|_view, menu, hit| {
        if hit.context_is_editable() {
            ensure_paste_as_plain_text_item(menu);
        }
        false
    });
}

fn ensure_paste_as_plain_text_item(menu: &ContextMenu) {
    let actions: Vec<_> = menu
        .items()
        .iter()
        .map(|item| item.stock_action())
        .collect();
    if let Some(at) = paste_as_plain_text_insert_at(&actions) {
        menu.insert(
            &ContextMenuItem::from_stock_action(ContextMenuAction::PasteAsPlainText),
            at,
        );
    }
}

fn paste_as_plain_text_insert_at(actions: &[ContextMenuAction]) -> Option<i32> {
    if actions.contains(&ContextMenuAction::PasteAsPlainText) {
        return None;
    }
    Some(
        actions
            .iter()
            .position(|action| *action == ContextMenuAction::Paste)
            .map(|index| index as i32 + 1)
            .unwrap_or(actions.len() as i32),
    )
}

fn wire_compose_navigation_policy(web_view: &WebView) {
    web_view.connect_decide_policy(move |_view, decision, decision_type| {
        if decision_type != PolicyDecisionType::NavigationAction {
            return false;
        }
        let Some(nav) = decision.downcast_ref::<NavigationPolicyDecision>() else {
            return false;
        };
        let Some(action) = nav.navigation_action() else {
            return false;
        };
        let Some(request) = action.request() else {
            return false;
        };
        let uri = request.uri().map(|uri| uri.to_string()).unwrap_or_default();
        if uri == "about:blank" || uri.starts_with("alcedo-cid:") {
            return false;
        }
        decision.ignore();
        true
    });
}

fn wire_preview_navigation_policy(web_view: &WebView) {
    web_view.connect_decide_policy(move |view, decision, decision_type| {
        let uri = match decision_type {
            PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction => {
                let Some(nav) = decision.downcast_ref::<NavigationPolicyDecision>() else {
                    return false;
                };
                let Some(action) = nav.navigation_action() else {
                    return false;
                };
                let Some(request) = action.request() else {
                    return false;
                };
                request.uri().map(|uri| uri.to_string())
            }
            _ => return false,
        };

        let Some(uri) = uri else {
            return false;
        };

        if uri == "about:blank" || uri.starts_with("about:blank#") || uri.starts_with("alcedo-cid:")
        {
            return false;
        }

        if uri.starts_with("mailto:") {
            decision.ignore();
            return true;
        }

        if uri.starts_with("http://") || uri.starts_with("https://") {
            decision.ignore();
            let parent = view.root().and_then(|w| w.downcast::<gtk::Window>().ok());
            let launcher = gtk::UriLauncher::new(&uri);
            launcher.launch(parent.as_ref(), None::<&gio::Cancellable>, |result| {
                if let Err(err) = result {
                    tracing::warn!(?err, "failed to launch external URI from compose preview");
                }
            });
            return true;
        }

        decision.ignore();
        true
    });
}

fn preview_wrapper_css(font_px: i32) -> String {
    preview_document_css(font_px, true)
}

fn preview_document_css(font_px: i32, outline_tables: bool) -> String {
    let table_rules = if outline_tables {
        r#"  th, td {
    border: 1px solid rgba(127,127,127,0.45);
    padding: 4px 8px;
  }"#
    } else {
        ""
    };
    format!(
        r#"  html, body {{
    margin: 0;
    padding: 0;
    background: transparent;
    color: inherit;
    font: {font_px}px/1.45 system-ui, "Segoe UI", sans-serif;
  }}
  body {{ padding: 12px 16px; }}
  img {{ max-width: 100%; height: auto; }}
  a {{
    color: #1c71d8;
    text-decoration: underline;
    cursor: pointer;
  }}
  @media (prefers-color-scheme: dark) {{
    a {{ color: #62a0ea; }}
  }}
  blockquote, blockquote.alcedo-quote {{
    margin: 0.5em 0;
    padding-left: 0.75em;
    border-left: 3px solid rgba(127,127,127,0.45);
  }}
  table {{ border-collapse: collapse; }}
{table_rules}"#
    )
}

fn wrap_preview_document(body: &str) -> String {
    wrap_preview_html(body, &preview_wrapper_css(editor_font_size()))
}

fn wrap_preview_html(body: &str, css: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<style>
{css}
</style>
</head>
<body>{body}</body>
</html>"#
    )
}

fn set_html_script(html: &str, focus_at_start: bool) -> String {
    format!(
        "window.alcedoCompose.setHtml({}, {})",
        js_string_literal(html),
        focus_at_start
    )
}

fn js_string_literal(value: &str) -> String {
    let mut out = String::from('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if c < '\x20' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::{js_string_literal, paste_as_plain_text_insert_at, preview_wrapper_css};
    use webkit6::ContextMenuAction;

    #[test]
    fn paste_as_plain_text_insert_at_already_present() {
        let actions = [
            ContextMenuAction::Paste,
            ContextMenuAction::PasteAsPlainText,
        ];
        assert_eq!(paste_as_plain_text_insert_at(&actions), None);
    }

    #[test]
    fn paste_as_plain_text_insert_at_after_paste() {
        let actions = [
            ContextMenuAction::Cut,
            ContextMenuAction::Copy,
            ContextMenuAction::Paste,
        ];
        assert_eq!(paste_as_plain_text_insert_at(&actions), Some(3));
    }

    #[test]
    fn paste_as_plain_text_insert_at_empty_menu() {
        let actions: [ContextMenuAction; 0] = [];
        assert_eq!(paste_as_plain_text_insert_at(&actions), Some(0));
    }

    #[test]
    fn paste_as_plain_text_insert_at_without_paste() {
        let actions = [ContextMenuAction::Cut, ContextMenuAction::Copy];
        assert_eq!(paste_as_plain_text_insert_at(&actions), Some(2));
    }

    #[test]
    fn js_string_literal_escapes_quotes_and_newlines() {
        assert_eq!(js_string_literal("a\"b\n"), r#""a\"b\n""#);
    }

    #[test]
    fn js_string_literal_escapes_line_and_paragraph_separators() {
        let line = js_string_literal("ok\u{2028});alert(1)//");
        assert!(line.contains("\\u2028"), "{line}");
        assert!(!line.contains('\u{2028}'), "{line}");
        let para = js_string_literal("ok\u{2029}x");
        assert!(para.contains("\\u2029"), "{para}");
        assert!(!para.contains('\u{2029}'), "{para}");
    }

    #[test]
    fn preview_wrapper_css_uses_editor_font() {
        let css = preview_wrapper_css(18);
        assert!(css.contains("font: 18px/1.45"));
    }
}
