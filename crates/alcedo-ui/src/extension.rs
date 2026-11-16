//! Extension points for features that live outside this crate.
//!
//! The app builds and runs with no extensions registered. An extension is
//! registered once on the GTK thread before the main window is built, and the
//! hooks below are called from the matching UI code. Every hook has a default
//! that does nothing, so an extension overrides only what it adds.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use alcedo_core::{
    AccountId, CalendarEventDraft, ComposeDraft, Contact, Envelope, ExtensionListId,
    ExtensionMessage, FolderId, ListUnsubscribe, MessageId, SpecialUse, ThreadId, UiCommand,
};
use alcedo_widgets::CidCache;
use relm4::adw;
use relm4::gtk;
use relm4::gtk::{gdk, gio};
use tokio::runtime::Handle;

use crate::activity_log::ActivityLog;
use crate::app::AppMsg;
use crate::composer::ComposerWidgets;
use crate::mail::MailMsg;

/// A message list an extension shows in the mail window, such as a virtual
/// mailbox. The extension fills it with `SyncEvent::EnvelopesReplaced` using
/// `EnvelopeListKind::Extension(id)`.
#[derive(Clone, Debug, PartialEq)]
pub struct MailViewSpec {
    pub id: ExtensionListId,
    /// Sidebar label, and the status bar's folder name.
    pub title: String,
    pub icon_name: &'static str,
    pub tooltip: String,
    pub unread: u32,
    /// Shown after the title in the status bar ("Title · detail").
    pub status_detail: String,
    /// Settings key for this list's column layout.
    pub column_scope: String,
    /// Fill the list by running this search, keeping it in the search bar
    /// (and conversation mode available). `None`: the extension fills it.
    pub search_query: Option<String>,
    /// Group the row under a collapsible sidebar section at the bottom,
    /// instead of after the inboxes.
    pub section: Option<ViewSection>,
    /// The row can be dragged onto another row of the same extension.
    pub reorderable: bool,
    /// Remember this view as the last folder and reopen it at startup.
    pub restore_on_start: bool,
    /// Behaves like the unified inbox: threaded lists, syncing refreshes every
    /// account, "this folder" searches mean the inboxes, new inbox mail
    /// reloads it, and the row menu offers sync and mark-all-read.
    pub inbox_like: bool,
    /// Sort newest first while this list is shown.
    pub sort_newest_first: bool,
    /// Still loading: an empty list shows a spinner instead of `empty_title`.
    pub loading: bool,
    pub empty_title: &'static str,
    pub empty_description: Option<&'static str>,
    /// A toggle in the list header while this view is shown.
    pub header_toggle: Option<HeaderToggleSpec>,
    /// Delete calls [`UiExtension::delete_in_view`] instead of deleting the
    /// mail, and permanent delete is off.
    pub handles_delete: bool,
}

/// A sidebar group at the bottom. An empty `title` gives a plain group of
/// rows after a separator, with no header.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewSection {
    pub title: String,
    pub collapsed: bool,
}

/// An item in an extension view's sidebar context menu.
pub struct ViewMenuItem {
    /// Menu label, with an optional `_` mnemonic.
    pub label: String,
    pub activate: Rc<dyn Fn(&MailContext)>,
}

/// What the search bar's save menu is offered for.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchSaveContext {
    /// The extension view shown, if any.
    pub view: Option<ExtensionListId>,
    /// The query as typed.
    pub query: String,
    /// The query with the chosen search scope written in, for saving.
    pub saved_query: String,
}

/// An item in the search bar's save menu.
pub struct SearchSaveAction {
    pub label: String,
    pub activate: Rc<dyn Fn(&MailContext)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HeaderToggleSpec {
    pub active: bool,
    pub busy: bool,
    pub tooltip: String,
}

/// Why the user left an extension view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewLeave {
    /// A search replaced the list; clearing it returns to the view.
    Search,
    /// The user opened another list.
    OtherList,
}

/// The messages a message action applies to.
#[derive(Clone, Debug, Default)]
pub struct MessageSelection {
    /// The extension view shown, if any.
    pub view: Option<ExtensionListId>,
    /// The message the context menu was opened on, if any.
    pub context: Option<MessageId>,
    /// The selected messages that are loaded in the list.
    pub messages: Vec<Envelope>,
    /// Special use of the folders `messages` are in, where they have one.
    pub folder_roles: HashMap<FolderId, SpecialUse>,
}

impl MessageSelection {
    /// Whether `message` is in a folder with special use `role`.
    pub fn is_in(&self, message: &Envelope, role: &SpecialUse) -> bool {
        self.folder_roles.get(&message.folder_id) == Some(role)
    }

    /// The context-menu message, or else the only selected message.
    pub fn target(&self) -> Option<MessageId> {
        let only = match self.messages.as_slice() {
            [only] => Some(only.id),
            _ => None,
        };
        self.context.or(only)
    }
}

/// An item in the message context menu and the Message menu.
pub struct MessageAction {
    /// Name under the window's `win.` action group, such as `mail-find-x`.
    pub action_name: &'static str,
    /// Menu label, with an optional `_` mnemonic.
    pub label: &'static str,
    /// Whether the action applies to the selection; `None` means always.
    pub enabled: Option<Rc<dyn Fn(&MessageSelection) -> bool>>,
    pub activate: Rc<dyn Fn(&MailContext, &MessageSelection)>,
}

/// The composer's Send button menu, from one extension.
pub struct SendMenu {
    /// Tooltip of the button's dropdown arrow.
    pub tooltip: &'static str,
    /// Build the menu; call `choose(choice)` when the user picks an option.
    pub build: Rc<dyn Fn(Rc<dyn Fn(String)>) -> gtk::Popover>,
    /// Take the checked, finished draft for `choice` instead of sending it now.
    pub send: Rc<dyn Fn(&MailContext, ComposeDraft, String)>,
}

/// A message that was just sent.
#[derive(Clone, Debug)]
pub struct SentMessage {
    pub account_id: AccountId,
    pub thread_id: Option<ThreadId>,
    /// The Message-ID header as sent; empty when unknown.
    pub message_id_hdr: String,
}

/// A button on a toast.
pub struct ToastAction {
    pub label: String,
    pub activate: Rc<dyn Fn(&MailContext)>,
}

/// An item in an address popover (reader headers and recipient chips).
pub struct AddressAction {
    pub label: &'static str,
    /// Called with the account the address was seen in, when known.
    pub activate: Rc<dyn Fn(&MailContext, Option<AccountId>, String)>,
}

/// A meeting invitation from a message, as offered to "Add to calendar".
#[derive(Clone, Debug)]
pub struct CalendarInvite {
    pub drafts: Vec<CalendarEventDraft>,
    pub ics: String,
    /// The invitation attachment's cache key and name; empty for a link.
    pub sha256: String,
    pub filename: String,
}

/// A top-level space beside Mail and Contacts: a rail button, a page in the
/// main window, an entry under Go To and in the command palette.
pub struct Space {
    /// Page name in the main window's stack.
    pub name: &'static str,
    /// Application action that shows it, such as `app.notes`. The app
    /// registers it.
    pub action: &'static str,
    pub title: &'static str,
    /// Rail button icon.
    pub icon: gio::Icon,
    /// Symbolic icon name for the command palette.
    pub palette_icon: &'static str,
    pub page: gtk::Widget,
    /// Called each time the space is shown.
    pub shown: Rc<dyn Fn()>,
}

/// What app-level hooks can do: show spaces, reach the window and its toasts.
#[derive(Clone)]
pub struct AppContext {
    app: relm4::Sender<AppMsg>,
    cmd: relm4::Sender<UiCommand>,
    window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    runtime: Handle,
    activity: Rc<ActivityLog>,
    cid_cache: CidCache,
    mock: bool,
}

/// Parts of the main window an [`AppContext`] hands out.
pub(crate) struct AppParts {
    pub app: relm4::Sender<AppMsg>,
    pub cmd: relm4::Sender<UiCommand>,
    pub window: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    pub runtime: Handle,
    pub activity: Rc<ActivityLog>,
    pub cid_cache: CidCache,
    pub mock: bool,
}

impl AppContext {
    pub(crate) fn new(parts: AppParts) -> Self {
        Self {
            app: parts.app,
            cmd: parts.cmd,
            window: parts.window,
            toasts: parts.toasts,
            runtime: parts.runtime,
            activity: parts.activity,
            cid_cache: parts.cid_cache,
            mock: parts.mock,
        }
    }

    /// Show the space named `name` (see [`Space::name`]).
    pub fn show_space(&self, name: &'static str) {
        self.app.emit(AppMsg::ShowSpace(name));
    }

    pub fn show_mail(&self) {
        self.app.emit(AppMsg::ShowMail);
    }

    pub fn open_preferences(&self) {
        self.app.emit(AppMsg::OpenPreferences);
    }

    pub fn send_command(&self, command: UiCommand) {
        self.cmd.emit(command);
    }

    pub fn send_extension_command(&self, extension: &str, payload: &impl serde::Serialize) {
        self.cmd.emit(UiCommand::Extension(ExtensionMessage::new(
            extension, payload,
        )));
    }

    pub fn window(&self) -> &adw::ApplicationWindow {
        &self.window
    }

    pub fn toasts(&self) -> &adw::ToastOverlay {
        &self.toasts
    }

    pub fn runtime(&self) -> &Handle {
        &self.runtime
    }

    pub fn activity(&self) -> &Rc<ActivityLog> {
        &self.activity
    }

    /// Attachment bytes the reader has loaded, by SHA-256.
    pub fn cid_cache(&self) -> &CidCache {
        &self.cid_cache
    }

    /// The app runs on fixture data (`ALCEDO_MOCK`), with no sync runtime.
    pub fn is_mock(&self) -> bool {
        self.mock
    }
}

/// Go To entry for a registered space.
#[derive(Clone, Debug)]
pub(crate) struct SpaceEntry {
    pub action: &'static str,
    pub title: &'static str,
    pub palette_icon: &'static str,
}

thread_local! {
    static SPACE_ENTRIES: RefCell<Vec<SpaceEntry>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn set_space_entries(entries: Vec<SpaceEntry>) {
    SPACE_ENTRIES.with(|slot| *slot.borrow_mut() = entries);
}

/// Spaces the app installed, for the Go To menu and the command palette.
pub(crate) fn space_entries() -> Vec<SpaceEntry> {
    SPACE_ENTRIES.with(|slot| slot.borrow().clone())
}

/// What mail-window hooks can do in response to the user.
#[derive(Clone)]
pub struct MailContext {
    mail: relm4::Sender<MailMsg>,
    commands: relm4::Sender<UiCommand>,
    window: gtk::Window,
}

impl MailContext {
    pub(crate) fn new(
        mail: relm4::Sender<MailMsg>,
        commands: relm4::Sender<UiCommand>,
        window: gtk::Window,
    ) -> Self {
        Self {
            mail,
            commands,
            window,
        }
    }

    /// The mail window, for dialogs.
    pub fn window(&self) -> &gtk::Window {
        &self.window
    }

    /// Move keyboard focus to the search entry.
    pub fn focus_search(&self) {
        self.mail.emit(MailMsg::FocusSearchEntry);
    }

    /// Send a command to the sync side.
    pub fn send_command(&self, command: UiCommand) {
        self.commands.emit(command);
    }

    /// Send a command to this extension's sync-side handler.
    pub fn send_extension_command(&self, extension: &str, payload: &impl serde::Serialize) {
        self.send_command(UiCommand::Extension(ExtensionMessage::new(
            extension, payload,
        )));
    }

    /// Show the extension view `id`, clearing the list until it is filled.
    pub fn show_view(&self, id: ExtensionListId) {
        self.mail.emit(MailMsg::ShowExtensionView(id));
    }

    /// The extension's views or their state changed; redraw sidebar and chrome.
    pub fn views_changed(&self) {
        self.mail.emit(MailMsg::ExtensionViewsChanged);
    }

    pub fn toast(&self, text: &str) {
        self.mail.emit(MailMsg::ExtensionToast(text.to_string()));
    }

    /// Show a toast built by the extension (for example with a button).
    pub fn show_toast(&self, toast: adw::Toast) {
        self.mail.emit(MailMsg::ExtensionToastWidget(toast));
    }

    /// Open `draft` in the main composer.
    pub fn open_draft(&self, draft: ComposeDraft) {
        self.mail.emit(MailMsg::ExtensionOpenDraft(Box::new(draft)));
    }

    /// Log to the activity log and, unless the main window has focus, show a
    /// desktop notification; clicking it opens `open_message`. `id` names the
    /// notification so a newer one replaces it.
    pub fn notify(
        &self,
        id: &'static str,
        title: &str,
        body: &str,
        open_message: Option<MessageId>,
    ) {
        self.mail.emit(MailMsg::ExtensionNotify {
            id,
            title: title.to_string(),
            body: body.to_string(),
            open_message,
        });
    }
}

/// A button added to every composer's formatting toolbar.
pub struct ComposerTool {
    pub icon_name: &'static str,
    pub tooltip: String,
    /// Ctrl+Shift+`key` runs the tool while the body has focus.
    pub ctrl_shift_key: Option<gdk::Key>,
    pub activate: Rc<dyn Fn(&ComposerWidgets)>,
}

/// A section added to the in-app help guide.
pub struct HelpSection {
    /// Anchor id; also the topic passed to `help::present`.
    pub id: &'static str,
    pub nav_label: &'static str,
    /// The section body, without the enclosing `<section>` tag.
    pub html: String,
    /// The section is inserted after this existing section id.
    pub after: &'static str,
}

/// What the help window exposes to header widgets added by an extension.
#[derive(Clone)]
pub struct HelpWindowContext {
    pub window: adw::Window,
    pub runtime: Handle,
    /// The section the reader is looking at.
    pub current_section: Rc<dyn Fn() -> String>,
}

/// Hooks an extension can implement. All run on the GTK thread.
pub trait UiExtension {
    /// Buttons for a new composer. Called once per composer, so per-composer
    /// state can live in the returned closures.
    fn composer_tools(&self, _runtime: &Handle) -> Vec<ComposerTool> {
        Vec::new()
    }

    /// Add groups to Preferences → Composing.
    fn extend_composing_preferences(
        &self,
        _runtime: &Handle,
        _dialog: &adw::PreferencesDialog,
        _page: &adw::PreferencesPage,
    ) {
    }

    /// Views to list in the mail sidebar, after the inboxes.
    fn mail_views(&self) -> Vec<MailViewSpec> {
        Vec::new()
    }

    /// The user opened view `id` (from the sidebar or [`MailContext::show_view`]).
    fn mail_view_opened(&self, _id: ExtensionListId, _ctx: &MailContext) {}

    /// The user left the extension's views. `ViewLeave::Search` means a search
    /// replaced the list for now and may return to view `id`.
    fn mail_view_left(&self, _id: ExtensionListId, _leave: ViewLeave, _ctx: &MailContext) {}

    /// The header toggle of view `id` was switched to `active`.
    fn header_toggled(&self, _id: ExtensionListId, _active: bool, _ctx: &MailContext) {}

    /// A row of view `id` was activated (double-click or Enter). Return `true`
    /// when handled; `false` opens the message as usual.
    fn activate_in_view(
        &self,
        _id: ExtensionListId,
        _message: MessageId,
        _ctx: &MailContext,
    ) -> bool {
        false
    }

    /// Delete in view `id`, when its spec sets `handles_delete`. The rows are
    /// removed from the list afterwards.
    fn delete_in_view(&self, _id: ExtensionListId, _messages: &[MessageId], _ctx: &MailContext) {}

    /// Sidebar context-menu items for view `id`.
    fn view_menu(&self, _id: ExtensionListId) -> Vec<ViewMenuItem> {
        Vec::new()
    }

    /// Row `dragged` was dropped on row `target` (both this extension's).
    fn views_reordered(
        &self,
        _dragged: ExtensionListId,
        _target: ExtensionListId,
        _ctx: &MailContext,
    ) {
    }

    /// The sidebar section titled `title` was collapsed or expanded.
    fn section_toggled(&self, _title: &str, _ctx: &MailContext) {}

    /// Items for the search bar's save menu.
    fn search_save_actions(&self, _ctx: &SearchSaveContext) -> Vec<SearchSaveAction> {
        Vec::new()
    }

    /// Mail changed somewhere (new mail, flags, moves, the unified inbox
    /// reloaded). Called often; debounce expensive work.
    fn mail_changed(&self, _ctx: &MailContext) {}

    /// The user's own addresses (accounts and identities) changed; also sent
    /// once at startup.
    fn own_addresses_changed(&self, _emails: &[String]) {}

    /// The contact book changed (also sent once at startup).
    fn contacts_changed(&self, _contacts: &[Contact]) {}

    /// Offer to add `invite` to a calendar, with any popover at `anchor`.
    /// Return `true` when handled; otherwise an invitation attachment opens
    /// in the system's calendar app.
    fn add_invite_to_calendar(&self, _anchor: &gtk::Widget, _invite: &CalendarInvite) -> bool {
        false
    }

    /// Spaces to add beside Mail and Contacts. Called once while the main
    /// window is built; keep the space's components alive in the extension.
    fn spaces(&self, _ctx: &AppContext) -> Vec<Space> {
        Vec::new()
    }

    /// A bar shown under the reader header, built once with the reader. The
    /// `alcedo-reader-bar` and `alcedo-reader-bar-title` style classes give it
    /// the app's look.
    fn reader_bar(&self, _ctx: &MailContext) -> Option<gtk::Widget> {
        None
    }

    /// The reader now shows `message` (one selected message) while `view` is
    /// the current extension view, if any.
    fn reader_message_changed(
        &self,
        _view: Option<ExtensionListId>,
        _message: Option<&Envelope>,
        _ctx: &MailContext,
    ) {
    }

    /// Whether replying to `from` should add them as an approved contact
    /// once the reply is sent.
    fn approve_contact_on_reply(
        &self,
        _from: &str,
        _list_unsubscribe: Option<&ListUnsubscribe>,
        _contacts: &[Contact],
    ) -> bool {
        false
    }

    /// Return false to suppress the new-mail notification for `envelope`.
    fn allow_notification(&self, _envelope: &Envelope, _contacts: &[Contact]) -> bool {
        true
    }

    /// Add rows or groups to Preferences → General.
    fn extend_general_preferences(&self, _page: &adw::PreferencesPage) {}

    /// The composer Send button's menu. Only the first extension offering one
    /// is used.
    fn send_menu(&self) -> Option<SendMenu> {
        None
    }

    /// A button for the toast shown after a message is sent.
    fn sent_toast_action(&self, _sent: &SentMessage) -> Option<ToastAction> {
        None
    }

    /// Message context-menu and Message menu items.
    fn message_actions(&self) -> Vec<MessageAction> {
        Vec::new()
    }

    /// Address popover items.
    fn address_actions(&self) -> Vec<AddressAction> {
        Vec::new()
    }

    /// A `SyncEvent::Extension` arrived. Ignore messages for other extensions.
    fn handle_event(&self, _event: &ExtensionMessage, _ctx: &MailContext) {}

    /// Whole pages for the preferences dialog, after Accounts.
    fn preferences_pages(
        &self,
        _runtime: &Handle,
        _dialog: &adw::PreferencesDialog,
    ) -> Vec<adw::PreferencesPage> {
        Vec::new()
    }

    /// CSS loaded after the app's stylesheet, for the extension's widgets.
    fn stylesheet(&self) -> Option<&'static str> {
        None
    }

    /// Extra sections for the help guide.
    fn help_sections(&self) -> Vec<HelpSection> {
        Vec::new()
    }

    /// Widgets packed at the end of the help window's header bar.
    fn help_header_widgets(&self, _ctx: &HelpWindowContext) -> Vec<gtk::Widget> {
        Vec::new()
    }
}

thread_local! {
    static EXTENSIONS: RefCell<Vec<Rc<dyn UiExtension>>> = const { RefCell::new(Vec::new()) };
}

/// Register an extension. Call on the GTK thread before the app starts.
pub fn register(extension: Rc<dyn UiExtension>) {
    EXTENSIONS.with(|list| list.borrow_mut().push(extension));
}

/// Every registered extension's message actions, in registration order. The
/// order is fixed after startup, so an index identifies an action.
pub(crate) fn message_actions() -> Vec<MessageAction> {
    registered()
        .iter()
        .flat_map(|ext| ext.message_actions())
        .collect()
}

/// Every registered extension's address actions, in registration order.
pub(crate) fn address_actions() -> Vec<AddressAction> {
    registered()
        .iter()
        .flat_map(|ext| ext.address_actions())
        .collect()
}

/// Snapshot of the registered extensions, so hooks can re-enter the registry.
pub(crate) fn registered() -> Vec<Rc<dyn UiExtension>> {
    EXTENSIONS.with(|list| list.borrow().clone())
}

/// Insert extension sections into the help HTML, after the sections they name.
/// A section whose anchor is missing is appended at the end of `<main>`.
pub(crate) fn merge_help_sections(html: &str, sections: &[HelpSection]) -> String {
    let mut html = html.to_string();
    for section in sections {
        let body = format!(
            "\n\n      <section id=\"{}\">\n{}\n      </section>",
            section.id, section.html
        );
        let section_at = html
            .find(&format!("<section id=\"{}\">", section.after))
            .and_then(|start| html[start..].find("</section>").map(|end| start + end));
        match section_at {
            Some(end) => html.insert_str(end + "</section>".len(), &body),
            None => {
                if let Some(main_end) = html.find("</main>") {
                    html.insert_str(main_end, &format!("{body}\n    "));
                }
            }
        }
        let link = format!(
            "\n      <a href=\"#{}\">{}</a>",
            section.id, section.nav_label
        );
        let nav_anchor = format!("<a href=\"#{}\">", section.after);
        let nav_at = html
            .find(&nav_anchor)
            .and_then(|start| html[start..].find("</a>").map(|end| start + end));
        match nav_at {
            Some(end) => html.insert_str(end + "</a>".len(), &link),
            None => {
                if let Some(nav_end) = html.find("</nav>") {
                    html.insert_str(nav_end, &format!("{link}\n    "));
                }
            }
        }
    }
    html
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "<nav>\n      <a href=\"#a\">A</a>\n      <a href=\"#c\">C</a>\n    </nav>\n    <main>\n      <section id=\"a\">\n        <h2>A</h2>\n      </section>\n\n      <section id=\"c\">\n        <h2>C</h2>\n      </section>\n    </main>";

    fn section(id: &'static str, after: &'static str) -> HelpSection {
        HelpSection {
            id,
            nav_label: "Added",
            html: format!("<h2>{id}</h2>"),
            after,
        }
    }

    #[test]
    fn help_section_lands_after_the_named_section_and_nav_link() {
        let html = merge_help_sections(PAGE, &[section("b", "a")]);
        let a = html.find("<section id=\"a\">").unwrap();
        let b = html.find("<section id=\"b\">").unwrap();
        let c = html.find("<section id=\"c\">").unwrap();
        assert!(a < b && b < c);
        let nav_a = html.find("href=\"#a\"").unwrap();
        let nav_b = html.find("href=\"#b\"").unwrap();
        let nav_c = html.find("href=\"#c\"").unwrap();
        assert!(nav_a < nav_b && nav_b < nav_c);
    }

    #[test]
    fn help_section_with_unknown_anchor_is_appended() {
        let html = merge_help_sections(PAGE, &[section("z", "missing")]);
        let z = html.find("<section id=\"z\">").unwrap();
        assert!(z > html.find("<section id=\"c\">").unwrap());
        assert!(z < html.find("</main>").unwrap());
        assert!(html.find("href=\"#z\"").unwrap() < html.find("</nav>").unwrap());
    }

    #[test]
    fn no_sections_leaves_html_unchanged() {
        assert_eq!(merge_help_sections(PAGE, &[]), PAGE);
    }
}
