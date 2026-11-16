//! Compose attachment chips, file dialogs, and inline image helpers.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use alcedo_core::{
    ComposeAttachment, ComposeFormat, ComposeInline, MAX_COMPOSE_INLINES, attachment_size_warning,
    filename_is_safe,
};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::gdk;
use relm4::gtk::gio;
use relm4::gtk::glib;
use relm4::gtk::prelude::{ButtonExt, EventControllerExt, TextBufferExt, WidgetExt};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::compose_html::ComposeHtmlEditor;

pub fn format_part_size(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if size >= MB {
        format!("{:.1} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.0} KB", size as f64 / KB as f64)
    } else {
        format!("{size} B")
    }
}

pub fn cid_from_widget_name(name: &str) -> Option<&str> {
    name.strip_prefix("alcedo-cid:")
}

fn read_compose_file(path: &Path) -> Result<ComposeAttachment, &'static str> {
    let filename = compose_basename(path)?;
    if !filename_is_safe(&filename) {
        return Err("Could not read file");
    }

    let bytes = std::fs::read(path).map_err(|_| "Could not read file")?;

    Ok(ComposeAttachment {
        filename,
        mime: "application/octet-stream".to_string(),
        bytes,
    })
}

fn read_inline_image(path: &Path) -> Result<ComposeInline, &'static str> {
    let filename = compose_basename(path)?;
    if !filename_is_safe(&filename) {
        return Err("Could not read file");
    }

    let bytes = std::fs::read(path).map_err(|_| "Could not read file")?;

    Ok(ComposeInline {
        content_id: format!("{}@alcedo.mail", Uuid::new_v4()),
        filename,
        mime: "application/octet-stream".to_string(),
        bytes,
    })
}

const READ_FAILED: &str = "Could not read file";

/// Read files to attach without blocking the GTK thread. Results are in the
/// order of `paths`.
async fn load_compose_files(paths: Vec<PathBuf>) -> Vec<Result<ComposeAttachment, &'static str>> {
    let count = paths.len();
    crate::blocking_io::run(move || paths.iter().map(|path| read_compose_file(path)).collect())
        .await
        .unwrap_or_else(|| vec![Err(READ_FAILED); count])
}

/// Read an image to insert inline without blocking the GTK thread.
pub async fn load_inline_image(path: PathBuf) -> Result<ComposeInline, &'static str> {
    crate::blocking_io::run(move || read_inline_image(&path))
        .await
        .unwrap_or(Err(READ_FAILED))
}

pub fn inline_image_client_warning(filename: &str) -> Option<String> {
    let lists = crate::settings::attachment_lists();
    if lists.filename_is_typical_inline_image(filename) {
        return None;
    }
    let kind = alcedo_core::extension_of(filename)
        .map(|ext| ext.to_ascii_uppercase())
        .unwrap_or_else(|| "This".to_string());
    Some(format!(
        "{kind} images may not display in some email clients."
    ))
}

fn compose_basename(path: &Path) -> Result<String, &'static str> {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .filter(|name| {
            !name.contains("..")
                && path
                    .components()
                    .all(|component| !matches!(component, Component::ParentDir))
        })
        .ok_or("Could not read file")
}

pub fn show_composer_toast(widget: &gtk::Widget, message: &str) {
    let mut ancestor = widget.parent();
    while let Some(parent) = ancestor {
        if let Ok(overlay) = parent.clone().downcast::<adw::ToastOverlay>() {
            overlay.add_toast(crate::toast::plain(message));
            return;
        }
        ancestor = parent.parent();
    }
}

pub fn build_attachments_box() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .css_classes(["alcedo-composer-attachments"])
        .visible(false)
        .halign(gtk::Align::Start)
        .build()
}

pub fn refresh_attachment_chips(
    box_: &gtk::Box,
    attachments: &[ComposeAttachment],
    on_remove: Rc<dyn Fn(usize)>,
) {
    while let Some(child) = box_.first_child() {
        box_.remove(&child);
    }

    if attachments.is_empty() {
        box_.set_visible(false);
        return;
    }

    box_.set_visible(true);
    for (index, attachment) in attachments.iter().enumerate() {
        let chip = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(0)
            .valign(gtk::Align::Center)
            .css_classes(["alcedo-attachment-chip"])
            .build();
        let icon = gtk::Image::from_icon_name("mail-attachment-symbolic");
        icon.set_pixel_size(12);
        let label = gtk::Label::builder()
            .label(format!(
                "{} ({})",
                attachment.filename,
                format_part_size(attachment.bytes.len() as u64)
            ))
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(28)
            .css_classes(["alcedo-attachment-chip-label"])
            .build();
        let close_btn = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(format!("Remove {}", attachment.filename))
            .css_classes(["flat", "circular", "alcedo-recipient-chip-remove"])
            .build();
        let on_remove = on_remove.clone();
        close_btn.connect_clicked(move |_| on_remove(index));
        chip.append(&icon);
        chip.append(&label);
        chip.append(&close_btn);
        box_.append(&chip);
    }
}

fn build_image_filter() -> gtk::FileFilter {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Images"));
    filter.add_mime_type("image/*");
    for ext in crate::settings::attachment_lists().typical_inline_image_extensions {
        filter.add_pattern(&format!("*.{ext}"));
    }
    filter
}

fn build_all_files_filter() -> gtk::FileFilter {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("All files"));
    filter.add_pattern("*");
    filter
}

fn dialog_parent(widget: &gtk::Widget) -> Option<gtk::Window> {
    widget
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok())
}

pub fn open_attach_dialog(
    parent: &gtk::Widget,
    multiple: bool,
    images_only: bool,
    on_paths: impl Fn(Vec<PathBuf>) + 'static,
) {
    let dialog = gtk::FileDialog::new();
    dialog.set_title(if images_only {
        "Insert image"
    } else {
        "Attach files"
    });
    if images_only {
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        let images = build_image_filter();
        filters.append(&images);
        filters.append(&build_all_files_filter());
        dialog.set_filters(Some(&filters));
        dialog.set_default_filter(Some(&images));
    }
    let parent_window = dialog_parent(parent);

    if multiple {
        dialog.open_multiple(
            parent_window.as_ref(),
            None::<&gio::Cancellable>,
            move |result| {
                if let Ok(model) = result {
                    let paths = list_model_paths(&model);
                    if !paths.is_empty() {
                        on_paths(paths);
                    }
                }
            },
        );
    } else {
        dialog.open(
            parent_window.as_ref(),
            None::<&gio::Cancellable>,
            move |result| {
                if let Ok(file) = result
                    && let Some(path) = file.path()
                {
                    on_paths(vec![path]);
                }
            },
        );
    }
}

fn list_model_paths(model: &gio::ListModel) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for index in 0..model.n_items() {
        if let Some(item) = model.item(index)
            && let Ok(file) = item.downcast::<gio::File>()
            && let Some(path) = file.path()
        {
            paths.push(path);
        }
    }
    paths
}

const FILE_DROP_CLASS: &str = "alcedo-composer-file-drop";

/// Attach files dropped anywhere on `target_widget`.
///
/// Text fields and the HTML body accept the same drag in the bubble phase and
/// either insert the path or drop it. Capture runs first, so releasing over a
/// header, the subject, or the message body attaches the file instead.
fn set_file_drop_highlight(target: &gtk::DropTarget, highlighted: bool) {
    let Some(widget) = target.widget() else {
        return;
    };
    if highlighted {
        widget.add_css_class(FILE_DROP_CLASS);
    } else {
        widget.remove_css_class(FILE_DROP_CLASS);
    }
}

pub fn install_file_drop(target_widget: &gtk::Widget, on_files: impl Fn(Vec<PathBuf>) + 'static) {
    let drop_target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    drop_target.set_propagation_phase(gtk::PropagationPhase::Capture);

    // The highlighted widget is read from the controller: capturing
    // `target_widget` would be a cycle that keeps a closed composer alive.
    drop_target.connect_enter(|target, _, _| {
        set_file_drop_highlight(target, true);
        gdk::DragAction::COPY
    });
    drop_target.connect_leave(|target| set_file_drop_highlight(target, false));
    drop_target.connect_drop(move |target, value, _, _| {
        set_file_drop_highlight(target, false);
        let Ok(file_list) = value.get::<gdk::FileList>() else {
            return false;
        };
        let paths: Vec<PathBuf> = file_list
            .files()
            .into_iter()
            .filter_map(|file| file.path())
            .collect();
        if paths.is_empty() {
            return false;
        }
        on_files(paths);
        true
    });
    target_widget.add_controller(drop_target);
}

fn inline_to_attachment(inline: ComposeInline) -> ComposeAttachment {
    ComposeAttachment {
        filename: inline.filename,
        mime: inline.mime,
        bytes: inline.bytes,
    }
}

pub fn image_from_bytes(bytes: &[u8]) -> Option<gtk::Image> {
    let texture = gdk::Texture::from_bytes(&glib::Bytes::from(bytes)).ok()?;
    let image = gtk::Image::from_paintable(Some(&texture));
    image.set_pixel_size(160);
    image.add_css_class("alcedo-composer-inline-image");
    Some(image)
}

pub fn insert_inline_image_at_iter(
    body_view: &gtk::TextView,
    iter: &mut gtk::TextIter,
    inline: &ComposeInline,
) -> bool {
    let Some(image) = image_from_bytes(&inline.bytes) else {
        return false;
    };
    image.set_widget_name(&format!("alcedo-cid:{}", inline.content_id));
    let buffer = body_view.buffer();
    let anchor = buffer.create_child_anchor(iter);
    body_view.add_child_at_anchor(&image, &anchor);
    true
}

pub fn insert_html_inline_at_cursor(editor: &ComposeHtmlEditor, inline: &ComposeInline) -> bool {
    editor.cache_inline(inline);
    let sha256 = format!("{:x}", Sha256::digest(&inline.bytes));
    let tag = format!(
        r#"<img src="alcedo-cid://{sha256}" alt="" data-cid="{}">"#,
        inline.content_id
    );
    editor.insert_html(&tag);
    true
}

pub fn referenced_inline_cids_html(html: &str) -> HashSet<String> {
    let mut cids = HashSet::new();
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0;

    while let Some(rel) = lower[cursor..].find("<img") {
        let start = cursor + rel;
        let end = lower[start..]
            .find('>')
            .map(|index| start + index + 1)
            .unwrap_or(html.len());
        let tag = &html[start..end];

        if let Some(data_cid) = parse_data_cid(tag) {
            cids.insert(data_cid);
        } else if let Some(src) = parse_img_src(tag) {
            let trimmed = src.trim();
            if let Some(cid) = trimmed.strip_prefix("cid:") {
                let cid = cid.trim_start_matches('<').trim_end_matches('>');
                if !cid.is_empty() {
                    cids.insert(cid.to_string());
                }
            }
        }

        cursor = end;
    }

    cids
}

fn parse_data_cid(tag: &str) -> Option<String> {
    parse_attr(tag, "data-cid")
}

fn parse_img_src(tag: &str) -> Option<String> {
    parse_attr(tag, "src")
}

fn parse_attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let name_lower = name.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find(&name_lower) {
        let idx = search_from + rel;
        let before_ok = idx == 0 || {
            let prev = lower.as_bytes()[idx - 1];
            prev.is_ascii_whitespace() || prev == b'<'
        };
        if !before_ok {
            search_from = idx + 1;
            continue;
        }
        let rest = tag[idx + name.len()..].trim_start();
        let rest = rest.strip_prefix('=')?.trim_start();
        let quote = rest.chars().next()?;
        if quote == '"' || quote == '\'' {
            let rest = &rest[quote.len_utf8()..];
            let end = rest.find(quote)?;
            return Some(rest[..end].to_string());
        }
        search_from = idx + 1;
    }
    None
}

pub fn referenced_inline_cids(
    body_view: &gtk::TextView,
    format: ComposeFormat,
    html_editor: &ComposeHtmlEditor,
) -> HashSet<String> {
    match format {
        ComposeFormat::Html => referenced_inline_cids_html(&html_editor.html_for_cid_scan()),
        ComposeFormat::Markdown => {
            let text = body_view.buffer().text(
                &body_view.buffer().start_iter(),
                &body_view.buffer().end_iter(),
                false,
            );
            referenced_inline_cids_markdown(&text)
        }
        ComposeFormat::Plain => HashSet::new(),
    }
}

pub fn referenced_inline_cids_markdown(text: &str) -> HashSet<String> {
    let mut cids = HashSet::new();
    let mut search = text;
    while let Some(idx) = search.find("cid:") {
        let rest = &search[idx + 4..];
        let end = rest
            .find(|ch: char| ch == ')' || ch.is_ascii_whitespace())
            .unwrap_or(rest.len());
        let cid = rest[..end].trim();
        if !cid.is_empty() {
            cids.insert(cid.to_string());
        }
        search = &search[idx + 4 + end..];
    }
    cids
}

pub fn drain_html_inline_images(
    html_editor: &ComposeHtmlEditor,
    inlines: &mut HashMap<String, ComposeInline>,
) -> Vec<ComposeInline> {
    let cids = referenced_inline_cids_html(&html_editor.html_for_cid_scan());
    let mut drained = Vec::new();
    for cid in cids {
        if let Some(inline) = inlines.remove(&cid) {
            drained.push(inline);
        }
    }

    let html = html_editor.html_for_cid_scan();
    let stripped = strip_cid_images(&html);
    html_editor.set_html(&stripped);

    drained
}

fn strip_cid_images(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0;

    while let Some(rel) = lower[cursor..].find("<img") {
        let start = cursor + rel;
        out.push_str(&html[cursor..start]);
        let end = lower[start..]
            .find('>')
            .map(|index| start + index + 1)
            .unwrap_or(html.len());
        let tag = &html[start..end];
        let is_cid = parse_data_cid(tag).is_some()
            || parse_img_src(tag).is_some_and(|src| {
                let lower_src = src.to_ascii_lowercase();
                lower_src.starts_with("cid:") || lower_src.starts_with("alcedo-cid://")
            });
        if !is_cid {
            out.push_str(tag);
        }
        cursor = end;
    }

    out.push_str(&html[cursor..]);
    out
}

#[derive(Clone)]
pub struct ComposePartsState {
    pub attachments: Rc<RefCell<Vec<ComposeAttachment>>>,
    pub inlines: Rc<RefCell<HashMap<String, ComposeInline>>>,
    /// Attach requests whose files are still being read on a worker thread.
    /// GTK-thread only; sending waits until this is zero.
    pending_reads: Rc<Cell<usize>>,
}

impl Default for ComposePartsState {
    fn default() -> Self {
        Self::new()
    }
}

impl ComposePartsState {
    pub fn new() -> Self {
        Self {
            attachments: Rc::new(RefCell::new(Vec::new())),
            inlines: Rc::new(RefCell::new(HashMap::new())),
            pending_reads: Rc::new(Cell::new(0)),
        }
    }

    /// True while picked or dropped files are still being read, so a send
    /// now would leave them out.
    pub fn is_reading_files(&self) -> bool {
        self.pending_reads.get() > 0
    }

    pub fn clear(&self) {
        self.attachments.borrow_mut().clear();
        self.inlines.borrow_mut().clear();
    }

    pub fn prune_unreferenced_inlines(
        &self,
        body_view: &gtk::TextView,
        format: ComposeFormat,
        html_editor: &ComposeHtmlEditor,
    ) {
        let referenced = referenced_inline_cids(body_view, format, html_editor);
        self.inlines
            .borrow_mut()
            .retain(|cid, _| referenced.contains(cid));
    }

    /// Attachment plus inline-image bytes currently in the draft.
    fn total_part_bytes(&self) -> u64 {
        let attachments: u64 = self
            .attachments
            .borrow()
            .iter()
            .map(|item| item.bytes.len() as u64)
            .sum();
        let inlines: u64 = self
            .inlines
            .borrow()
            .values()
            .map(|item| item.bytes.len() as u64)
            .sum();
        attachments + inlines
    }

    /// The server has the final say on size, so going over the expected
    /// limit only warns; the part is still added.
    fn warn_if_over_size_limit(&self, toast_host: &gtk::Widget) {
        if let Some(message) = attachment_size_warning(self.total_part_bytes(), None) {
            show_composer_toast(toast_host, &message);
        }
    }

    pub fn try_add_attachment(
        &self,
        attachment: ComposeAttachment,
        toast_host: &gtk::Widget,
    ) -> bool {
        self.attachments.borrow_mut().push(attachment);
        self.warn_if_over_size_limit(toast_host);
        true
    }

    pub fn try_add_inline(
        &self,
        inline: ComposeInline,
        toast_host: &gtk::Widget,
        body_view: &gtk::TextView,
        format: ComposeFormat,
        html_editor: &ComposeHtmlEditor,
    ) -> Option<ComposeInline> {
        self.prune_unreferenced_inlines(body_view, format, html_editor);
        if self.inlines.borrow().len() >= MAX_COMPOSE_INLINES {
            show_composer_toast(toast_host, "Too many inline images (max 10)");
            return None;
        }
        self.inlines
            .borrow_mut()
            .insert(inline.content_id.clone(), inline.clone());
        self.warn_if_over_size_limit(toast_host);
        Some(inline)
    }

    pub fn append_drained_attachments(&self, drained: Vec<ComposeInline>) {
        let mut attachments = self.attachments.borrow_mut();
        for inline in drained {
            attachments.push(inline_to_attachment(inline));
        }
    }

    pub fn add_attachments_from_paths(
        &self,
        paths: Vec<PathBuf>,
        toast_host: &gtk::Widget,
        attachments_box: &gtk::Box,
        body_view: &gtk::TextView,
        format: ComposeFormat,
        html_editor: &ComposeHtmlEditor,
    ) {
        self.prune_unreferenced_inlines(body_view, format, html_editor);
        let parts = self.clone();
        let toast_host = toast_host.clone();
        let attachments_box = attachments_box.clone();
        // The files are read on a worker thread; a large attachment would
        // otherwise freeze the window until the read finished.
        self.pending_reads.set(self.pending_reads.get() + 1);
        glib::spawn_future_local(async move {
            let loaded_files = load_compose_files(paths).await;
            parts
                .pending_reads
                .set(parts.pending_reads.get().saturating_sub(1));
            for loaded in loaded_files {
                match loaded {
                    Ok(attachment) => {
                        parts.try_add_attachment(attachment, &toast_host);
                    }
                    Err(message) => show_composer_toast(&toast_host, message),
                }
            }
            let on_remove =
                build_remove_handler(parts.attachments.clone(), attachments_box.clone());
            refresh_attachment_chips(&attachments_box, &parts.attachments.borrow(), on_remove);
        });
    }
}

pub fn build_remove_handler(
    attachments: Rc<RefCell<Vec<ComposeAttachment>>>,
    attachments_box: gtk::Box,
) -> Rc<dyn Fn(usize)> {
    Rc::new(move |index| {
        {
            let mut list = attachments.borrow_mut();
            if index < list.len() {
                list.remove(index);
            }
        }
        let attachments = attachments.clone();
        let attachments_box = attachments_box.clone();
        glib::idle_add_local_once(move || {
            refresh_attachment_chips(
                &attachments_box,
                &attachments.borrow(),
                build_remove_handler(attachments.clone(), attachments_box.clone()),
            );
        });
    })
}

pub fn insert_markdown_inline_at_cursor(body_view: &gtk::TextView, inline: &ComposeInline) {
    let buffer = body_view.buffer();
    let (mut start, mut end) = match buffer.selection_bounds() {
        Some(bounds) => bounds,
        None => {
            let insert = buffer.get_insert();
            let iter = buffer.iter_at_mark(&insert);
            (iter, iter)
        }
    };
    let token = format!("![{}](cid:{})", inline.filename, inline.content_id);
    if start.offset() != end.offset() {
        buffer.delete(&mut start, &mut end);
    }
    buffer.insert(&mut start, &token);
}
