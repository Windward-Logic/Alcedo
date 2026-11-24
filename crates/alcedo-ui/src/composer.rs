//! Compose view (P4 + P9 disclosure).

use alcedo_i18n::{n_, tr, trf, trm, trs};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::glib;

use alcedo_core::compose_status::{DraftStatus, draft_status_subtitle};
use alcedo_core::{
    Account, AccountId, AccountIdentity, Address, ComposeDraft, ComposeFormat, ComposeFromChoice,
    ComposeInline, EncryptReadiness, FolderId, IdentityId, ImapDraftRef, PgpAccountState, Snippet,
    SnippetId, ThreadId, recipient_encrypt_ready, resolve_compose_from_text,
};
use tokio::runtime::Handle;

use crate::commands;

/// GTK accelerator for Send. The tooltip and the composer shortcut controller
/// both use this so the hint cannot drift from the key.
pub(crate) const SEND_ACCEL: &str = "<Primary>Return";

use crate::compose_attachments::{
    ComposePartsState, build_attachments_box, build_remove_handler, inline_image_client_warning,
    insert_html_inline_at_cursor, insert_markdown_inline_at_cursor, install_file_drop,
    load_inline_image, open_attach_dialog, refresh_attachment_chips, show_composer_toast,
};
use crate::compose_draft_store::{DraftSlot, clear_compose_draft, save_compose_draft};
use crate::compose_html::{ComposeHtmlEditor, ComposeViewMode, disable_viewport_scroll_to_focus};
use crate::compose_toolbar::{ComposeToolbar, ToolbarTool, build_compose_toolbar};
use crate::help::{self, TOPIC_COMPOSE};
use crate::icons;
use crate::recipient_row::{ContactBook, RecipientField};
use crate::settings;
use uuid::Uuid;

const MDN_DISCLOSURE: &str = n_!("Recipients may be notified when this is read.");
const PIXEL_DISCLOSURE: &str = n_!("A tracking image will be inserted.");

/// Parsed `mailto:` launch parameters.
#[derive(Clone, Debug, Default)]
pub struct MailtoPrefill {
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
}

static MAILTO_PREFILL: Mutex<Option<MailtoPrefill>> = Mutex::new(None);

/// Compose toolbar and field change hooks wired from [`crate::mail::MailModel`].
#[derive(Clone)]
pub struct ComposerHooks {
    pub on_include_signature_toggled: Rc<dyn Fn(bool)>,
    pub on_snippet_chosen: Rc<dyn Fn(SnippetId)>,
    pub on_manage_snippets: Rc<dyn Fn()>,
    pub on_from_changed: Rc<dyn Fn()>,
    pub on_compose_changed: Rc<dyn Fn()>,
    /// Runs extension address action `index` for an email on a recipient chip.
    pub on_address_action: Rc<dyn Fn(usize, String)>,
    pub snippets: Rc<RefCell<Vec<Snippet>>>,
}

impl ComposerHooks {
    pub fn noop() -> Self {
        Self {
            on_include_signature_toggled: Rc::new(|_| {}),
            on_snippet_chosen: Rc::new(|_| {}),
            on_manage_snippets: Rc::new(|| {}),
            on_from_changed: Rc::new(|| {}),
            on_compose_changed: Rc::new(|| {}),
            on_address_action: Rc::new(|_, _| {}),
            snippets: Rc::new(RefCell::new(Vec::new())),
        }
    }
}
static MAILTO_OPEN_COMPOSER: AtomicBool = AtomicBool::new(false);

/// Store a pending `mailto:` launch to apply when the composer opens.
pub fn set_mailto_launch(prefill: MailtoPrefill) {
    if let Ok(mut slot) = MAILTO_PREFILL.lock() {
        *slot = Some(prefill);
    }
    MAILTO_OPEN_COMPOSER.store(true, Ordering::SeqCst);
}

/// Whether a `mailto:` launch requested the composer.
pub fn mailto_open_pending() -> bool {
    MAILTO_OPEN_COMPOSER.load(Ordering::SeqCst)
}

/// Returns true once when a `mailto:` launch requested the composer.
pub fn take_mailto_open_request() -> bool {
    MAILTO_OPEN_COMPOSER.swap(false, Ordering::SeqCst)
}

/// Parse a `mailto:` URI (RFC 6068) into compose fields.
///
/// Values are percent-decoded as UTF-8; `+` is a literal plus (RFC 6068
/// does not use form encoding). Addresses from the path and any `to=`
/// (and repeated `cc=` / `bcc=`) parameters are merged. Header-ish fields
/// lose CR/LF so a link cannot inject extra headers.
pub fn parse_mailto_uri(uri: &str) -> Option<MailtoPrefill> {
    let scheme = uri.get(..7)?;
    if !scheme.eq_ignore_ascii_case("mailto:") {
        return None;
    }
    let rest = &uri[7..];
    let rest = rest.split_once('#').map_or(rest, |(before, _)| before);
    if rest.is_empty() {
        return None;
    }

    let (address_part, query_part) = match rest.split_once('?') {
        Some((addrs, query)) => (addrs, Some(query)),
        None => (rest, None),
    };

    let mut to = Vec::new();
    let mut cc = Vec::new();
    let mut bcc = Vec::new();
    push_addresses(&mut to, address_part);
    let mut prefill = MailtoPrefill::default();

    if let Some(query) = query_part {
        for pair in query.split('&') {
            let (key, value) = match pair.split_once('=') {
                Some((k, v)) => (k, v),
                None => (pair, ""),
            };
            match decode_mailto_component(key).to_ascii_lowercase().as_str() {
                "to" => push_addresses(&mut to, value),
                "cc" => push_addresses(&mut cc, value),
                "bcc" => push_addresses(&mut bcc, value),
                "subject" if prefill.subject.is_empty() => {
                    prefill.subject = single_line(&decode_mailto_component(value));
                }
                "body" if prefill.body.is_empty() => {
                    prefill.body = decode_mailto_component(value).replace("\r\n", "\n");
                }
                _ => {}
            }
        }
    }

    prefill.to = to.join(", ");
    prefill.cc = cc.join(", ");
    prefill.bcc = bcc.join(", ");
    if prefill.to.is_empty()
        && prefill.cc.is_empty()
        && prefill.bcc.is_empty()
        && prefill.subject.is_empty()
        && prefill.body.is_empty()
    {
        return None;
    }
    Some(prefill)
}

fn push_addresses(out: &mut Vec<String>, raw: &str) {
    let decoded = single_line(&decode_mailto_component(raw));
    for address in decoded.split(',') {
        let address = address.trim();
        if !address.is_empty() && !out.iter().any(|seen| seen.eq_ignore_ascii_case(address)) {
            out.push(address.to_string());
        }
    }
}

fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch == '\r' || ch == '\n' { ' ' } else { ch })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Percent-decode as UTF-8 bytes (invalid sequences are kept literally).
fn decode_mailto_component(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = input
                .get(i + 1..i + 3)
                .filter(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Weak handle to a composer's widgets, for callbacks owned by those widgets:
/// a strong `ComposerWidgets` there is a cycle that keeps a closed composer
/// (editor WebView included) alive. The form root owns the strong handle.
type WeakComposerWidgets = std::rc::Weak<RefCell<Option<ComposerWidgets>>>;

fn upgrade_composer_widgets(holder: &WeakComposerWidgets) -> Option<ComposerWidgets> {
    holder.upgrade()?.borrow().clone()
}

#[derive(Clone)]
pub struct ComposerWidgets {
    pub account_dropdown: gtk::DropDown,
    account_switchable: Rc<Cell<bool>>,
    composer_accounts: Rc<RefCell<Vec<Account>>>,
    composer_identities: Rc<RefCell<HashMap<AccountId, Vec<AccountIdentity>>>>,
    pub from_entry: gtk::Entry,
    from_button: gtk::Button,
    from_popover: gtk::Popover,
    from_list: gtk::ListBox,
    pub(crate) from_choice: Rc<RefCell<ComposeFromChoice>>,
    from_popup_choices: Rc<RefCell<Vec<ComposeFromChoice>>>,
    from_account: Rc<Cell<AccountId>>,
    from_syncing: Rc<Cell<bool>>,
    pub imap_draft: Rc<RefCell<Option<ImapDraftRef>>>,
    /// Crash-recovery file this composer autosaves to.
    draft_slot: Rc<Cell<DraftSlot>>,
    pub include_signature: Rc<Cell<bool>>,
    pub to_field: RecipientField,
    pub cc_field: RecipientField,
    pub bcc_field: RecipientField,
    pub cc_revealer: gtk::Revealer,
    pub bcc_revealer: gtk::Revealer,
    pub cc_toggle: gtk::ToggleButton,
    pub bcc_toggle: gtk::ToggleButton,
    cc_bcc_syncing: Rc<Cell<bool>>,
    pub subject_entry: gtk::Entry,
    pub body_view: gtk::TextView,
    pub html_editor: Rc<ComposeHtmlEditor>,
    pub body_frame: gtk::Frame,
    pub toolbar: ComposeToolbar,
    pub attachments_box: gtk::Box,
    parts: ComposePartsState,
    sending_banner: adw::Banner,
    disclosure_banner: adw::Banner,
    /// Header title; its subtitle reports the draft's save state.
    pub header_title: adw::WindowTitle,
    pgp_disclosure: gtk::Box,
    sign_toggle: gtk::ToggleButton,
    encrypt_toggle: gtk::ToggleButton,
    pgp_explanation: gtk::Label,
    pub pgp_state: Rc<RefCell<Option<PgpAccountState>>>,
    in_reply_to: Rc<RefCell<Option<String>>>,
    references: Rc<RefCell<Option<String>>>,
    thread_id: Rc<Cell<Option<ThreadId>>>,
    approve_contact_email: Rc<RefCell<Option<String>>>,
    /// Drop late autosave flushes after this composer has been handed to Send.
    autosave_suspended: Rc<Cell<bool>>,
    /// Account whose Drafts the last server save went to. When From moves to
    /// another account, that account still holds the copy and must delete it.
    draft_saved_account: Rc<Cell<Option<AccountId>>>,
}

impl ComposerWidgets {
    pub fn draft_slot(&self) -> DraftSlot {
        self.draft_slot.get()
    }

    pub fn set_draft_slot(&self, slot: DraftSlot) {
        self.draft_slot.set(slot);
    }

    /// Autosave this composer's current content to its own recovery file.
    pub fn autosave_local(&self, draft: &ComposeDraft) {
        if self.autosave_suspended.get() {
            return;
        }
        if is_untouched_new_compose(draft, self.draft_saved_account()) {
            // Nothing to recover, and a leftover file would offer
            // "Restore unsent message?" for an empty compose on next launch.
            self.clear_local_autosave();
            return;
        }
        save_compose_draft(self.draft_slot(), draft);
        self.set_draft_status(DraftStatus::SavedLocally);
    }

    pub fn clear_local_autosave(&self) {
        clear_compose_draft(self.draft_slot());
        self.set_draft_status(DraftStatus::New);
    }

    /// Ignore autosave writes until [`Self::resume_autosave`]. Used once a
    /// draft has been queued so a flush that started earlier cannot write it back.
    pub fn suspend_autosave(&self) {
        self.autosave_suspended.set(true);
        self.html_editor.invalidate_pending_flush();
    }

    pub fn resume_autosave(&self) {
        self.autosave_suspended.set(false);
    }

    pub fn autosave_is_suspended(&self) -> bool {
        self.autosave_suspended.get()
    }

    pub fn note_draft_saved_to(&self, account: AccountId) {
        self.draft_saved_account.set(Some(account));
    }

    pub fn draft_saved_account(&self) -> Option<AccountId> {
        self.draft_saved_account.get()
    }

    /// Detach the server draft from this composer, e.g. after From moved to
    /// another account. The next save starts a new Message-ID.
    pub fn take_imap_draft(&self) -> Option<ImapDraftRef> {
        self.draft_saved_account.set(None);
        self.imap_draft.borrow_mut().take()
    }

    /// Show the draft's save state under the composer title.
    pub fn set_draft_status(&self, status: DraftStatus) {
        let subtitle = draft_status_subtitle(status);
        self.header_title.set_subtitle(subtitle);
        if subtitle.is_empty() {
            self.header_title.set_tooltip_text(None);
        } else {
            self.header_title.set_tooltip_text(Some(subtitle));
        }
    }

    pub fn from_account_id(&self) -> AccountId {
        self.from_account.get()
    }

    pub fn identity_id(&self) -> Option<IdentityId> {
        match *self.from_choice.borrow() {
            ComposeFromChoice::Identity(id) => Some(id),
            _ => None,
        }
    }

    pub(crate) fn is_reply_or_forward(&self) -> bool {
        self.in_reply_to.borrow().is_some() || self.thread_id.get().is_some()
    }

    pub(crate) fn inlines_mut(
        &self,
    ) -> std::cell::RefMut<'_, std::collections::HashMap<String, ComposeInline>> {
        self.parts.inlines.borrow_mut()
    }

    pub fn set_sign_toggle_active(&self, active: bool) {
        self.sign_toggle.set_active(active);
    }
}

/// Send button chrome (spinner + label) shared by inline and detached composers.
#[derive(Clone)]
pub struct ComposeSendChrome {
    send_button: gtk::Widget,
    send_icon: gtk::Image,
    send_spinner: adw::Spinner,
    send_label: gtk::Label,
}

impl ComposeSendChrome {
    pub fn set_sending(&self, widgets: &ComposerWidgets, sending: bool) {
        set_composer_sending(widgets, self, sending);
    }
}

pub struct ComposerPage {
    pub root: gtk::Box,
    pub header: adw::HeaderBar,
    pub header_chrome: ComposerHeaderChrome,
    pub widgets: ComposerWidgets,
    pub send_chrome: ComposeSendChrome,
}

/// Inline composer header controls that collapse on narrow windows.
#[derive(Clone)]
pub struct ComposerHeaderChrome {
    title: adw::WindowTitle,
    send_label: gtk::Label,
    help_btn: gtk::Button,
    cc_toggle: gtk::ToggleButton,
    bcc_toggle: gtk::ToggleButton,
    pop_out_btn: gtk::Button,
    overflow_btn: gtk::MenuButton,
}

impl ComposerHeaderChrome {
    /// Drop the title and the Send label, and move Help, Cc, Bcc and Open
    /// in New Window into an overflow menu, so the header fits beside the
    /// window controls. The window clips anything wider, controls included.
    pub fn set_compact(&self, compact: bool) {
        self.title.set_visible(!compact);
        self.send_label.set_visible(!compact);
        self.help_btn.set_visible(!compact);
        self.cc_toggle.set_visible(!compact);
        self.bcc_toggle.set_visible(!compact);
        self.pop_out_btn.set_visible(!compact);
        self.overflow_btn.set_visible(compact);
    }
}

const ACTION_POP_OUT: &str = "composer.pop-out";

/// Holds the header controls that `ComposerHeaderChrome::set_compact` hides.
/// Cc and Bcc mirror their toggles' `active`, so the field logic is shared.
fn build_composer_overflow(
    header: &adw::HeaderBar,
    widgets: &ComposerWidgets,
    on_pop_out: impl Fn() + 'static,
) -> gtk::MenuButton {
    let actions = gtk::gio::SimpleActionGroup::new();
    let pop_out = gtk::gio::SimpleAction::new("pop-out", None);
    pop_out.connect_activate(move |_, _| on_pop_out());
    actions.add_action(&pop_out);
    actions.add_action(&gtk::gio::PropertyAction::new(
        "show-cc",
        &widgets.cc_toggle,
        "active",
    ));
    actions.add_action(&gtk::gio::PropertyAction::new(
        "show-bcc",
        &widgets.bcc_toggle,
        "active",
    ));
    header.insert_action_group("composer", Some(&actions));

    let fields = gtk::gio::Menu::new();
    fields.append(Some(&tr!("Cc")), Some("composer.show-cc"));
    fields.append(Some(&tr!("Bcc")), Some("composer.show-bcc"));
    let menu = gtk::gio::Menu::new();
    menu.append_section(None, &fields);
    menu.append(Some(&tr!("Open in New Window")), Some(ACTION_POP_OUT));
    let help_item = gtk::gio::MenuItem::new(Some(&tr!("Help")), None);
    help_item.set_action_and_target_value(
        Some(crate::menu::ACTION_HELP_TOPIC),
        Some(&TOPIC_COMPOSE.to_variant()),
    );
    menu.append_item(&help_item);

    let button = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .menu_model(&menu)
        .css_classes(["flat", "circular", "alcedo-composer-icon-btn"])
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    crate::a11y::name_icon_control(&button, &tr!("Compose Options"), &tr!("Compose Options"));
    button
}

struct ComposerForm {
    scroll: gtk::ScrolledWindow,
    widgets: ComposerWidgets,
}

pub struct ComposerWindow {
    window: adw::ApplicationWindow,
    pub widgets: ComposerWidgets,
    send_chrome: ComposeSendChrome,
    close_without_confirm: Rc<Cell<bool>>,
    sending: Rc<Cell<bool>>,
}

impl ComposerWindow {
    pub fn present(&self) {
        self.window.present();
    }

    pub fn dialog_parent(&self) -> &adw::ApplicationWindow {
        &self.window
    }

    pub fn account_id(&self) -> AccountId {
        self.widgets.from_account_id()
    }

    pub fn close_after_send(&self) {
        self.close_without_confirm.set(true);
        self.window.close();
    }

    pub fn set_sending(&self, sending: bool) {
        self.sending.set(sending);
        self.send_chrome.set_sending(&self.widgets, sending);
    }

    pub fn is_sending(&self) -> bool {
        self.sending.get()
    }
}

fn build_send_chrome(
    on_send: impl Fn() + Clone + 'static,
    on_send_choice: impl Fn(String) + Clone + 'static,
) -> (gtk::Widget, ComposeSendChrome) {
    let send_icon = gtk::Image::new();
    icons::set_image(&send_icon, icons::SEND_SYMBOLIC, icons::TOOLBAR_PIXEL_SIZE);
    send_icon.add_css_class("alcedo-composer-send-icon");
    let send_spinner = adw::Spinner::builder()
        .width_request(16)
        .height_request(16)
        .visible(false)
        .build();
    crate::a11y::set_label(&send_spinner, &tr!("Sending"));
    let send_label = gtk::Label::builder().label(tr!("Send")).build();
    let inner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    inner.append(&send_icon);
    inner.append(&send_spinner);
    inner.append(&send_label);

    let tooltip = commands::tooltip_with_shortcut(
        &tr!("Send"),
        Some(&commands::format_accel_for_display(SEND_ACCEL)),
    );
    // An extension may add a menu of other ways to send.
    let send_menu = crate::extension::registered()
        .iter()
        .find_map(|extension| extension.send_menu());
    let send_button: gtk::Widget = match send_menu {
        Some(menu) => {
            let popover = (menu.build)(Rc::new(on_send_choice));
            let button = adw::SplitButton::builder()
                .child(&inner)
                .tooltip_text(tooltip)
                .dropdown_tooltip(menu.tooltip)
                .css_classes(["suggested-action", "pill", "alcedo-composer-send"])
                .popover(&popover)
                .build();
            button.connect_clicked(move |_| on_send());
            button.upcast()
        }
        None => {
            let button = gtk::Button::builder()
                .child(&inner)
                .tooltip_text(tooltip)
                .css_classes(["suggested-action", "pill", "alcedo-composer-send"])
                .build();
            button.connect_clicked(move |_| on_send());
            button.upcast()
        }
    };
    send_button.set_valign(gtk::Align::Center);
    // The label hides on narrow windows; keep the accessible name.
    crate::a11y::set_label(&send_button, &tr!("Send"));

    let chrome = ComposeSendChrome {
        send_button: send_button.clone(),
        send_icon,
        send_spinner,
        send_label,
    };
    (send_button, chrome)
}

pub fn set_composer_sending(widgets: &ComposerWidgets, chrome: &ComposeSendChrome, sending: bool) {
    widgets.sending_banner.set_revealed(sending);
    chrome.send_button.set_sensitive(!sending);
    if sending {
        chrome.send_icon.set_visible(false);
        chrome.send_spinner.set_visible(true);
        chrome.send_label.set_label(&tr!("Sending…"));
    } else {
        chrome.send_spinner.set_visible(false);
        chrome.send_icon.set_visible(true);
        chrome.send_label.set_label(&tr!("Send"));
    }
    set_composer_editable(widgets, !sending);
}

fn install_composer_file_drop(target: &gtk::Widget, widgets: &ComposerWidgets) {
    let widgets = widgets.clone();
    let toast_host = widgets.body_view.clone().upcast::<gtk::Widget>();
    install_file_drop(target, move |paths| {
        if !widgets.body_frame.is_sensitive() {
            return;
        }
        let format = widgets.toolbar.current_format();
        widgets.parts.add_attachments_from_paths(
            paths,
            &toast_host,
            &widgets.attachments_box,
            &widgets.body_view,
            format,
            &widgets.html_editor,
        );
    });
}

fn set_composer_editable(widgets: &ComposerWidgets, editable: bool) {
    widgets.to_field.widget().set_sensitive(editable);
    widgets.cc_field.widget().set_sensitive(editable);
    widgets.bcc_field.widget().set_sensitive(editable);
    widgets.cc_toggle.set_sensitive(editable);
    widgets.bcc_toggle.set_sensitive(editable);
    widgets.subject_entry.set_sensitive(editable);
    widgets
        .account_dropdown
        .set_sensitive(editable && widgets.account_switchable.get());
    widgets.from_entry.set_sensitive(editable);
    widgets.from_button.set_sensitive(editable);
    widgets.body_frame.set_sensitive(editable);
    widgets.toolbar.root.set_sensitive(editable);
    widgets.attachments_box.set_sensitive(editable);
}

pub fn build_composer_page(
    book: Rc<RefCell<ContactBook>>,
    sent: Rc<RefCell<Vec<Address>>>,
    runtime: Handle,
    hooks: ComposerHooks,
    on_send: impl Fn() + Clone + 'static,
    on_send_choice: impl Fn(String) + Clone + 'static,
    on_cancel: impl Fn() + Clone + 'static,
    on_pop_out: impl Fn() + Clone + 'static,
) -> ComposerPage {
    let form = build_composer_form(book, sent, runtime, hooks);

    form.widgets.header_title.set_title(&tr!("Compose"));
    let header = adw::HeaderBar::builder()
        .title_widget(&form.widgets.header_title)
        .build();
    header.add_css_class("alcedo-composer-headerbar");
    header.set_centering_policy(adw::CenteringPolicy::Loose);
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);

    // This leaves the composer and discards the draft (after confirming when
    // there is content), so a back chevron misleads; the trash can says what it does.
    let discard_btn = composer_icon_button(
        "user-trash-symbolic",
        &tr!("Discard Draft"),
        &tr!("Discard Draft (Esc)"),
    );
    discard_btn.connect_clicked(move |_| on_cancel());
    header.pack_start(&discard_btn);

    let (send_button, send_chrome) = build_send_chrome(on_send, on_send_choice);
    header.pack_end(&send_button);

    let overflow_btn = build_composer_overflow(&header, &form.widgets, on_pop_out);
    let pop_out_btn = composer_icon_button(
        "window-new-symbolic",
        &tr!("Open in New Window"),
        &tr!("Open in New Window"),
    );
    pop_out_btn.set_action_name(Some(ACTION_POP_OUT));
    header.pack_end(&pop_out_btn);
    header.pack_end(&overflow_btn);

    header.pack_end(&form.widgets.bcc_toggle);
    header.pack_end(&form.widgets.cc_toggle);
    let help_btn = help::help_button(TOPIC_COMPOSE);
    header.pack_end(&help_btn);
    let header_chrome = ComposerHeaderChrome {
        title: form.widgets.header_title.clone(),
        send_label: send_chrome.send_label.clone(),
        help_btn,
        cc_toggle: form.widgets.cc_toggle.clone(),
        bcc_toggle: form.widgets.bcc_toggle.clone(),
        pop_out_btn,
        overflow_btn,
    };

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_top_bar_style(adw::ToolbarStyle::Flat);
    toolbar.set_content(Some(&form.scroll));
    toolbar.set_vexpand(true);
    install_composer_file_drop(toolbar.upcast_ref(), &form.widgets);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_vexpand(true);
    root.append(&toolbar);

    ComposerPage {
        root,
        header,
        header_chrome,
        widgets: form.widgets,
        send_chrome,
    }
}

pub fn open_composer_window(
    book: Rc<RefCell<ContactBook>>,
    sent: Rc<RefCell<Vec<Address>>>,
    runtime: Handle,
    accounts: &[Account],
    identities: &HashMap<AccountId, Vec<AccountIdentity>>,
    title: &str,
    draft: &ComposeDraft,
    focus_body_at_start: bool,
    hooks: ComposerHooks,
    on_send: impl Fn() + Clone + 'static,
    on_send_choice: impl Fn(String) + Clone + 'static,
    on_close: impl Fn() + 'static,
) -> ComposerWindow {
    let app = relm4::main_application();
    let window = adw::ApplicationWindow::builder()
        .application(&app)
        .title(title)
        .icon_name("dev.alcedo.Mail")
        .default_width(720)
        .default_height(640)
        .build();
    window.add_css_class(crate::theme::APPLICATION_FONT_WINDOW_CLASS);

    let form = build_composer_form(book, sent, runtime, hooks);
    let initial_choice = if let Some(identity_id) = draft.identity_id {
        ComposeFromChoice::Identity(identity_id)
    } else if let Some(custom_from) = draft.custom_from.as_deref() {
        if let Some(addr) = alcedo_core::parse_address_token(custom_from) {
            ComposeFromChoice::Custom {
                display_name: addr.name.unwrap_or_default(),
                email: addr.email,
            }
        } else {
            ComposeFromChoice::Account
        }
    } else {
        ComposeFromChoice::Account
    };
    sync_composer_from_accounts(
        &form.widgets,
        accounts,
        identities,
        draft.account_id,
        initial_choice,
    );
    apply_compose_draft(&form.widgets, draft, focus_body_at_start);

    form.widgets.header_title.set_title(title);
    let header = adw::HeaderBar::builder()
        .title_widget(&form.widgets.header_title)
        .build();
    header.add_css_class("alcedo-composer-headerbar");
    header.set_centering_policy(adw::CenteringPolicy::Loose);

    let (send_button, send_chrome) = build_send_chrome(
        {
            let on_send = on_send.clone();
            move || on_send()
        },
        on_send_choice,
    );
    header.pack_end(&send_button);

    header.pack_end(&form.widgets.bcc_toggle);
    header.pack_end(&form.widgets.cc_toggle);
    header.pack_end(&help::help_button(TOPIC_COMPOSE));

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&form.scroll));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_top_bar_style(adw::ToolbarStyle::Flat);
    toolbar.set_content(Some(&toast_overlay));
    install_composer_file_drop(toolbar.upcast_ref(), &form.widgets);
    window.set_content(Some(&toolbar));
    window.connect_map(|window| {
        crate::theme::apply_toplevel_icon(window);
    });

    let send_controller = gtk::ShortcutController::new();
    send_controller.set_scope(gtk::ShortcutScope::Local);
    send_controller.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string(SEND_ACCEL),
        Some(gtk::CallbackAction::new(move |_, _| {
            on_send();
            glib::Propagation::Stop
        })),
    ));
    window.add_controller(send_controller);

    let close_without_confirm = Rc::new(Cell::new(false));
    let sending = Rc::new(Cell::new(false));
    window.connect_close_request({
        let widgets = form.widgets.clone();
        let close_without_confirm = close_without_confirm.clone();
        let sending = sending.clone();
        move |window| {
            if close_without_confirm.get() {
                return glib::Propagation::Proceed;
            }
            if sending.get() {
                return glib::Propagation::Stop;
            }
            if !composer_has_draft(&widgets) {
                return glib::Propagation::Proceed;
            }
            let window = window.clone();
            let skip = close_without_confirm.clone();
            confirm_discard_composer(&widgets, Some(&window), {
                let window = window.clone();
                move || {
                    skip.set(true);
                    window.close();
                }
            });
            glib::Propagation::Stop
        }
    });
    window.connect_destroy(move |_| on_close());

    ComposerWindow {
        window,
        widgets: form.widgets,
        send_chrome,
        close_without_confirm,
        sending,
    }
}

/// Icon-only composer header button with a tooltip and an accessible name.
fn composer_icon_button(icon: &str, label: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .css_classes(["flat", "circular", "alcedo-composer-icon-btn"])
        .focus_on_click(false)
        .build();
    crate::a11y::name_icon_control(&button, label, tooltip);
    button
}

fn cc_bcc_toggle(label: &str, tooltip: &str) -> gtk::ToggleButton {
    gtk::ToggleButton::builder()
        .label(label)
        .tooltip_text(tooltip)
        .css_classes(["flat", "alcedo-composer-cc-toggle"])
        .focus_on_click(false)
        .build()
}

fn build_account_field() -> (gtk::Box, gtk::DropDown) {
    let model = gtk::StringList::new(&[] as &[&str]);
    let dropdown = gtk::DropDown::builder()
        .model(&model)
        .hexpand(true)
        .tooltip_text(tr!("Sending account"))
        .css_classes(["alcedo-composer-from-dropdown"])
        .build();
    dropdown.update_property(&[gtk::accessible::Property::Label(&tr!("Account"))]);

    let label = gtk::Label::builder()
        .label(tr!("Account"))
        .xalign(0.0)
        .valign(gtk::Align::Center)
        .css_classes(["alcedo-recipient-label"])
        .build();
    label.set_width_chars(7);

    let field = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .css_classes(["alcedo-recipient-field", "alcedo-composer-account-field"])
        .build();
    field.append(&label);
    field.append(&dropdown);
    (field, dropdown)
}

fn build_from_field() -> (
    gtk::Box,
    gtk::Entry,
    gtk::Button,
    gtk::Popover,
    gtk::ListBox,
) {
    let entry = gtk::Entry::builder()
        .hexpand(true)
        .has_frame(false)
        .tooltip_text(tr!("From address"))
        .css_classes(["alcedo-composer-from-entry"])
        .build();
    entry.update_property(&[gtk::accessible::Property::Label(&tr!("From"))]);

    let listbox = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .can_focus(true)
        .css_classes(["alcedo-autocomplete"])
        .build();
    listbox.update_property(&[gtk::accessible::Property::Label(&tr!("From addresses"))]);

    let popover = gtk::Popover::builder()
        .autohide(false)
        .can_focus(false)
        .has_arrow(false)
        .css_classes(["alcedo-autocomplete-popover"])
        .child(&listbox)
        .build();
    popover.set_parent(&entry);
    alcedo_widgets::unparent_popover_on_destroy(&entry, &popover);

    let from_button = gtk::Button::builder()
        .icon_name("pan-down-symbolic")
        .tooltip_text(tr!("Choose a From address"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "alcedo-recipient-picker-btn"])
        .focus_on_click(false)
        .build();
    from_button.update_property(&[gtk::accessible::Property::Label(&tr!(
        "Choose a From address"
    ))]);

    let label = gtk::Label::builder()
        .label(tr!("From"))
        .xalign(0.0)
        .valign(gtk::Align::Center)
        .css_classes(["alcedo-recipient-label"])
        .build();
    label.set_width_chars(7);

    let field = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .css_classes(["alcedo-recipient-field", "alcedo-composer-from-field"])
        .build();
    field.append(&label);
    field.append(&entry);
    field.append(&from_button);
    (field, entry, from_button, popover, listbox)
}

fn composer_from_identity_label(identity: &AccountIdentity) -> String {
    let name = identity.display_name.trim();
    if name.is_empty() || name.eq_ignore_ascii_case(&identity.email) {
        identity.email.clone()
    } else {
        format!("{name} <{}>", identity.email)
    }
}

fn compose_from_label_for_choice(
    choice: &ComposeFromChoice,
    account: &Account,
    identities: &[AccountIdentity],
) -> String {
    match choice {
        ComposeFromChoice::Account => account.email.clone(),
        ComposeFromChoice::Identity(id) => identities
            .iter()
            .find(|identity| identity.id == *id)
            .map(composer_from_identity_label)
            .unwrap_or_else(|| account.email.clone()),
        ComposeFromChoice::Custom {
            display_name,
            email,
        } => {
            let name = display_name.trim();
            if name.is_empty() || name.eq_ignore_ascii_case(email) {
                email.clone()
            } else {
                format!("{name} <{email}>")
            }
        }
    }
}

fn committed_email_for_choice(
    choice: &ComposeFromChoice,
    account: &Account,
    identities: &[AccountIdentity],
) -> String {
    match choice {
        ComposeFromChoice::Account => account.email.clone(),
        ComposeFromChoice::Identity(id) => identities
            .iter()
            .find(|identity| identity.id == *id)
            .map(|identity| identity.email.clone())
            .unwrap_or_else(|| account.email.clone()),
        ComposeFromChoice::Custom { email, .. } => email.clone(),
    }
}

fn compose_from_choice_matches_filter(
    choice: &ComposeFromChoice,
    account: &Account,
    identities: &[AccountIdentity],
    needle: &str,
) -> bool {
    if needle.is_empty() {
        return true;
    }
    let label = compose_from_label_for_choice(choice, account, identities);
    label.to_ascii_lowercase().contains(needle)
        || match choice {
            ComposeFromChoice::Account => account.email.to_ascii_lowercase().contains(needle),
            ComposeFromChoice::Identity(id) => identities
                .iter()
                .find(|identity| identity.id == *id)
                .is_some_and(|identity| {
                    identity.email.to_ascii_lowercase().contains(needle)
                        || identity.display_name.to_ascii_lowercase().contains(needle)
                }),
            ComposeFromChoice::Custom {
                display_name,
                email,
            } => {
                email.to_ascii_lowercase().contains(needle)
                    || display_name.to_ascii_lowercase().contains(needle)
            }
        }
}

fn from_popup_choices_for_account(
    _account: &Account,
    identities: &[AccountIdentity],
) -> Vec<ComposeFromChoice> {
    let mut choices = vec![ComposeFromChoice::Account];
    for identity in identities {
        choices.push(ComposeFromChoice::Identity(identity.id));
    }
    choices
}

fn apply_from_choice(
    widgets: &ComposerWidgets,
    choice: ComposeFromChoice,
    account: &Account,
    identities: &[AccountIdentity],
) {
    let label = compose_from_label_for_choice(&choice, account, identities);
    *widgets.from_choice.borrow_mut() = choice;
    widgets.from_entry.set_text(&label);
}

fn refresh_from_popup_list(widgets: &ComposerWidgets, show_full_list: bool) {
    let account_id = widgets.from_account.get();
    let accounts = widgets.composer_accounts.borrow();
    let account = accounts
        .iter()
        .find(|account| account.id == account_id)
        .cloned();
    let identities_map = widgets.composer_identities.borrow();
    let identities = identities_map
        .get(&account_id)
        .map(|values| values.as_slice())
        .unwrap_or(&[]);
    let Some(account) = account else {
        widgets.from_popover.popdown();
        return;
    };

    let all_choices = from_popup_choices_for_account(&account, identities);
    let current_label =
        compose_from_label_for_choice(&widgets.from_choice.borrow(), &account, identities);
    let entry_text = widgets.from_entry.text();
    let filter = if show_full_list || entry_text == current_label {
        None
    } else {
        Some(entry_text.trim().to_ascii_lowercase())
    };

    let choices: Vec<ComposeFromChoice> = if let Some(needle) = filter.as_deref() {
        all_choices
            .iter()
            .filter(|choice| {
                compose_from_choice_matches_filter(choice, &account, identities, needle)
            })
            .cloned()
            .collect()
    } else {
        all_choices
    };

    while let Some(row) = widgets.from_list.row_at_index(0) {
        widgets.from_list.remove(&row);
    }
    *widgets.from_popup_choices.borrow_mut() = choices.clone();
    if choices.is_empty() {
        widgets.from_popover.popdown();
        return;
    }
    for choice in &choices {
        let label_text = compose_from_label_for_choice(choice, &account, identities);
        let row = gtk::ListBoxRow::new();
        row.set_can_focus(false);
        let box_ = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        box_.add_css_class("alcedo-autocomplete-row");
        let label = gtk::Label::builder()
            .label(&label_text)
            .use_markup(false)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["alcedo-autocomplete-label"])
            .build();
        box_.append(&label);
        row.set_child(Some(&box_));
        widgets.from_list.append(&row);
    }
    if !widgets.from_popover.is_visible() {
        widgets.from_popover.popup();
    }
    if let Some(row) = widgets.from_list.row_at_index(0) {
        widgets.from_list.select_row(Some(&row));
    }
}

fn focus_from_autocomplete_list(listbox: &gtk::ListBox, ignore_focus_leave: &Rc<Cell<bool>>) {
    if let Some(row) = listbox.row_at_index(0) {
        listbox.select_row(Some(&row));
    } else {
        listbox.unselect_all();
    }
    ignore_focus_leave.set(true);
    listbox.grab_focus();
    let ignore_focus_leave = ignore_focus_leave.clone();
    glib::idle_add_local(move || {
        ignore_focus_leave.set(false);
        glib::ControlFlow::Break
    });
}

fn navigate_from_autocomplete_list(listbox: &gtk::ListBox, key: gtk::gdk::Key) {
    let mut count = 0;
    while listbox.row_at_index(count).is_some() {
        count += 1;
    }
    if count == 0 {
        return;
    }
    let current = listbox.selected_row().map(|row| row.index()).unwrap_or(-1);
    let next = match key {
        gtk::gdk::Key::Down | gtk::gdk::Key::KP_Down => {
            if current < 0 {
                Some(0)
            } else {
                Some((current + 1).min(count - 1))
            }
        }
        gtk::gdk::Key::Up | gtk::gdk::Key::KP_Up => {
            if current <= 0 {
                None
            } else {
                Some(current - 1)
            }
        }
        _ => None,
    };
    match next {
        Some(index) => {
            if let Some(row) = listbox.row_at_index(index) {
                listbox.select_row(Some(&row));
            }
        }
        None => listbox.unselect_all(),
    }
}

fn select_from_popup_index(
    widgets: &ComposerWidgets,
    index: usize,
    on_from_changed: &Rc<dyn Fn()>,
    on_compose_changed: &Rc<dyn Fn()>,
) {
    let choice = widgets.from_popup_choices.borrow().get(index).cloned();
    let Some(choice) = choice else {
        return;
    };
    let account_id = widgets.from_account.get();
    let accounts = widgets.composer_accounts.borrow();
    let account = accounts.iter().find(|account| account.id == account_id);
    let identities_map = widgets.composer_identities.borrow();
    let identities = identities_map
        .get(&account_id)
        .map(|values| values.as_slice())
        .unwrap_or(&[]);
    if let Some(account) = account {
        widgets.from_syncing.set(true);
        apply_from_choice(widgets, choice, account, identities);
        widgets.from_syncing.set(false);
        widgets.from_popover.popdown();
        on_from_changed();
        on_compose_changed();
    }
}

/// Last checks before a send or scheduled send: every attached file has
/// finished loading and the From field holds a usable address.
pub(crate) fn ready_to_send(widgets: &ComposerWidgets) -> Result<(), &'static str> {
    if widgets.parts.is_reading_files() {
        return Err(trs!("Still attaching files. Try again in a moment."));
    }
    commit_composer_from(widgets)
}

pub(crate) fn commit_composer_from(widgets: &ComposerWidgets) -> Result<(), &'static str> {
    if widgets.from_syncing.get() {
        return Ok(());
    }
    let account_id = widgets.from_account.get();
    let accounts = widgets.composer_accounts.borrow();
    let account = accounts
        .iter()
        .find(|account| account.id == account_id)
        .ok_or_else(|| trs!("Email is invalid"))?;
    let identities_map = widgets.composer_identities.borrow();
    let identities = identities_map
        .get(&account_id)
        .map(|values| values.as_slice())
        .unwrap_or(&[]);
    let text = widgets.from_entry.text();
    if text.trim().is_empty() {
        widgets.from_syncing.set(true);
        apply_from_choice(widgets, ComposeFromChoice::Account, account, identities);
        widgets.from_syncing.set(false);
        return Ok(());
    }
    match resolve_compose_from_text(&text, &account.email, identities) {
        Ok(choice) => {
            widgets.from_syncing.set(true);
            apply_from_choice(widgets, choice, account, identities);
            widgets.from_syncing.set(false);
            Ok(())
        }
        Err(message) => Err(message),
    }
}

fn build_subject_field() -> (gtk::Box, gtk::Entry) {
    let entry = gtk::Entry::builder()
        .hexpand(true)
        .has_frame(false)
        .css_classes(["alcedo-composer-subject-entry"])
        .build();

    let label = gtk::Label::builder()
        .label(tr!("Subject"))
        .xalign(0.0)
        .valign(gtk::Align::Center)
        .css_classes(["alcedo-recipient-label"])
        .build();
    label.set_width_chars(7);

    let field = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .css_classes(["alcedo-composer-subject-field"])
        .build();
    field.append(&label);
    field.append(&entry);
    (field, entry)
}

fn build_composer_form(
    book: Rc<RefCell<ContactBook>>,
    sent: Rc<RefCell<Vec<Address>>>,
    runtime: Handle,
    hooks: ComposerHooks,
) -> ComposerForm {
    let sending_banner = adw::Banner::builder()
        .use_markup(false)
        .title(tr!("Sending message…"))
        .revealed(false)
        .build();
    sending_banner.add_css_class("alcedo-composer-sending-banner");

    let disclosure_banner = adw::Banner::builder()
        .title("")
        .use_markup(false)
        .revealed(false)
        .build();
    refresh_disclosure_banner(&disclosure_banner);

    let sign_toggle = gtk::ToggleButton::builder()
        .icon_name("application-certificate-symbolic")
        .tooltip_text(tr!("Sign this message"))
        .build();
    sign_toggle.update_property(&[gtk::accessible::Property::Label(&tr!(
        "Sign outgoing message"
    ))]);
    let encrypt_toggle = gtk::ToggleButton::builder()
        .icon_name("channel-secure-symbolic")
        .tooltip_text(tr!("Encrypt this message"))
        .build();
    encrypt_toggle.update_property(&[gtk::accessible::Property::Label(&tr!(
        "Encrypt outgoing message"
    ))]);
    let pgp_explanation = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .visible(false)
        .build();
    let pgp_disclosure = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .css_classes(["alcedo-composer-disclosure"])
        .build();
    pgp_disclosure.append(&sign_toggle);
    pgp_disclosure.append(&encrypt_toggle);
    pgp_disclosure.append(&pgp_explanation);

    let refresh_banner = {
        let disclosure_banner = disclosure_banner.downgrade();
        move |_: bool| {
            if let Some(banner) = disclosure_banner.upgrade() {
                refresh_disclosure_banner(&banner);
            }
        }
    };
    settings::disconnect_on_destroy(
        &disclosure_banner,
        [
            settings::on_request_mdn_changed(refresh_banner.clone()),
            settings::on_insert_tracking_pixel_changed(refresh_banner),
        ],
    );

    // The form root, created first so components can tie their lifetime to it.
    let scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(false)
        .build();
    let toast_host_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    // Toasts and dialogs search upward from `toast_widget`, so any widget in
    // the form works. Use an empty hidden leaf: many callbacks inside the form
    // hold it, and holding `toast_host_box` (an ancestor of those callbacks'
    // widgets) formed cycles that kept every closed composer alive.
    let toast_anchor = gtk::Box::builder().visible(false).build();
    toast_host_box.append(&toast_anchor);
    let toast_widget: gtk::Widget = toast_anchor.upcast();

    let to_field =
        RecipientField::new(&tr!("To"), book.clone(), sent.clone(), toast_widget.clone());
    let cc_field =
        RecipientField::new(&tr!("Cc"), book.clone(), sent.clone(), toast_widget.clone());
    let bcc_field = RecipientField::new(&tr!("Bcc"), book, sent, toast_widget.clone());

    let cc_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(180)
        .reveal_child(false)
        .child(cc_field.widget())
        .build();
    let bcc_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(180)
        .reveal_child(false)
        .child(bcc_field.widget())
        .build();

    let cc_toggle = cc_bcc_toggle(&tr!("Cc"), &tr!("Show Cc"));
    let bcc_toggle = cc_bcc_toggle(&tr!("Bcc"), &tr!("Show Bcc"));
    let cc_bcc_syncing = Rc::new(Cell::new(false));
    let (account_field, account_dropdown) = build_account_field();
    let account_switchable = Rc::new(Cell::new(false));
    let composer_accounts = Rc::new(RefCell::new(Vec::<Account>::new()));
    let composer_identities = Rc::new(RefCell::new(
        HashMap::<AccountId, Vec<AccountIdentity>>::new(),
    ));
    let (from_field, from_entry, from_button, from_popover, from_list) = build_from_field();
    let from_choice = Rc::new(RefCell::new(ComposeFromChoice::Account));
    let from_popup_choices = Rc::new(RefCell::new(Vec::<ComposeFromChoice>::new()));
    let from_show_full_list = Rc::new(Cell::new(false));
    let from_ignore_focus_leave = Rc::new(Cell::new(false));
    let from_account = Rc::new(Cell::new(AccountId(Uuid::nil())));
    let from_syncing = Rc::new(Cell::new(false));
    let imap_draft = Rc::new(RefCell::new(None));
    let include_signature = Rc::new(Cell::new(true));
    let (subject_field, subject_entry) = build_subject_field();
    let body_view = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::Word)
        .build();
    apply_compose_plain_spacing(&body_view, settings::editor_font_size());
    body_view.update_property(&[gtk::accessible::Property::Label(&tr!("Message body"))]);

    let html_editor = Rc::new(ComposeHtmlEditor::new(&body_view));
    html_editor.set_on_input(hooks.on_compose_changed.clone());
    let body_frame = gtk::Frame::builder().child(&html_editor.stack).build();
    body_frame.add_css_class("alcedo-composer-body");
    body_frame.set_hexpand(true);
    body_frame.set_vexpand(true);

    let parts = ComposePartsState::new();
    let attachments_box = build_attachments_box();

    let on_html_images_drained = {
        let parts = parts.clone();
        let attachments_box = attachments_box.clone();
        move |drained: Vec<ComposeInline>| {
            parts.append_drained_attachments(drained);
            let on_remove =
                build_remove_handler(parts.attachments.clone(), attachments_box.clone());
            refresh_attachment_chips(&attachments_box, &parts.attachments.borrow(), on_remove);
        }
    };

    let toolbar_holder: Rc<RefCell<Option<ComposeToolbar>>> = Rc::new(RefCell::new(None));

    let on_attach = {
        let parts = parts.clone();
        let attachments_box = attachments_box.clone();
        let toast_widget = toast_widget.clone();
        let body_view = body_view.clone();
        let html_editor = html_editor.clone();
        let toolbar_holder = Rc::downgrade(&toolbar_holder);
        move || {
            open_attach_dialog(&toast_widget, true, false, {
                let parts = parts.clone();
                let attachments_box = attachments_box.clone();
                let toast_widget = toast_widget.clone();
                let body_view = body_view.clone();
                let html_editor = html_editor.clone();
                let toolbar_holder = toolbar_holder.clone();
                move |paths| {
                    let format = toolbar_holder
                        .upgrade()
                        .and_then(|holder| holder.borrow().as_ref().map(|t| t.current_format()))
                        .unwrap_or(ComposeFormat::Plain);
                    parts.add_attachments_from_paths(
                        paths,
                        &toast_widget,
                        &attachments_box,
                        &body_view,
                        format,
                        &html_editor,
                    );
                }
            });
        }
    };

    let on_insert_image = {
        let parts = parts.clone();
        let body_view = body_view.clone();
        let html_editor = html_editor.clone();
        let toast_widget = toast_widget.clone();
        let toolbar_holder = Rc::downgrade(&toolbar_holder);
        move || {
            open_attach_dialog(&toast_widget, false, true, {
                let parts = parts.clone();
                let body_view = body_view.clone();
                let html_editor = html_editor.clone();
                let toolbar_holder = toolbar_holder.clone();
                let toast_widget = toast_widget.clone();
                move |paths| {
                    let format = toolbar_holder
                        .upgrade()
                        .and_then(|holder| holder.borrow().as_ref().map(|t| t.current_format()))
                        .unwrap_or(ComposeFormat::Plain);
                    let Some(path) = paths.into_iter().next() else {
                        return;
                    };
                    let parts = parts.clone();
                    let body_view = body_view.clone();
                    let html_editor = html_editor.clone();
                    let toast_widget = toast_widget.clone();
                    glib::spawn_future_local(async move {
                        let inline = match load_inline_image(path).await {
                            Ok(inline) => inline,
                            Err(message) => {
                                show_composer_toast(&toast_widget, message);
                                return;
                            }
                        };
                        let Some(inline) = parts.try_add_inline(
                            inline,
                            &toast_widget,
                            &body_view,
                            format,
                            &html_editor,
                        ) else {
                            return;
                        };
                        if format == ComposeFormat::Html {
                            if !insert_html_inline_at_cursor(&html_editor, &inline) {
                                parts.inlines.borrow_mut().remove(&inline.content_id);
                                show_composer_toast(&toast_widget, &tr!("Could not read file"));
                            } else if let Some(warning) =
                                inline_image_client_warning(&inline.filename)
                            {
                                show_composer_toast(&toast_widget, &warning);
                            }
                        } else if format == ComposeFormat::Markdown {
                            insert_markdown_inline_at_cursor(&body_view, &inline);
                            if let Some(warning) = inline_image_client_warning(&inline.filename) {
                                show_composer_toast(&toast_widget, &warning);
                            }
                        }
                    });
                }
            });
        }
    };

    let composer_widgets_holder: Rc<RefCell<Option<ComposerWidgets>>> = Rc::new(RefCell::new(None));

    let tools: Rc<[ToolbarTool]> = crate::extension::registered()
        .iter()
        .flat_map(|extension| extension.composer_tools(&runtime))
        .map(|tool| {
            let composer_widgets_holder = Rc::downgrade(&composer_widgets_holder);
            let activate = tool.activate.clone();
            ToolbarTool {
                icon_name: tool.icon_name,
                tooltip: tool.tooltip,
                ctrl_shift_key: tool.ctrl_shift_key,
                run: Rc::new(move || {
                    if let Some(widgets) = upgrade_composer_widgets(&composer_widgets_holder) {
                        activate(&widgets);
                    }
                }),
            }
        })
        .collect();

    let on_compose_changed = hooks.on_compose_changed.clone();
    let toolbar = build_compose_toolbar(
        body_view.clone(),
        html_editor.clone(),
        body_frame.clone(),
        parts.inlines.clone(),
        {
            let from_account = from_account.clone();
            let on_compose_changed = on_compose_changed.clone();
            move |fmt| {
                settings::set_compose_format_for_account(from_account.get(), fmt);
                on_compose_changed();
            }
        },
        on_attach,
        tools,
        on_insert_image,
        on_html_images_drained,
        hooks.on_include_signature_toggled.clone(),
        hooks.on_snippet_chosen.clone(),
        hooks.on_manage_snippets.clone(),
        &hooks.snippets.borrow(),
        scroll.upcast_ref(),
    );
    *toolbar_holder.borrow_mut() = Some(toolbar.clone());

    let headers = gtk::Box::new(gtk::Orientation::Vertical, 2);
    headers.add_css_class("alcedo-composer-headers");
    headers.append(&account_field);
    headers.append(&from_field);
    headers.append(to_field.widget());
    headers.append(&cc_revealer);
    headers.append(&bcc_revealer);
    headers.append(&subject_field);

    account_dropdown.connect_selected_notify({
        let from_syncing = from_syncing.clone();
        let from_account = from_account.clone();
        let from_choice = from_choice.clone();
        let from_entry = from_entry.clone();
        let composer_accounts = composer_accounts.clone();
        let on_from_changed = hooks.on_from_changed.clone();
        let on_compose_changed = on_compose_changed.clone();
        move |dropdown| {
            if from_syncing.get() {
                return;
            }
            let idx = dropdown.selected() as usize;
            let accounts = composer_accounts.borrow();
            let Some(account) = accounts.get(idx) else {
                return;
            };
            let account_id = account.id;
            from_syncing.set(true);
            from_account.set(account_id);
            *from_choice.borrow_mut() = ComposeFromChoice::Account;
            from_entry.set_text(&account.email);
            from_syncing.set(false);
            on_from_changed();
            on_compose_changed();
        }
    });

    from_button.connect_clicked({
        let from_show_full_list = from_show_full_list.clone();
        let widgets_holder = Rc::downgrade(&composer_widgets_holder);
        move |_| {
            let Some(widgets) = upgrade_composer_widgets(&widgets_holder) else {
                return;
            };
            if widgets.from_popover.is_visible() && from_show_full_list.get() {
                widgets.from_popover.popdown();
                return;
            }
            from_show_full_list.set(true);
            refresh_from_popup_list(&widgets, true);
        }
    });
    from_popover.connect_closed({
        let from_show_full_list = from_show_full_list.clone();
        move |_| {
            from_show_full_list.set(false);
        }
    });
    from_entry.connect_changed({
        let widgets_holder = Rc::downgrade(&composer_widgets_holder);
        let from_show_full_list = from_show_full_list.clone();
        move |_| {
            if let Some(widgets) = upgrade_composer_widgets(&widgets_holder).as_ref() {
                if widgets.from_syncing.get() {
                    return;
                }
                refresh_from_popup_list(widgets, from_show_full_list.get());
            }
        }
    });
    let commit_from_with_hooks = {
        let widgets_holder = Rc::downgrade(&composer_widgets_holder);
        let toast_widget = toast_widget.clone();
        let on_from_changed = hooks.on_from_changed.clone();
        let on_compose_changed = on_compose_changed.clone();
        Rc::new(move || {
            let widgets = upgrade_composer_widgets(&widgets_holder);
            let Some(widgets) = widgets else {
                return;
            };
            match commit_composer_from(&widgets) {
                Ok(()) => {
                    on_from_changed();
                    on_compose_changed();
                }
                Err(message) => show_composer_toast(&toast_widget, message),
            }
        })
    };
    from_entry.connect_activate({
        let commit_from_with_hooks = commit_from_with_hooks.clone();
        let widgets_holder = Rc::downgrade(&composer_widgets_holder);
        let on_from_changed = hooks.on_from_changed.clone();
        let on_compose_changed = on_compose_changed.clone();
        move |_| {
            let widgets = upgrade_composer_widgets(&widgets_holder);
            let Some(widgets) = widgets else {
                return;
            };
            if widgets.from_popover.is_visible() {
                let index = widgets
                    .from_list
                    .selected_row()
                    .map(|row| row.index())
                    .filter(|index| *index >= 0)
                    .map(|index| index as usize);
                if let Some(index) = index {
                    select_from_popup_index(&widgets, index, &on_from_changed, &on_compose_changed);
                    return;
                }
            }
            commit_from_with_hooks();
        }
    });
    let key_controller = gtk::EventControllerKey::new();
    key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    key_controller.connect_key_pressed({
        let commit_from_with_hooks = commit_from_with_hooks.clone();
        let widgets_holder = Rc::downgrade(&composer_widgets_holder);
        let from_ignore_focus_leave = from_ignore_focus_leave.clone();
        let on_from_changed = hooks.on_from_changed.clone();
        let on_compose_changed = on_compose_changed.clone();
        move |_, key, _, modifiers| {
            let widgets = upgrade_composer_widgets(&widgets_holder);
            let Some(widgets) = widgets else {
                return glib::Propagation::Proceed;
            };
            if key == gtk::gdk::Key::Escape && widgets.from_popover.is_visible() {
                widgets.from_popover.popdown();
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Tab {
                let shift = modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK);
                if widgets.from_popover.is_visible() && !shift {
                    focus_from_autocomplete_list(&widgets.from_list, &from_ignore_focus_leave);
                    return glib::Propagation::Stop;
                }
                if !shift {
                    commit_from_with_hooks();
                } else if widgets.from_popover.is_visible() {
                    widgets.from_popover.popdown();
                }
                return glib::Propagation::Proceed;
            }
            if widgets.from_popover.is_visible()
                && matches!(
                    key,
                    gtk::gdk::Key::Up
                        | gtk::gdk::Key::Down
                        | gtk::gdk::Key::KP_Up
                        | gtk::gdk::Key::KP_Down
                )
            {
                navigate_from_autocomplete_list(&widgets.from_list, key);
                return glib::Propagation::Stop;
            }
            if matches!(key, gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter)
                && widgets.from_popover.is_visible()
            {
                let index = widgets
                    .from_list
                    .selected_row()
                    .map(|row| row.index())
                    .filter(|index| *index >= 0)
                    .map(|index| index as usize);
                if let Some(index) = index {
                    select_from_popup_index(&widgets, index, &on_from_changed, &on_compose_changed);
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    });
    from_entry.add_controller(key_controller);
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave({
        let commit_from_with_hooks = commit_from_with_hooks.clone();
        let from_list = from_list.clone();
        let from_ignore_focus_leave = from_ignore_focus_leave.clone();
        move |_| {
            if from_ignore_focus_leave.get() {
                return;
            }
            let commit_from_with_hooks = commit_from_with_hooks.clone();
            let from_list = from_list.clone();
            glib::idle_add_local(move || {
                if from_list.has_focus() {
                    return glib::ControlFlow::Break;
                }
                commit_from_with_hooks();
                glib::ControlFlow::Break
            });
        }
    });
    from_entry.add_controller(focus);

    let listbox_focus = gtk::EventControllerFocus::new();
    listbox_focus.connect_leave({
        let widgets_holder = Rc::downgrade(&composer_widgets_holder);
        move |_| {
            let widgets_holder = widgets_holder.clone();
            glib::idle_add_local(move || {
                let Some(widgets) = upgrade_composer_widgets(&widgets_holder) else {
                    return glib::ControlFlow::Break;
                };
                if widgets.from_entry.has_focus() || widgets.from_list.has_focus() {
                    return glib::ControlFlow::Break;
                }
                if widgets.from_popover.is_visible() {
                    widgets.from_popover.popdown();
                }
                glib::ControlFlow::Break
            });
        }
    });
    from_list.add_controller(listbox_focus);

    let listbox_key = gtk::EventControllerKey::new();
    listbox_key.connect_key_pressed({
        let widgets_holder = Rc::downgrade(&composer_widgets_holder);
        let on_from_changed = hooks.on_from_changed.clone();
        let on_compose_changed = on_compose_changed.clone();
        move |_, key, _, modifiers| {
            let Some(widgets) = upgrade_composer_widgets(&widgets_holder) else {
                return glib::Propagation::Proceed;
            };
            let shift = modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            match key {
                gtk::gdk::Key::Escape => {
                    widgets.from_popover.popdown();
                    widgets.from_entry.grab_focus();
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter => {
                    if let Some(index) = widgets
                        .from_list
                        .selected_row()
                        .map(|row| row.index())
                        .filter(|index| *index >= 0)
                    {
                        select_from_popup_index(
                            &widgets,
                            index as usize,
                            &on_from_changed,
                            &on_compose_changed,
                        );
                    }
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Tab if shift => {
                    widgets.from_entry.grab_focus();
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Tab => {
                    if let Some(index) = widgets
                        .from_list
                        .selected_row()
                        .map(|row| row.index())
                        .filter(|index| *index >= 0)
                    {
                        select_from_popup_index(
                            &widgets,
                            index as usize,
                            &on_from_changed,
                            &on_compose_changed,
                        );
                    }
                    glib::Propagation::Proceed
                }
                _ => glib::Propagation::Proceed,
            }
        }
    });
    from_list.add_controller(listbox_key);

    from_list.connect_row_activated({
        let widgets_holder = Rc::downgrade(&composer_widgets_holder);
        let on_from_changed = hooks.on_from_changed.clone();
        let on_compose_changed = on_compose_changed.clone();
        move |_, row| {
            let index = row.index();
            if index < 0 {
                return;
            }
            if let Some(widgets) = upgrade_composer_widgets(&widgets_holder).as_ref() {
                select_from_popup_index(
                    widgets,
                    index as usize,
                    &on_from_changed,
                    &on_compose_changed,
                );
            }
        }
    });

    subject_entry.connect_changed({
        let on_compose_changed = on_compose_changed.clone();
        move |_| on_compose_changed()
    });
    body_view.buffer().connect_changed({
        let on_compose_changed = on_compose_changed.clone();
        move |_| on_compose_changed()
    });
    for field in [&to_field, &cc_field, &bcc_field] {
        let on_compose_changed = on_compose_changed.clone();
        field.set_on_changed(move || on_compose_changed());
    }

    let attachments_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(true)
        .overlay_scrolling(true)
        .hexpand(false)
        .halign(gtk::Align::End)
        .valign(gtk::Align::Center)
        .child(&attachments_box)
        .build();
    attachments_box
        .bind_property("visible", &attachments_scroll, "visible")
        .sync_create()
        .build();

    let chrome = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .css_classes(["alcedo-composer-chrome"])
        .build();
    toolbar.root.set_hexpand(true);
    toolbar.root.set_halign(gtk::Align::Fill);
    chrome.append(&toolbar.root);
    chrome.append(&attachments_scroll);

    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
    box_.add_css_class("alcedo-composer");
    box_.set_hexpand(true);
    box_.append(&sending_banner);
    box_.append(&disclosure_banner);
    box_.append(&pgp_disclosure);
    box_.append(&headers);
    box_.append(&chrome);
    box_.append(&body_frame);
    toast_host_box.set_hexpand(true);
    toast_host_box.append(&box_);

    let pgp_state = Rc::new(RefCell::new(None));

    let draft_slot = Rc::new(Cell::new(DraftSlot::INLINE));
    let autosave_suspended = Rc::new(Cell::new(false));
    let draft_saved_account = Rc::new(Cell::new(None));
    let header_title = adw::WindowTitle::new(&tr!("Compose"), "");
    header_title.add_css_class("alcedo-composer-title");
    let widgets_for_cc = ComposerWidgets {
        account_dropdown: account_dropdown.clone(),
        account_switchable: account_switchable.clone(),
        composer_accounts: composer_accounts.clone(),
        composer_identities: composer_identities.clone(),
        from_entry: from_entry.clone(),
        from_button: from_button.clone(),
        from_popover: from_popover.clone(),
        from_list: from_list.clone(),
        from_choice: from_choice.clone(),
        from_popup_choices: from_popup_choices.clone(),
        from_account: from_account.clone(),
        from_syncing: from_syncing.clone(),
        imap_draft: imap_draft.clone(),
        draft_slot: draft_slot.clone(),
        include_signature: include_signature.clone(),
        to_field: to_field.clone(),
        cc_field: cc_field.clone(),
        bcc_field: bcc_field.clone(),
        cc_revealer: cc_revealer.clone(),
        bcc_revealer: bcc_revealer.clone(),
        cc_toggle: cc_toggle.clone(),
        bcc_toggle: bcc_toggle.clone(),
        cc_bcc_syncing: cc_bcc_syncing.clone(),
        subject_entry: subject_entry.clone(),
        body_view: body_view.clone(),
        html_editor: html_editor.clone(),
        body_frame: body_frame.clone(),
        toolbar: toolbar.clone(),
        attachments_box: attachments_box.clone(),
        parts: parts.clone(),
        sending_banner: sending_banner.clone(),
        disclosure_banner: disclosure_banner.clone(),
        header_title: header_title.clone(),
        pgp_disclosure: pgp_disclosure.clone(),
        sign_toggle: sign_toggle.clone(),
        encrypt_toggle: encrypt_toggle.clone(),
        pgp_explanation: pgp_explanation.clone(),
        pgp_state: pgp_state.clone(),
        in_reply_to: Rc::new(RefCell::new(None)),
        references: Rc::new(RefCell::new(None)),
        thread_id: Rc::new(Cell::new(None)),
        approve_contact_email: Rc::new(RefCell::new(None)),
        autosave_suspended: autosave_suspended.clone(),
        draft_saved_account: draft_saved_account.clone(),
    };

    *composer_widgets_holder.borrow_mut() = Some(widgets_for_cc.clone());
    cc_toggle.connect_toggled({
        let widgets_holder = Rc::downgrade(&composer_widgets_holder);
        move |btn| {
            let Some(widgets) = upgrade_composer_widgets(&widgets_holder) else {
                return;
            };
            if widgets.cc_bcc_syncing.get() {
                return;
            }
            let active = btn.is_active();
            widgets.cc_revealer.set_reveal_child(active);
            btn.set_tooltip_text(if active {
                Some(trs!("Hide Cc"))
            } else {
                Some(trs!("Show Cc"))
            });
            if active {
                widgets.cc_field.focus_entry();
            }
        }
    });

    bcc_toggle.connect_toggled({
        let widgets_holder = Rc::downgrade(&composer_widgets_holder);
        move |btn| {
            let Some(widgets) = upgrade_composer_widgets(&widgets_holder) else {
                return;
            };
            if widgets.cc_bcc_syncing.get() {
                return;
            }
            let active = btn.is_active();
            widgets.bcc_revealer.set_reveal_child(active);
            btn.set_tooltip_text(if active {
                Some(trs!("Hide Bcc"))
            } else {
                Some(trs!("Show Bcc"))
            });
            if active {
                widgets.bcc_field.focus_entry();
            }
        }
    });

    sync_cc_bcc_visibility(&widgets_for_cc);

    scroll.set_child(Some(&toast_host_box));
    disable_viewport_scroll_to_focus(&scroll);

    let widgets = ComposerWidgets {
        account_dropdown,
        account_switchable,
        composer_accounts,
        composer_identities,
        from_entry,
        from_button,
        from_popover,
        from_list,
        from_choice,
        from_popup_choices,
        from_account,
        from_syncing,
        imap_draft,
        draft_slot,
        include_signature: include_signature.clone(),
        to_field,
        cc_field,
        bcc_field,
        cc_revealer,
        bcc_revealer,
        cc_toggle,
        bcc_toggle,
        cc_bcc_syncing,
        subject_entry,
        body_view,
        html_editor,
        body_frame,
        toolbar,
        attachments_box,
        parts,
        sending_banner,
        disclosure_banner,
        header_title,
        pgp_disclosure,
        sign_toggle,
        encrypt_toggle,
        pgp_explanation,
        pgp_state: pgp_state.clone(),
        in_reply_to: Rc::new(RefCell::new(None)),
        references: Rc::new(RefCell::new(None)),
        thread_id: Rc::new(Cell::new(None)),
        approve_contact_email: Rc::new(RefCell::new(None)),
        autosave_suspended,
        draft_saved_account,
    };
    let refresh_recipient_pgp = {
        let widgets_holder = Rc::downgrade(&composer_widgets_holder);
        Rc::new(move || {
            if let Some(widgets) = upgrade_composer_widgets(&widgets_holder) {
                refresh_pgp_disclosure(&widgets, widgets.pgp_state.borrow().as_ref());
            }
        })
    };
    for field in [&widgets.to_field, &widgets.cc_field, &widgets.bcc_field] {
        let refresh = refresh_recipient_pgp.clone();
        field.set_on_changed(move || refresh());
        field.set_on_address_action(hooks.on_address_action.clone());
    }
    *composer_widgets_holder.borrow_mut() = Some(widgets.clone());
    // The form root owns the strong handles (callbacks hold them weakly), so
    // the widgets live exactly as long as the form does.
    scroll.connect_destroy(move |_| {
        composer_widgets_holder.borrow_mut().take();
        toolbar_holder.borrow_mut().take();
    });

    ComposerForm { scroll, widgets }
}

pub fn refresh_pgp_disclosure(widgets: &ComposerWidgets, state: Option<&PgpAccountState>) {
    let has_key = state.and_then(|s| s.own.as_ref()).is_some();
    widgets.pgp_disclosure.set_visible(has_key);
    if !has_key {
        widgets.sign_toggle.set_active(false);
        widgets.encrypt_toggle.set_active(false);
        widgets.encrypt_toggle.set_sensitive(false);
        widgets.pgp_explanation.set_visible(false);
        return;
    }

    let recipients: Vec<String> = widgets
        .to_field
        .strings()
        .into_iter()
        .chain(widgets.cc_field.strings())
        .chain(widgets.bcc_field.strings())
        .collect();
    let peers = state.map(|s| s.peers.as_slice()).unwrap_or(&[]);
    match recipient_encrypt_ready(&recipients, peers, true) {
        EncryptReadiness::Ready => {
            widgets.encrypt_toggle.set_sensitive(true);
            widgets.pgp_explanation.set_visible(false);
        }
        EncryptReadiness::MissingOwn => {
            widgets.encrypt_toggle.set_sensitive(false);
            widgets.encrypt_toggle.set_active(false);
            widgets
                .pgp_explanation
                .set_label(&tr!("Add an OpenPGP key to encrypt messages."));
            widgets.pgp_explanation.set_visible(true);
        }
        EncryptReadiness::Missing(emails) => {
            widgets.encrypt_toggle.set_sensitive(false);
            widgets.encrypt_toggle.set_active(false);
            let list = emails.join(", ");
            widgets.pgp_explanation.set_label(&trf!(
                "Missing public keys for: {recipients}",
                recipients = list
            ));
            widgets.pgp_explanation.set_visible(true);
        }
    }
}

pub fn refresh_disclosure_banner(banner: &adw::Banner) {
    let mdn_on = settings::request_mdn();
    let pixel_on = settings::insert_tracking_pixel();

    let title = match (mdn_on, pixel_on) {
        (true, true) => format!("{} {}", trm!(MDN_DISCLOSURE), trm!(PIXEL_DISCLOSURE)),
        (true, false) => trm!(MDN_DISCLOSURE).to_string(),
        (false, true) => trm!(PIXEL_DISCLOSURE).to_string(),
        (false, false) => String::new(),
    };

    banner.set_title(&title);
    banner.set_revealed(!title.is_empty());
}

fn apply_prefill_fields(widgets: &ComposerWidgets, prefill: &MailtoPrefill) {
    widgets
        .to_field
        .set_from_strings(&split_addresses(&prefill.to));
    widgets
        .cc_field
        .set_from_strings(&split_addresses(&prefill.cc));
    widgets
        .bcc_field
        .set_from_strings(&split_addresses(&prefill.bcc));
    widgets.subject_entry.set_text(&prefill.subject);
    widgets.toolbar.apply_body(
        &widgets.body_view,
        &widgets.body_frame,
        settings::compose_format_for_account(widgets.from_account.get()),
        &prefill.body,
        false,
    );
    sync_cc_bcc_visibility(widgets);
}

/// Build a send payload from the composer widgets.
pub fn compose_draft_from_widgets(widgets: &ComposerWidgets) -> ComposeDraft {
    let format = widgets.toolbar.current_format();
    widgets
        .parts
        .prune_unreferenced_inlines(&widgets.body_view, format, &widgets.html_editor);
    widgets
        .html_editor
        .sync_cid_cache(&widgets.parts.inlines.borrow());
    let inlines = widgets.parts.inlines.borrow().values().cloned().collect();

    ensure_compose_draft_identity(widgets);
    let choice = widgets.from_choice.borrow().clone();
    let (identity_id, custom_from) = match choice {
        ComposeFromChoice::Account => (None, None),
        ComposeFromChoice::Identity(id) => (Some(id), None),
        ComposeFromChoice::Custom {
            display_name,
            email,
        } => {
            let formatted = if display_name.trim().is_empty() {
                email
            } else {
                Address {
                    name: Some(display_name),
                    email,
                }
                .formatted()
            };
            (None, Some(formatted))
        }
    };
    ComposeDraft {
        account_id: widgets.from_account_id(),
        to: widgets.to_field.strings(),
        cc: widgets.cc_field.strings(),
        bcc: widgets.bcc_field.strings(),
        subject: widgets.subject_entry.text().to_string(),
        body_text: widgets.toolbar.body_text_for_send(&widgets.body_view),
        format,
        request_mdn: settings::request_mdn(),
        tracking_pixel_url: settings::effective_tracking_pixel_url(),
        collect_outgoing_contacts: settings::collect_outgoing_contacts(),
        attachments: widgets.parts.attachments.borrow().clone(),
        inlines,
        in_reply_to: widgets.in_reply_to.borrow().clone(),
        references: widgets.references.borrow().clone(),
        thread_id: widgets.thread_id.get(),
        approve_contact_email: widgets.approve_contact_email.borrow().clone(),
        sign: widgets.sign_toggle.is_active(),
        encrypt: widgets.encrypt_toggle.is_active(),
        autocrypt_header: None,
        identity_id,
        custom_from,
        imap_draft: widgets.imap_draft.borrow().clone(),
        reaction_emoji: None,
    }
}

/// Stable Message-ID for one compose session (IMAP APPEND identity).
pub fn compose_message_id_for_email(account_email: &str) -> String {
    let domain = account_email
        .split('@')
        .nth(1)
        .filter(|d| !d.is_empty())
        .unwrap_or("localhost");
    format!("<{}@{}>", uuid::Uuid::new_v4(), domain)
}

pub(crate) fn imap_draft_needs_identity(imap: Option<&ImapDraftRef>) -> bool {
    imap.is_none_or(|draft| draft.message_id.trim().is_empty())
}

pub(crate) fn imap_draft_has_server_uid(imap: &ImapDraftRef) -> bool {
    imap.uid != 0
}

/// A compose that holds only the signature and has never been saved. It is
/// kept off Drafts; once a server copy exists, saves continue as normal so
/// that copy does not go stale.
pub(crate) fn is_untouched_new_compose(draft: &ComposeDraft, saved_to: Option<AccountId>) -> bool {
    saved_to.is_none() && alcedo_html::draft_is_only_signature(draft)
}

/// Whether a server copy may exist for this ref. A composer whose first save
/// is still in flight, or whose APPEND never reported a UID, has only its
/// Message-ID; the worker finds the copy by that.
pub(crate) fn imap_draft_may_be_on_server(imap: &ImapDraftRef) -> bool {
    imap_draft_has_server_uid(imap) || !imap.message_id.trim().is_empty()
}

pub(crate) fn placeholder_imap_draft(message_id: String) -> ImapDraftRef {
    ImapDraftRef {
        folder_id: FolderId(0),
        uid: 0,
        uidvalidity: 0,
        message_id,
        last_internaldate: None,
    }
}

/// Stamp `imap_draft` with a session Message-ID before the first IMAP save.
pub fn ensure_compose_draft_identity(widgets: &ComposerWidgets) {
    if !imap_draft_needs_identity(widgets.imap_draft.borrow().as_ref()) {
        return;
    }
    let account_id = widgets.from_account_id();
    let accounts = widgets.composer_accounts.borrow();
    let account = accounts.iter().find(|account| account.id == account_id);
    let identities_map = widgets.composer_identities.borrow();
    let identities = identities_map
        .get(&account_id)
        .map(|values| values.as_slice())
        .unwrap_or(&[]);
    let email = account
        .map(|account| {
            committed_email_for_choice(&widgets.from_choice.borrow(), account, identities)
        })
        .filter(|email| !email.is_empty())
        .unwrap_or_default();
    let domain_email = if email.is_empty() {
        "localhost"
    } else {
        email.as_str()
    };
    *widgets.imap_draft.borrow_mut() = Some(placeholder_imap_draft(compose_message_id_for_email(
        domain_email,
    )));
}

/// Flush the HTML editor snapshot when needed, then build a draft.
pub fn flush_compose_draft_then(
    widgets: &ComposerWidgets,
    on_ready: impl FnOnce(ComposeDraft) + 'static,
) {
    let account_id = widgets.from_account_id();
    let finish = move |widgets: &ComposerWidgets| {
        let mut draft = compose_draft_from_widgets(widgets);
        draft.account_id = account_id;
        widgets.autosave_local(&draft);
        on_ready(draft);
    };

    let needs_flush = widgets.toolbar.current_format() == ComposeFormat::Html
        && widgets.html_editor.view_mode() == ComposeViewMode::Html;
    if needs_flush {
        let widgets = widgets.clone();
        let html_editor = widgets.html_editor.clone();
        html_editor.flush_html(move || {
            finish(&widgets);
        });
    } else {
        finish(widgets);
    }
}

fn split_addresses(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn apply_compose_draft(
    widgets: &ComposerWidgets,
    draft: &ComposeDraft,
    focus_body_at_start: bool,
) {
    widgets.to_field.set_from_strings(&draft.to);
    widgets.cc_field.set_from_strings(&draft.cc);
    widgets.bcc_field.set_from_strings(&draft.bcc);
    widgets.subject_entry.set_text(&draft.subject);

    widgets.parts.clear();
    *widgets.parts.attachments.borrow_mut() = draft.attachments.clone();
    widgets.parts.inlines.borrow_mut().extend(
        draft
            .inlines
            .iter()
            .map(|inline| (inline.content_id.clone(), inline.clone())),
    );
    let on_remove = build_remove_handler(
        widgets.parts.attachments.clone(),
        widgets.attachments_box.clone(),
    );
    refresh_attachment_chips(
        &widgets.attachments_box,
        &widgets.parts.attachments.borrow(),
        on_remove,
    );

    widgets.toolbar.apply_body(
        &widgets.body_view,
        &widgets.body_frame,
        draft.format,
        &draft.body_text,
        focus_body_at_start,
    );
    if focus_body_at_start && draft.format == ComposeFormat::Html {
        let web_view = widgets.html_editor.web_view.clone();
        glib::idle_add_local(move || {
            web_view.grab_focus();
            glib::ControlFlow::Break
        });
    }
    refresh_disclosure_banner(&widgets.disclosure_banner);
    sync_cc_bcc_visibility(widgets);
    *widgets.imap_draft.borrow_mut() = draft.imap_draft.clone();
    widgets
        .draft_saved_account
        .set(draft.imap_draft.as_ref().map(|_| draft.account_id));
    widgets.include_signature.set(true);
    widgets.toolbar.include_signature.set_active(true);
    widgets.from_syncing.set(true);
    let accounts = widgets.composer_accounts.borrow();
    if let Some(account_idx) = accounts.iter().position(|a| a.id == draft.account_id) {
        widgets.account_dropdown.set_selected(account_idx as u32);
        widgets.from_account.set(draft.account_id);
    }
    let identities_map = widgets.composer_identities.borrow();
    let identities = identities_map
        .get(&draft.account_id)
        .map(|values| values.as_slice())
        .unwrap_or(&[]);
    let choice = if let Some(identity_id) = draft.identity_id {
        ComposeFromChoice::Identity(identity_id)
    } else if let Some(custom_from) = draft.custom_from.as_deref() {
        if let Some(addr) = alcedo_core::parse_address_token(custom_from) {
            ComposeFromChoice::Custom {
                display_name: addr.name.unwrap_or_default(),
                email: addr.email,
            }
        } else {
            ComposeFromChoice::Account
        }
    } else {
        ComposeFromChoice::Account
    };
    if let Some(account) = accounts
        .iter()
        .find(|account| account.id == draft.account_id)
    {
        apply_from_choice(widgets, choice, account, identities);
    }
    widgets.from_syncing.set(false);
    ensure_compose_draft_identity(widgets);
    *widgets.in_reply_to.borrow_mut() = draft.in_reply_to.clone();
    *widgets.references.borrow_mut() = draft.references.clone();
    widgets.thread_id.set(draft.thread_id);
    *widgets.approve_contact_email.borrow_mut() = draft.approve_contact_email.clone();
    widgets.sign_toggle.set_active(draft.sign);
    widgets.encrypt_toggle.set_active(draft.encrypt);
}

fn composer_from_account_label(account: &Account) -> String {
    let label = account.label();
    if label == account.email {
        account.email.clone()
    } else {
        format!("{label} <{}>", account.email)
    }
}

pub fn sync_composer_from_accounts(
    widgets: &ComposerWidgets,
    accounts: &[Account],
    identities: &HashMap<AccountId, Vec<AccountIdentity>>,
    selected: AccountId,
    choice: ComposeFromChoice,
) {
    widgets.from_syncing.set(true);

    if accounts.is_empty() {
        let model = gtk::StringList::new(&[] as &[&str]);
        widgets.account_dropdown.set_model(Some(&model));
        widgets.from_entry.set_text("");
        widgets.account_switchable.set(false);
        widgets.account_dropdown.set_sensitive(false);
        widgets.from_entry.set_sensitive(false);
        widgets.from_button.set_sensitive(false);
        widgets.from_syncing.set(false);
        return;
    }

    *widgets.composer_accounts.borrow_mut() = accounts.to_vec();
    *widgets.composer_identities.borrow_mut() = identities.clone();

    let account_labels: Vec<String> = accounts.iter().map(composer_from_account_label).collect();
    let account_label_refs: Vec<&str> = account_labels.iter().map(String::as_str).collect();
    let account_model = gtk::StringList::new(&account_label_refs);
    widgets.account_dropdown.set_model(Some(&account_model));

    let account_idx = accounts
        .iter()
        .position(|account| account.id == selected)
        .unwrap_or(0);
    widgets.account_dropdown.set_selected(account_idx as u32);
    let account = &accounts[account_idx];
    widgets.from_account.set(account.id);

    let editable = widgets.subject_entry.is_sensitive();
    let account_switchable = accounts.len() >= 2;
    widgets.account_switchable.set(account_switchable);
    widgets
        .account_dropdown
        .set_sensitive(account_switchable && editable);
    widgets.from_entry.set_sensitive(editable);
    widgets.from_button.set_sensitive(editable);

    let account_identities = identities
        .get(&account.id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    apply_from_choice(widgets, choice, account, account_identities);

    widgets.from_syncing.set(false);
}

fn sync_cc_bcc_visibility(widgets: &ComposerWidgets) {
    widgets.cc_bcc_syncing.set(true);

    let cc_active = widgets.cc_field.has_typed_or_chips();
    widgets.cc_toggle.set_active(cc_active);
    widgets.cc_revealer.set_reveal_child(cc_active);
    widgets.cc_toggle.set_tooltip_text(if cc_active {
        Some(trs!("Hide Cc"))
    } else {
        Some(trs!("Show Cc"))
    });

    let bcc_active = widgets.bcc_field.has_typed_or_chips();
    widgets.bcc_toggle.set_active(bcc_active);
    widgets.bcc_revealer.set_reveal_child(bcc_active);
    widgets.bcc_toggle.set_tooltip_text(if bcc_active {
        Some(trs!("Hide Bcc"))
    } else {
        Some(trs!("Show Bcc"))
    });

    widgets.cc_bcc_syncing.set(false);
}

pub fn focus_composer_to(widgets: &ComposerWidgets) {
    widgets.to_field.focus_entry();
}

pub fn composer_has_draft(widgets: &ComposerWidgets) -> bool {
    widgets.parts.prune_unreferenced_inlines(
        &widgets.body_view,
        widgets.toolbar.current_format(),
        &widgets.html_editor,
    );
    let body = widgets.toolbar.body_text_for_send(&widgets.body_view);
    widgets.to_field.has_typed_or_chips()
        || widgets.cc_field.has_typed_or_chips()
        || widgets.bcc_field.has_typed_or_chips()
        || !widgets.subject_entry.text().trim().is_empty()
        || !body.trim().is_empty()
        || !widgets.parts.attachments.borrow().is_empty()
        || !widgets.parts.inlines.borrow().is_empty()
}

pub fn confirm_discard_composer(
    widgets: &ComposerWidgets,
    parent: Option<&impl IsA<gtk::Widget>>,
    on_discard: impl Fn() + 'static,
) {
    if !composer_has_draft(widgets) {
        on_discard();
        return;
    }

    let has_server_draft = widgets
        .imap_draft
        .borrow()
        .as_ref()
        .is_some_and(imap_draft_has_server_uid);
    let dialog = adw::AlertDialog::builder()
        .heading(tr!("Discard draft?"))
        .body(if has_server_draft {
            trs!("This will also delete the copy saved in Drafts on the server.")
        } else {
            trs!("Your message has not been sent.")
        })
        .build();
    dialog.add_response("cancel", &tr!("Keep editing"));
    dialog.add_response("discard", &tr!("Discard"));
    dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.choose(parent, None::<&gtk::gio::Cancellable>, move |response| {
        if response.as_str() == "discard" {
            on_discard();
        }
    });
}

pub fn clear_composer(widgets: &ComposerWidgets) {
    widgets.clear_local_autosave();
    widgets.in_reply_to.replace(None);
    widgets.references.replace(None);
    widgets.thread_id.set(None);
    widgets.approve_contact_email.replace(None);
    *widgets.imap_draft.borrow_mut() = None;
    widgets.draft_saved_account.set(None);
    widgets.include_signature.set(true);
    widgets.toolbar.include_signature.set_active(true);
    let prefill = MAILTO_PREFILL.lock().ok().and_then(|mut slot| slot.take());
    let default_format = settings::compose_format_for_account(widgets.from_account.get());

    refresh_disclosure_banner(&widgets.disclosure_banner);

    if let Some(prefill) = prefill {
        apply_prefill_fields(widgets, &prefill);
    } else {
        widgets.to_field.clear();
        widgets.cc_field.clear();
        widgets.bcc_field.clear();
        widgets.subject_entry.set_text("");
        widgets.parts.clear();
        let on_remove = build_remove_handler(
            widgets.parts.attachments.clone(),
            widgets.attachments_box.clone(),
        );
        refresh_attachment_chips(
            &widgets.attachments_box,
            &widgets.parts.attachments.borrow(),
            on_remove,
        );
        widgets.toolbar.apply_body(
            &widgets.body_view,
            &widgets.body_frame,
            default_format,
            "",
            false,
        );
        sync_cc_bcc_visibility(widgets);
    }
    widgets
        .toolbar
        .set_format(&widgets.body_frame, default_format);
    widgets.sign_toggle.set_active(false);
    widgets.encrypt_toggle.set_active(false);
    widgets.pgp_explanation.set_visible(false);
}

/// Extra pixels under each plain-text line.
///
/// The text view’s own line box is the font’s ascent and descent, about
/// 1.2–1.4em for UI fonts. HTML mail defaults to line-height 1.5. This pad
/// opens plain and Markdown compose up to about that without adding a blank
/// line between returns.
pub fn compose_plain_line_pad(font_px: i32) -> i32 {
    (font_px * 3 / 10).max(2)
}

pub fn apply_compose_plain_spacing(view: &gtk::TextView, font_px: i32) {
    let pad = compose_plain_line_pad(font_px);
    view.set_pixels_above_lines(0);
    view.set_pixels_below_lines(pad);
    view.set_pixels_inside_wrap(pad);
}

#[cfg(test)]
mod tests {
    use super::{
        compose_message_id_for_email, compose_plain_line_pad, composer_from_account_label,
        composer_from_identity_label, from_popup_choices_for_account, imap_draft_has_server_uid,
        imap_draft_needs_identity, parse_mailto_uri, placeholder_imap_draft,
    };
    use alcedo_core::{Account, AccountIdentity, ComposeFromChoice, IdentityId};
    use uuid::Uuid;

    #[test]
    fn compose_plain_line_pad_opens_up_with_font_size() {
        assert_eq!(compose_plain_line_pad(10), 3);
        assert_eq!(compose_plain_line_pad(14), 4);
        assert_eq!(compose_plain_line_pad(24), 7);
    }

    #[test]
    fn composer_from_account_label_includes_email_when_named() {
        let named = Account {
            id: alcedo_core::AccountId(uuid::Uuid::nil()),
            email: "alice@example.com".to_string(),
            username: "imapuser".to_string(),
            split_credentials: false,
            smtp_username: String::new(),
            display_name: "Work".to_string(),
            sender_name: String::new(),
            imap_host: String::new(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 465,
            smtp_starttls: false,
            auth: alcedo_core::AuthMethod::Password,
            accent_color: String::new(),
            avatar_text: String::new(),
            avatar_image: Vec::new(),
        };
        assert_eq!(
            composer_from_account_label(&named),
            "Work <alice@example.com>"
        );
        let unnamed = Account {
            display_name: String::new(),
            username: String::new(),
            ..named
        };
        assert_eq!(composer_from_account_label(&unnamed), "alice@example.com");
    }

    #[test]
    fn from_popup_choices_include_saved_identity() {
        let account = Account {
            id: alcedo_core::AccountId(uuid::Uuid::nil()),
            email: "alice@example.com".to_string(),
            username: "imapuser".to_string(),
            split_credentials: false,
            smtp_username: String::new(),
            display_name: "Work".to_string(),
            sender_name: String::new(),
            imap_host: String::new(),
            imap_port: 993,
            smtp_host: String::new(),
            smtp_port: 465,
            smtp_starttls: false,
            auth: alcedo_core::AuthMethod::Password,
            accent_color: String::new(),
            avatar_text: String::new(),
            avatar_image: Vec::new(),
        };
        let identity_id = IdentityId(Uuid::new_v4());
        let identity = AccountIdentity {
            id: identity_id,
            account_id: account.id,
            display_name: "Hamblin".to_string(),
            email: "hamblin@codeinfusion.example".to_string(),
        };
        assert_eq!(
            composer_from_identity_label(&identity),
            "Hamblin <hamblin@codeinfusion.example>"
        );
        let choices = from_popup_choices_for_account(&account, &[identity]);
        assert_eq!(
            choices,
            vec![
                ComposeFromChoice::Account,
                ComposeFromChoice::Identity(identity_id),
            ]
        );
    }

    #[test]
    fn composer_from_identity_label_bare_email_when_empty_or_same() {
        let empty_name = AccountIdentity {
            id: IdentityId(Uuid::nil()),
            account_id: alcedo_core::AccountId(Uuid::nil()),
            display_name: String::new(),
            email: "alias@example.com".to_string(),
        };
        assert_eq!(
            composer_from_identity_label(&empty_name),
            "alias@example.com"
        );
        let same_name = AccountIdentity {
            id: IdentityId(Uuid::nil()),
            account_id: alcedo_core::AccountId(Uuid::nil()),
            display_name: "alias@example.com".to_string(),
            email: "alias@example.com".to_string(),
        };
        assert_eq!(
            composer_from_identity_label(&same_name),
            "alias@example.com"
        );
        let named = AccountIdentity {
            id: IdentityId(Uuid::nil()),
            account_id: alcedo_core::AccountId(Uuid::nil()),
            display_name: "Work Alias".to_string(),
            email: "alias@example.com".to_string(),
        };
        assert_eq!(
            composer_from_identity_label(&named),
            "Work Alias <alias@example.com>"
        );
    }

    #[test]
    fn parse_simple_mailto() {
        let prefill = parse_mailto_uri("mailto:user@example.com").expect("mailto");
        assert_eq!(prefill.to, "user@example.com");
        assert!(prefill.subject.is_empty());
    }

    #[test]
    fn parse_mailto_with_subject() {
        let prefill =
            parse_mailto_uri("mailto:user@example.com?subject=Hello%20World").expect("mailto");
        assert_eq!(prefill.to, "user@example.com");
        assert_eq!(prefill.subject, "Hello World");
    }

    #[test]
    fn parse_mailto_decodes_utf8_and_keeps_plus() {
        let prefill = parse_mailto_uri(
            "mailto:a+tag@example.com?subject=Caf%C3%A9%20%E2%9C%93+1&body=line1%0D%0Aline2",
        )
        .expect("mailto");
        assert_eq!(prefill.to, "a+tag@example.com");
        assert_eq!(prefill.subject, "Café ✓+1");
        assert_eq!(prefill.body, "line1\nline2");
    }

    #[test]
    fn parse_mailto_merges_to_params_and_repeated_cc() {
        let prefill = parse_mailto_uri(
            "MAILTO:one@example.com?to=two@example.com,%20three@example.com&TO=one@example.com&cc=c1@example.com&cc=c2@example.com",
        )
        .expect("mailto");
        assert_eq!(
            prefill.to,
            "one@example.com, two@example.com, three@example.com"
        );
        assert_eq!(prefill.cc, "c1@example.com, c2@example.com");
    }

    #[test]
    fn parse_mailto_query_only_and_header_injection() {
        let prefill =
            parse_mailto_uri("mailto:?to=x@example.com&subject=Hi%0D%0ABcc:%20evil@example.com")
                .expect("mailto");
        assert_eq!(prefill.to, "x@example.com");
        assert!(!prefill.subject.contains('\n') && !prefill.subject.contains('\r'));
        assert!(parse_mailto_uri("mailto:").is_none());
        assert!(parse_mailto_uri("mailto:?").is_none());
        assert!(parse_mailto_uri("https://example.com").is_none());
        let bad = parse_mailto_uri("mailto:x@example.com?subject=100%").expect("mailto");
        assert_eq!(bad.subject, "100%");
    }

    #[test]
    fn mailto_decoding_rejects_signed_or_short_escapes() {
        assert_eq!(super::decode_mailto_component("a%+1b"), "a%+1b");
        assert_eq!(super::decode_mailto_component("50%"), "50%");
        assert_eq!(super::decode_mailto_component("%4"), "%4");
        assert_eq!(super::decode_mailto_component("%41%c3%a9"), "Aé");
    }

    #[test]
    fn parse_mailto_with_bcc() {
        let prefill =
            parse_mailto_uri("mailto:to@example.com?cc=cc@example.com&bcc=secret@example.com")
                .expect("mailto");
        assert_eq!(prefill.to, "to@example.com");
        assert_eq!(prefill.cc, "cc@example.com");
        assert_eq!(prefill.bcc, "secret@example.com");
    }

    #[test]
    fn compose_message_id_uses_account_domain() {
        let id = compose_message_id_for_email("me@example.com");
        assert!(id.starts_with('<') && id.ends_with("@example.com>"));
        assert!(imap_draft_needs_identity(None));
        assert!(imap_draft_needs_identity(Some(&placeholder_imap_draft(
            String::new()
        ))));
        let stamped = placeholder_imap_draft(id);
        assert!(!imap_draft_needs_identity(Some(&stamped)));
        assert_eq!(stamped.uid, 0);
        assert!(!imap_draft_has_server_uid(&stamped));
    }
}

#[cfg(test)]
mod teardown_tests {
    use super::*;

    /// Closing a composer drops its page; the page's widgets (including the
    /// editor WebView) and every callback it was handed must be freed.
    #[test]
    fn composer_page_is_freed_after_its_window_is_destroyed() {
        crate::test_helpers::gtk_test_run(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("test runtime");
            let sentinel = Rc::new(());
            let s = sentinel.clone();
            let hooks = ComposerHooks {
                on_include_signature_toggled: Rc::new({
                    let s = s.clone();
                    move |_| drop(s.clone())
                }),
                on_snippet_chosen: Rc::new({
                    let s = s.clone();
                    move |_| drop(s.clone())
                }),
                on_manage_snippets: Rc::new({
                    let s = s.clone();
                    move || drop(s.clone())
                }),
                on_from_changed: Rc::new({
                    let s = s.clone();
                    move || drop(s.clone())
                }),
                on_compose_changed: Rc::new({
                    let s = s.clone();
                    move || drop(s.clone())
                }),
                on_address_action: Rc::new({
                    let s = s.clone();
                    move |_, _| drop(s.clone())
                }),
                snippets: Rc::new(RefCell::new(Vec::new())),
            };
            let page = build_composer_page(
                Rc::new(RefCell::new(ContactBook {
                    contacts: Vec::new(),
                    groups: Vec::new(),
                })),
                Rc::new(RefCell::new(Vec::new())),
                runtime.handle().clone(),
                hooks,
                {
                    let s = s.clone();
                    move || drop(s.clone())
                },
                {
                    let s = s.clone();
                    move |_| drop(s.clone())
                },
                {
                    let s = s.clone();
                    move || drop(s.clone())
                },
                move || drop(s.clone()),
            );
            // Recipient chips carry their own callbacks; build some.
            page.widgets.to_field.set_from_strings(&[
                "Ada <ada@example.com>".to_string(),
                "bob@example.com".to_string(),
            ]);
            page.widgets
                .cc_field
                .set_from_strings(&["cat@example.com".to_string()]);
            let mut weak_widgets = Vec::new();
            let mut stack: Vec<gtk::Widget> = vec![page.root.clone().upcast()];
            while let Some(widget) = stack.pop() {
                weak_widgets.push((widget.type_().name().to_string(), widget.downgrade()));
                let mut child = widget.first_child();
                while let Some(current) = child {
                    child = current.next_sibling();
                    stack.push(current);
                }
            }
            let window = gtk::Window::new();
            window.set_child(Some(&page.root));
            drop(page);
            window.destroy();
            drop(window);
            // WebKit may finish tearing a view down from the main loop.
            let context = gtk::glib::MainContext::default();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while weak_widgets.iter().any(|(_, w)| w.upgrade().is_some())
                && std::time::Instant::now() < deadline
            {
                context.iteration(false);
            }
            let mut leaked: Vec<&str> = weak_widgets
                .iter()
                .filter(|(_, weak)| weak.upgrade().is_some())
                .map(|(name, _)| name.as_str())
                .collect();
            leaked.sort_unstable();
            leaked.dedup();
            assert!(leaked.is_empty(), "composer widgets leaked: {leaked:?}");
            assert_eq!(Rc::strong_count(&sentinel), 1, "composer callbacks leaked");
        });
    }
}

#[cfg(test)]
mod header_tests {
    use super::*;

    /// Room for minimize, maximize and close beside the header (~108px with
    /// Adwaita, plus spacing).
    const WINDOW_CONTROLS_ALLOWANCE: i32 = 120;

    fn build_test_page(runtime: &tokio::runtime::Runtime) -> ComposerPage {
        build_composer_page(
            Rc::new(RefCell::new(ContactBook {
                contacts: Vec::new(),
                groups: Vec::new(),
            })),
            Rc::new(RefCell::new(Vec::new())),
            runtime.handle().clone(),
            ComposerHooks::noop(),
            || {},
            |_| {},
            || {},
            || {},
        )
    }

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime")
    }

    #[test]
    fn compact_header_leaves_room_for_window_controls() {
        crate::test_helpers::gtk_test_run(|| {
            let runtime = test_runtime();
            let page = build_test_page(&runtime);
            page.header_chrome.set_compact(true);
            let (min, _, _, _) = page.header.measure(gtk::Orientation::Horizontal, -1);
            let budget = alcedo_core::layout_policy::MIN_WINDOW_WIDTH - WINDOW_CONTROLS_ALLOWANCE;
            assert!(min <= budget, "compact header needs {min}px of {budget}px");
        });
    }

    #[test]
    fn overflow_cc_and_bcc_items_show_the_fields() {
        crate::test_helpers::gtk_test_run(|| {
            let runtime = test_runtime();
            let page = build_test_page(&runtime);
            page.header_chrome.set_compact(true);
            assert!(!page.widgets.cc_revealer.reveals_child());
            assert!(!page.widgets.bcc_revealer.reveals_child());
            assert!(
                page.header
                    .activate_action("composer.show-cc", None)
                    .is_ok()
            );
            assert!(
                page.header
                    .activate_action("composer.show-bcc", None)
                    .is_ok()
            );
            assert!(page.widgets.cc_toggle.is_active());
            assert!(page.widgets.bcc_toggle.is_active());
            assert!(page.widgets.cc_revealer.reveals_child());
            assert!(page.widgets.bcc_revealer.reveals_child());
        });
    }
}
