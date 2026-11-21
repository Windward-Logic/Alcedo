//! Message reader with sandboxed WebKit body (P8).

use alcedo_i18n::{ntr, tr, trc, trf, trs};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::rc::Rc;

use alcedo_core::private_fs::{create_private_dir_all, write_private_file_atomic};

use crate::a11y;
use crate::address_popover::attach_address_copy_context_menu;
use crate::attachment_save::{
    SaveError, SaveItem, prepare_save_items, sanitize_filename, write_file_atomic,
    write_items_to_dir,
};
use crate::commands;
use crate::compose_attachments::show_composer_toast;
use crate::extension::CalendarInvite;
use crate::folder_bar::archive_icon_name;
use crate::icons;
use crate::meeting_invite::{
    MeetingBanner, build_meeting_banner, hide_meeting_banner, update_meeting_banner,
};
use crate::meeting_rsvp::offer_add_to_calendar;
use crate::menu::{
    ACTION_ARCHIVE, ACTION_COMPOSE, ACTION_DELETE, ACTION_FORWARD, ACTION_REPLY, ACTION_STAR,
    ACTION_UNSTAR,
};

use crate::message_list::ignore_focus_selection_briefly;
use crate::notifications::copy_text_to_clipboard;
use crate::overflow_tooltip::install_overflow_tooltip;
use crate::pgp_banner::{
    PgpBanner, build_pgp_banner, hide_pgp_banner, update_autocrypt_offer_banner,
    update_pgp_banner_for_message,
};
use crate::settings::{ListDensity, list_density, reader_font_size};
use crate::verification_code::{OtpBanner, build_otp_banner, hide_otp_banner, update_otp_banner};
use alcedo_core::{
    Address, BodyAttachment, Envelope, ListUnsubscribe, MessageId, PgpStatus, extension_of,
    is_calendar_invite_uri, is_calendar_part, parse_ics, show_distinct_address_row,
};
use alcedo_html::{
    SANITIZER_VERSION, SanitizeOptions, adapt_html_for_dark_mode, autolink_html_fragment,
    html_has_remote_images, looks_like_generated_plaintext_html, looks_like_html,
    plaintext_to_fragment, sanitize_html,
};
use alcedo_widgets::css_color::{lookup_css_color, rgba_hex};
use alcedo_widgets::sandbox::{
    CidCache, create_isolated_context, ephemeral_network_session, mail_settings,
    register_cid_scheme,
};
use chrono::{DateTime, Local, Utc};
use relm4::Sender;

use alcedo_widgets::unparent_popover_on_destroy;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::gdk;
use webkit6::prelude::*;
use webkit6::{
    LoadEvent, NavigationPolicyDecision, PolicyDecisionType, Settings, WebContext, WebView,
};

/// Collapsed recipient clip height (~one chip row). GTK CSS has no max-height.
const COLLAPSED_RECIPIENT_HEIGHT: i32 = 26;

/// `AdwSplitButton` treats `label` and `icon-name` as mutually exclusive:
/// setting one clears the other. These stay in sync with the reply/forward
/// button builders below and with `set_reader_reply_icon_only`.
const REPLY_ICON_NAME: &str = "mail-reply-sender-symbolic";
const FORWARD_ICON_NAME: &str = "mail-forward-symbolic";

thread_local! {
    static ENABLED_RISKY_ATTACHMENTS: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

struct AddressField {
    row: gtk::Box,
    chips: adw::WrapBox,
    clip: gtk::ScrolledWindow,
    expand: gtk::Button,
    expanded: Rc<Cell<bool>>,
}

pub struct ReaderWidgets {
    /// App shell sender (meeting banner RSVP, calendar routing).
    pub subject: gtk::Label,
    from: AddressField,
    sender: AddressField,
    reply_to: AddressField,
    pub unsubscribe_btn: gtk::Button,
    pub date: gtk::Label,
    to: AddressField,
    cc: AddressField,
    compose_btn: gtk::Button,
    /// Hidden with the Reply and Forward text on compact layouts.
    compose_label: gtk::Label,
    pub reply_btn: adw::SplitButton,
    /// Enabled only for mail with a List-Post address.
    reply_list_action: gtk::gio::SimpleAction,
    pub forward_btn: adw::SplitButton,
    pub react_btn: gtk::Button,
    pub reactions: adw::WrapBox,
    reaction_popover: gtk::Popover,
    reaction_quick: Vec<(gtk::Button, String)>,
    pub archive_btn: gtk::Button,
    pub star_btn: gtk::ToggleButton,
    pub delete_btn: gtk::Button,
    pub header_bar: adw::HeaderBar,
    pub back_btn: gtk::Button,
    star_syncing: Rc<Cell<bool>>,
    pub meeting_banner: MeetingBanner,
    otp_banner: OtpBanner,
    pgp_banner: PgpBanner,
    attachments_box: adw::WrapBox,
    remote_banner: gtk::Box,
    pub load_remote_btn: adw::SplitButton,
    pub content_stack: gtk::Stack,
    pub empty_page: adw::StatusPage,
    body_stack: gtk::Stack,
    web_view: WebView,
    settings: Settings,
    raw_html: Rc<RefCell<String>>,
    allow_remote: Rc<Cell<bool>>,
    show_original_colours: Rc<Cell<bool>>,
    original_colours_action: gtk::gio::SimpleAction,
    cid_cache: CidCache,
    /// Wrapped HTML for the currently open message only; cleared when the reader empties.
    body_document_cache: Rc<RefCell<Option<ReaderBodyDocumentCache>>>,
}

pub struct ReaderPage {
    pub root: gtk::Box,
    pub widgets: ReaderWidgets,
}

/// Placeholder content shown when no message or multiple messages are selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderPlaceholder {
    Empty,
    Multi { count: u32 },
    OpeningDraft,
}

/// Spoken name of a quick-reaction emoji, for tooltips and screen readers.
fn reaction_name(emoji: &str) -> Option<&'static str> {
    Some(match emoji {
        "👍" => trs!("Thumbs up"),
        "❤️" => trs!("Heart"),
        "😂" => trs!("Laugh"),
        "🎉" => trs!("Celebrate"),
        "😮" => trs!("Surprised"),
        "😢" => trs!("Sad"),
        _ => return None,
    })
}

pub fn multi_placeholder_title(count: u32) -> String {
    ntr!("{n} message selected", "{n} messages selected", count)
}

fn show_pending_attachment_chip(box_: &adw::WrapBox) {
    clear_attachment_chips(box_);
    box_.set_visible(true);
    let card = build_attachment_card(
        "mail-attachment-symbolic",
        &tr!("Attachment"),
        "…",
        false,
        false,
    );
    card.set_tooltip_text(Some(&tr!("Attachments are still loading")));
    box_.append(&card);
}

pub fn show_reader_body_loading(widgets: &ReaderWidgets, show_pending_attachment: bool) {
    hide_meeting_banner(&widgets.meeting_banner);
    hide_otp_banner(&widgets.otp_banner);
    if show_pending_attachment {
        show_pending_attachment_chip(&widgets.attachments_box);
    } else {
        clear_attachment_chips(&widgets.attachments_box);
    }
    widgets.remote_banner.set_visible(false);
    clear_reader_body_document_cache(widgets);
    *widgets.raw_html.borrow_mut() = String::new();
    widgets.content_stack.set_visible_child_name("content");
    widgets.body_stack.set_visible_child_name("loading");
}

fn show_reader_body_document(widgets: &ReaderWidgets) {
    widgets.body_stack.set_visible_child_name("body");
}

pub fn show_reader_placeholder(widgets: &ReaderWidgets, kind: ReaderPlaceholder) {
    match kind {
        ReaderPlaceholder::Empty => {
            widgets.empty_page.set_icon_name(Some("mail-read-symbolic"));
            widgets.empty_page.set_title(&tr!("Select a message"));
            widgets
                .empty_page
                .set_description(Some(&tr!("Choose a message from the list to read it.")));
        }
        ReaderPlaceholder::Multi { count } => {
            let icon = if gtk::IconTheme::default().has_icon("checkbox-checked-symbolic") {
                "checkbox-checked-symbolic"
            } else {
                "mail-unread-symbolic"
            };
            widgets.empty_page.set_icon_name(Some(icon));
            widgets
                .empty_page
                .set_title(&multi_placeholder_title(count));
            widgets
                .empty_page
                .set_description(Some(&tr!("Archive, star, or delete from the toolbar.")));
        }
        ReaderPlaceholder::OpeningDraft => {
            widgets
                .empty_page
                .set_icon_name(Some("document-edit-symbolic"));
            widgets.empty_page.set_title(&tr!("Opening draft…"));
            widgets
                .empty_page
                .set_description(Some(&tr!("Fetching this draft from the server.")));
        }
    }
    widgets.subject.set_label("");
    sync_reader_star(widgets, false);
    widgets.from.reset();
    widgets.date.set_label("");
    widgets.to.reset();
    widgets.cc.reset();
    widgets.cc.row.set_visible(false);
    widgets.reactions.set_visible(false);
    while let Some(child) = widgets.reactions.first_child() {
        widgets.reactions.remove(&child);
    }
    set_reader_actions_sensitive(widgets, false);
    hide_meeting_banner(&widgets.meeting_banner);
    hide_otp_banner(&widgets.otp_banner);
    hide_pgp_banner(&widgets.pgp_banner);
    clear_attachment_chips(&widgets.attachments_box);
    widgets.remote_banner.set_visible(false);
    update_unsubscribe_button(widgets, None, false, false);
    widgets.allow_remote.set(false);
    widgets.settings.set_auto_load_images(false);
    clear_reader_body_document_cache(widgets);
    *widgets.raw_html.borrow_mut() = String::new();
    widgets.content_stack.set_visible_child_name("empty");
}

pub fn build_reader_page(
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
    on_archive: impl Fn() + 'static,
    on_star: impl Fn() + 'static,
    on_unstar: impl Fn() + 'static,
    on_delete: impl Fn() + 'static,
    on_view_source: impl Fn() + 'static,
    on_save_to_file: impl Fn() + 'static,
    on_unsubscribe: impl Fn() + 'static,
    on_otp_copied: impl Fn(MessageId) + 'static,
    extension_bars: Vec<gtk::Widget>,
    on_react: impl Fn() + 'static,
    on_react_emoji: impl Fn(String) + 'static,
    _on_address_click: impl Fn(&gtk::Widget, Address) + Clone + 'static,
    cid_cache: CidCache,
) -> ReaderPage {
    let subject = gtk::Label::builder()
        .css_classes(["title", "alcedo-reader-title"])
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .lines(2)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    install_overflow_tooltip(&subject);

    let remote_menu = gtk::gio::Menu::new();
    remote_menu.append(
        Some(&tr!("Always load images from this sender")),
        Some("reader.load-remote-sender"),
    );

    let remote_actions = gtk::gio::SimpleActionGroup::new();
    let always_from_sender_action = gtk::gio::SimpleAction::new("load-remote-sender", None);
    remote_actions.add_action(&always_from_sender_action);

    let load_remote_btn = adw::SplitButton::builder()
        .label(tr!("Load images"))
        .tooltip_text(tr!("Show images from the web in this message"))
        .css_classes(["pill", "alcedo-reader-remote"])
        .halign(gtk::Align::End)
        .valign(gtk::Align::Center)
        .menu_model(&remote_menu)
        .build();
    load_remote_btn.insert_action_group("reader", Some(&remote_actions));

    let remote_icon = gtk::Image::from_icon_name("image-x-generic-symbolic");
    remote_icon.add_css_class("alcedo-remote-banner-icon");
    remote_icon.set_valign(gtk::Align::Center);

    let remote_label = gtk::Label::builder()
        .label(tr!("Remote images blocked"))
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["alcedo-remote-banner-title"])
        .build();

    let remote_banner = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .css_classes(["alcedo-remote-banner"])
        .visible(false)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    remote_banner.update_property(&[gtk::accessible::Property::Label(&tr!(
        "Remote images blocked"
    ))]);
    remote_banner.append(&remote_icon);
    remote_banner.append(&remote_label);
    remote_banner.append(&load_remote_btn);

    let from = build_recipient_row(true, &tr!("From:"));
    from.row.add_css_class("alcedo-reader-recipients-first");

    let unsubscribe_btn = gtk::Button::builder()
        .label(tr!("Unsubscribe"))
        .tooltip_text(tr!("Unsubscribe from this mailing list"))
        .css_classes(["flat", "alcedo-reader-unsubscribe"])
        .has_frame(false)
        .valign(gtk::Align::Start)
        .visible(false)
        .build();
    unsubscribe_btn.connect_clicked(move |_| on_unsubscribe());
    from.row.append(&unsubscribe_btn);

    let sender = build_recipient_row(false, &tr!("Sender:"));
    let reply_to = build_recipient_row(false, &tr!("Reply-To:"));

    let date = gtk::Label::builder()
        .xalign(1.0)
        .halign(gtk::Align::End)
        .valign(gtk::Align::Center)
        .wrap(false)
        .css_classes(["dim-label", "alcedo-reader-meta", "alcedo-reader-date"])
        .build();

    let to = build_recipient_row(true, &tr!("To:"));
    let cc = build_recipient_row(false, &tr!("Cc:"));

    // Always enabled and independent of the selected message, so unlike Reply
    // and Forward it drives the app-level action instead of a reader callback.
    let compose_btn = gtk::Button::builder()
        .action_name(ACTION_COMPOSE)
        .valign(gtk::Align::Center)
        .build();
    // Icon theme lookup misses this bundled name, so paint it from gresource.
    let compose_icon = gtk::Image::new();
    icons::set_image(
        &compose_icon,
        icons::COMPOSE_SYMBOLIC,
        icons::TOOLBAR_PIXEL_SIZE,
    );
    let compose_label = gtk::Label::new(Some(&tr!("Compose")));
    let compose_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    compose_content.append(&compose_icon);
    compose_content.append(&compose_label);
    compose_btn.set_child(Some(&compose_content));
    a11y::name_icon_control(
        &compose_btn,
        &tr!("New Message"),
        &commands::command_tooltip(ACTION_COMPOSE, &tr!("New Message")),
    );

    let reply_menu = gtk::gio::Menu::new();
    reply_menu.append(Some(&tr!("Reply All")), Some("reader.reply-all"));
    let reply_list_item =
        gtk::gio::MenuItem::new(Some(&tr!("Reply to List")), Some("reader.reply-list"));
    reply_list_item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
    reply_menu.append_item(&reply_list_item);
    let reply_window_section = gtk::gio::Menu::new();
    reply_window_section.append(
        Some(&tr!("Reply in New Window")),
        Some("reader.reply-window"),
    );
    reply_window_section.append(
        Some(&tr!("Reply All in New Window")),
        Some("reader.reply-all-window"),
    );
    reply_menu.append_section(None, &reply_window_section);

    let reply_actions = gtk::gio::SimpleActionGroup::new();
    let reply_all_action = gtk::gio::SimpleAction::new("reply-all", None);
    reply_all_action.connect_activate(move |_, _| on_reply_all());
    reply_actions.add_action(&reply_all_action);
    let reply_window_action = gtk::gio::SimpleAction::new("reply-window", None);
    reply_window_action.connect_activate(move |_, _| on_reply_in_window());
    reply_actions.add_action(&reply_window_action);
    let reply_all_window_action = gtk::gio::SimpleAction::new("reply-all-window", None);
    reply_all_window_action.connect_activate(move |_, _| on_reply_all_in_window());
    reply_actions.add_action(&reply_all_window_action);
    let reply_list_action = gtk::gio::SimpleAction::new("reply-list", None);
    reply_list_action.set_enabled(false);
    reply_list_action.connect_activate(move |_, _| on_reply_list());
    reply_actions.add_action(&reply_list_action);

    let reply_btn = adw::SplitButton::builder()
        .icon_name(REPLY_ICON_NAME)
        .label(tr!("Reply"))
        .css_classes(["suggested-action", "alcedo-reader-reply"])
        .menu_model(&reply_menu)
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    reply_btn.insert_action_group("reader", Some(&reply_actions));
    reply_btn.connect_clicked(move |_| on_reply());
    a11y::name_icon_control(
        &reply_btn,
        &tr!("Reply"),
        &commands::command_tooltip(ACTION_REPLY, &tr!("Reply")),
    );

    let forward_menu = gtk::gio::Menu::new();
    forward_menu.append(
        Some(&tr!("Forward in New Window")),
        Some("reader.forward-window"),
    );

    let forward_actions = gtk::gio::SimpleActionGroup::new();
    let forward_window_action = gtk::gio::SimpleAction::new("forward-window", None);
    forward_window_action.connect_activate(move |_, _| on_forward_in_window());
    forward_actions.add_action(&forward_window_action);

    let forward_btn = adw::SplitButton::builder()
        .icon_name(FORWARD_ICON_NAME)
        .label(tr!("Forward"))
        .css_classes(["alcedo-reader-forward"])
        .menu_model(&forward_menu)
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    forward_btn.insert_action_group("reader", Some(&forward_actions));
    forward_btn.connect_clicked(move |_| on_forward());
    a11y::name_icon_control(
        &forward_btn,
        &tr!("Forward"),
        &commands::command_tooltip(ACTION_FORWARD, &tr!("Forward")),
    );

    let react_btn = gtk::Button::builder()
        .icon_name("face-smile-symbolic")
        .css_classes(["flat", "alcedo-reader-icon"])
        .valign(gtk::Align::Center)
        .sensitive(false)
        .has_frame(false)
        .build();
    a11y::label_icon_control(&react_btn, &tr!("Add reaction"));

    let reaction_popover = gtk::Popover::new();
    reaction_popover.set_parent(&react_btn);
    unparent_popover_on_destroy(&react_btn, &reaction_popover);
    let reaction_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .margin_start(8)
        .margin_end(8)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    let quick_emojis = ["👍", "❤️", "😂", "🎉", "😮", "😢"];
    let mut reaction_quick = Vec::new();
    let on_react_emoji = Rc::new(on_react_emoji);
    for emoji in quick_emojis {
        let button = gtk::Button::builder()
            .label(emoji)
            .tooltip_text(reaction_name(emoji).unwrap_or_default())
            .css_classes(["flat"])
            .build();
        let emoji = emoji.to_string();
        let emoji_for_click = emoji.clone();
        // Weak: these buttons live inside the popover.
        let popover = reaction_popover.downgrade();
        let on_react_emoji = on_react_emoji.clone();
        button.connect_clicked(move |_| {
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
            on_react_emoji(emoji_for_click.clone());
        });
        reaction_row.append(&button);
        reaction_quick.push((button, emoji));
    }
    let more_reactions_btn = gtk::Button::builder()
        .label(tr!("More reactions…"))
        .css_classes(["flat"])
        .build();
    let emoji_chooser = gtk::EmojiChooser::new();
    emoji_chooser.set_parent(&more_reactions_btn);
    unparent_popover_on_destroy(&more_reactions_btn, &emoji_chooser);
    let popover = reaction_popover.downgrade();
    let on_react_emoji = on_react_emoji.clone();
    emoji_chooser.connect_emoji_picked(move |_, emoji| {
        if let Some(popover) = popover.upgrade() {
            popover.popdown();
        }
        on_react_emoji(emoji.to_string());
    });
    more_reactions_btn.connect_clicked(move |_| emoji_chooser.popup());
    reaction_row.append(&more_reactions_btn);
    reaction_popover.set_child(Some(&reaction_row));
    react_btn.connect_clicked(move |_| on_react());

    let reactions = adw::WrapBox::builder()
        .child_spacing(6)
        .line_spacing(6)
        .wrap_policy(adw::WrapPolicy::Minimum)
        .css_classes(["alcedo-reaction-row"])
        .visible(false)
        .build();

    let archive_btn = gtk::Button::builder()
        .icon_name(archive_icon_name())
        .css_classes(["flat", "alcedo-reader-icon"])
        .valign(gtk::Align::Center)
        .sensitive(false)
        .has_frame(false)
        .build();
    a11y::name_icon_control(
        &archive_btn,
        &tr!("Archive"),
        &commands::command_tooltip(ACTION_ARCHIVE, &tr!("Archive")),
    );
    archive_btn.connect_clicked(move |_| on_archive());

    let star_syncing = Rc::new(Cell::new(false));
    let star_btn = gtk::ToggleButton::builder()
        .icon_name("non-starred-symbolic")
        .css_classes(["flat", "alcedo-reader-icon"])
        .valign(gtk::Align::Center)
        .sensitive(false)
        .build();
    a11y::name_icon_control(
        &star_btn,
        &tr!("Star"),
        &commands::command_tooltip(ACTION_STAR, &tr!("Star")),
    );
    {
        let star_syncing = star_syncing.clone();
        star_btn.connect_toggled(move |btn| {
            if star_syncing.get() {
                return;
            }
            if btn.is_active() {
                on_star();
            } else {
                on_unstar();
            }
        });
    }

    let delete_btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .css_classes(["flat", "alcedo-reader-icon"])
        .valign(gtk::Align::Center)
        .sensitive(false)
        .has_frame(false)
        .build();
    a11y::name_icon_control(
        &delete_btn,
        &tr!("Delete"),
        &commands::command_tooltip(ACTION_DELETE, &tr!("Delete")),
    );
    delete_btn.connect_clicked(move |_| on_delete());

    let back_btn = gtk::Button::builder()
        .label(tr!("Back to Messages"))
        .tooltip_text(tr!("Back to Messages"))
        .visible(false)
        .build();

    let header_bar = adw::HeaderBar::new();
    header_bar.set_show_title(false);
    header_bar.set_show_start_title_buttons(false);
    header_bar.set_show_end_title_buttons(false);
    let drag = gtk::WindowHandle::new();
    drag.set_hexpand(true);
    header_bar.set_title_widget(Some(&drag));
    header_bar.pack_start(&back_btn);
    header_bar.pack_start(&compose_btn);
    header_bar.pack_start(&reply_btn);
    header_bar.pack_start(&forward_btn);
    header_bar.pack_end(&react_btn);
    header_bar.pack_end(&delete_btn);
    header_bar.pack_end(&star_btn);
    header_bar.pack_end(&archive_btn);

    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .css_classes(["alcedo-reader-header"])
        .hexpand(true)
        .halign(gtk::Align::Fill)
        .build();
    header.append(&subject);
    header.append(&date);

    let attachments_box = adw::WrapBox::builder()
        .child_spacing(6)
        .line_spacing(6)
        .line_homogeneous(false)
        .justify(adw::JustifyMode::None)
        .wrap_policy(adw::WrapPolicy::Minimum)
        .hexpand(true)
        .halign(gtk::Align::Fill)
        .css_classes(["alcedo-reader-attachments"])
        .visible(false)
        .build();
    let attachments_clip = clip_h_natural(&attachments_box);
    attachments_clip.set_propagate_natural_height(true);
    attachments_box
        .bind_property("visible", &attachments_clip, "visible")
        .sync_create()
        .build();

    let mail_context = create_mail_context(cid_cache.clone());
    let settings = mail_settings();
    let web_view = WebView::builder()
        .web_context(&mail_context)
        .network_session(&ephemeral_network_session())
        .settings(&settings)
        .build();
    // WebKitGTK reports a large default size request that would floor the
    // window's minimum width (same pattern as the composer WebView).
    web_view.set_hexpand(true);
    web_view.set_vexpand(true);
    web_view.set_size_request(1, 1);
    wire_reader_link_tooltip(&web_view);

    let loading_page = adw::StatusPage::builder()
        .title(tr!("Loading message…"))
        .build();
    let loading_paintable = adw::SpinnerPaintable::new(Some(&loading_page));
    loading_page.set_paintable(Some(&loading_paintable));
    let body_stack = gtk::Stack::new();
    body_stack.set_hexpand(true);
    body_stack.set_vexpand(true);
    body_stack.add_named(&loading_page, Some("loading"));
    body_stack.add_named(&web_view, Some("body"));
    body_stack.set_visible_child_name("body");

    let raw_html = Rc::new(RefCell::new(String::new()));
    let body_document_cache = Rc::new(RefCell::new(None));
    let allow_remote = Rc::new(Cell::new(false));
    let show_original_colours = Rc::new(Cell::new(false));
    apply_web_view_background(
        &web_view,
        &canvas_paint(&web_view, show_original_colours.get()).rgba,
    );
    let on_load_remote = Rc::new(on_load_remote);
    let on_always_from_sender = Rc::new(on_always_from_sender);

    let empty_page = adw::StatusPage::builder()
        .icon_name("mail-read-symbolic")
        .title(tr!("Select a message"))
        .description(tr!("Choose a message from the list to read it."))
        .build();

    let content_stack = gtk::Stack::new();
    content_stack.set_hexpand(true);
    content_stack.set_vexpand(true);
    content_stack.add_named(&empty_page, Some("empty"));
    content_stack.set_visible_child_name("empty");

    load_remote_btn.connect_clicked({
        let widgets_for_reload = ReaderReloadState {
            web_view: web_view.clone(),
            raw_html: raw_html.clone(),
            allow_remote: allow_remote.clone(),
            show_original_colours: show_original_colours.clone(),
            remote_banner: remote_banner.clone(),
            body_document_cache: body_document_cache.clone(),
        }
        .downgrade();
        let on_load_remote = on_load_remote.clone();
        let settings = settings.clone();
        let allow_remote = allow_remote.clone();
        move |_| {
            let Some(widgets_for_reload) = widgets_for_reload.upgrade() else {
                return;
            };
            allow_remote.set(true);
            settings.set_auto_load_images(true);
            reload_reader_body(&widgets_for_reload);
            on_load_remote();
        }
    });

    always_from_sender_action.connect_activate({
        let widgets_for_reload = ReaderReloadState {
            web_view: web_view.clone(),
            raw_html: raw_html.clone(),
            allow_remote: allow_remote.clone(),
            show_original_colours: show_original_colours.clone(),
            remote_banner: remote_banner.clone(),
            body_document_cache: body_document_cache.clone(),
        }
        .downgrade();
        let on_load_remote = on_load_remote.clone();
        let on_always_from_sender = on_always_from_sender.clone();
        let settings = settings.clone();
        let allow_remote = allow_remote.clone();
        move |_, _| {
            let Some(widgets_for_reload) = widgets_for_reload.upgrade() else {
                return;
            };
            allow_remote.set(true);
            settings.set_auto_load_images(true);
            reload_reader_body(&widgets_for_reload);
            on_load_remote();
            on_always_from_sender();
        }
    });

    let reader_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    reader_box.set_css_classes(&["alcedo-reader"]);
    reader_box.set_hexpand(true);
    reader_box.set_vexpand(true);
    reader_box.set_overflow(gtk::Overflow::Hidden);
    attach_subject_copy_context_menu(&subject, reader_box.upcast_ref::<gtk::Widget>());

    let header_zone = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["alcedo-reader-header-zone"])
        .build();
    header_zone.append(&header);
    header_zone.append(&from.row);
    header_zone.append(&sender.row);
    header_zone.append(&reply_to.row);
    header_zone.append(&to.row);
    header_zone.append(&cc.row);
    header_zone.append(&reactions);

    let source_menu = gtk::gio::Menu::new();
    source_menu.append(Some(&tr!("View Source")), Some("reader.view-source"));
    source_menu.append(Some(&tr!("Save to File…")), Some("reader.save-to-file"));
    source_menu.append(
        Some(&tr!("Show Original Colors")),
        Some("reader.original-colours"),
    );

    let source_actions = gtk::gio::SimpleActionGroup::new();
    let view_source_action = gtk::gio::SimpleAction::new("view-source", None);
    view_source_action.connect_activate(move |_, _| on_view_source());
    source_actions.add_action(&view_source_action);
    let save_to_file_action = gtk::gio::SimpleAction::new("save-to-file", None);
    save_to_file_action.connect_activate(move |_, _| on_save_to_file());
    source_actions.add_action(&save_to_file_action);
    let original_action =
        gtk::gio::SimpleAction::new_stateful("original-colours", None, &false.to_variant());
    original_action.connect_change_state({
        // Weak: this action is installed on `reader_box`, which contains the
        // WebView and the banner.
        let reload_state = ReaderReloadState {
            web_view: web_view.clone(),
            raw_html: raw_html.clone(),
            allow_remote: allow_remote.clone(),
            show_original_colours: show_original_colours.clone(),
            remote_banner: remote_banner.clone(),
            body_document_cache: body_document_cache.clone(),
        }
        .downgrade();
        let show_original_colours = show_original_colours.clone();
        move |action, value| {
            let Some(value) = value else {
                return;
            };
            let on = value.get::<bool>().unwrap_or(false);
            action.set_state(&on.to_variant());
            show_original_colours.set(on);
            if let Some(reload_state) = reload_state.upgrade() {
                reload_reader_body(&reload_state);
            }
        }
    });
    source_actions.add_action(&original_action);
    reader_box.insert_action_group("reader", Some(&source_actions));
    header_bar.insert_action_group("reader", Some(&source_actions));

    let overflow_btn = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .menu_model(&source_menu)
        .css_classes(["flat"])
        .build();
    a11y::name_icon_control(
        &overflow_btn,
        &tr!("Message Options"),
        &tr!("Message Options"),
    );
    header_bar.pack_end(&overflow_btn);

    let context_menu = gtk::PopoverMenu::from_model(Some(&source_menu));
    context_menu.set_has_arrow(false);
    unparent_popover_on_destroy(&header_zone, &context_menu);

    let context_gesture = gtk::GestureClick::new();
    context_gesture.set_button(3);
    let context_menu = context_menu.clone();
    context_gesture.connect_pressed(move |gesture, _, x, y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        // Read the header zone from the gesture; capturing it would be a cycle.
        let Some(header_zone) = gesture.widget() else {
            return;
        };
        context_menu.set_parent(&header_zone);
        let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        context_menu.set_pointing_to(Some(&rect));
        context_menu.popup();
    });
    header_zone.add_controller(context_gesture);

    let meeting_banner = build_meeting_banner();
    wire_navigation_policy(&web_view, &meeting_banner, on_mailto);
    let otp_banner = build_otp_banner(on_otp_copied);
    let pgp_banner = build_pgp_banner();

    reader_box.append(&header_zone);
    reader_box.append(&otp_banner.root);
    reader_box.append(&pgp_banner.root);
    for bar in &extension_bars {
        reader_box.append(bar);
    }
    reader_box.append(&meeting_banner.root);
    reader_box.append(&attachments_clip);
    reader_box.append(&remote_banner);
    reader_box.append(&body_stack);
    content_stack.add_named(&reader_box, Some("content"));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.set_vexpand(true);
    root.set_hexpand(true);
    root.set_overflow(gtk::Overflow::Hidden);
    root.append(&content_stack);

    ReaderPage {
        root,
        widgets: ReaderWidgets {
            subject,
            from,
            sender,
            reply_to,
            unsubscribe_btn,
            date,
            to,
            cc,
            compose_btn,
            compose_label,
            reply_btn,
            reply_list_action,
            forward_btn,
            react_btn,
            reactions,
            reaction_popover,
            reaction_quick,
            archive_btn,
            star_btn,
            delete_btn,
            header_bar,
            back_btn,
            star_syncing,
            meeting_banner,
            otp_banner,
            pgp_banner,
            attachments_box,
            remote_banner,
            load_remote_btn,
            content_stack,
            empty_page,
            body_stack,
            web_view,
            settings,
            raw_html,
            allow_remote,
            show_original_colours,
            original_colours_action: original_action,
            cid_cache,
            body_document_cache,
        },
    }
}

struct ReaderReloadState {
    web_view: WebView,
    raw_html: Rc<RefCell<String>>,
    allow_remote: Rc<Cell<bool>>,
    show_original_colours: Rc<Cell<bool>>,
    remote_banner: gtk::Box,
    body_document_cache: Rc<RefCell<Option<ReaderBodyDocumentCache>>>,
}

impl ReaderReloadState {
    fn downgrade(&self) -> WeakReaderReloadState {
        WeakReaderReloadState {
            web_view: self.web_view.downgrade(),
            raw_html: self.raw_html.clone(),
            allow_remote: self.allow_remote.clone(),
            show_original_colours: self.show_original_colours.clone(),
            remote_banner: self.remote_banner.downgrade(),
            body_document_cache: self.body_document_cache.clone(),
        }
    }
}

/// [`ReaderReloadState`] for callbacks owned by widgets inside the reader
/// (the remote-images banner): a strong WebView or banner there would form a
/// cycle that keeps a closed message window's reader alive.
struct WeakReaderReloadState {
    web_view: gtk::glib::WeakRef<WebView>,
    raw_html: Rc<RefCell<String>>,
    allow_remote: Rc<Cell<bool>>,
    show_original_colours: Rc<Cell<bool>>,
    remote_banner: gtk::glib::WeakRef<gtk::Box>,
    body_document_cache: Rc<RefCell<Option<ReaderBodyDocumentCache>>>,
}

impl WeakReaderReloadState {
    fn upgrade(&self) -> Option<ReaderReloadState> {
        Some(ReaderReloadState {
            web_view: self.web_view.upgrade()?,
            raw_html: self.raw_html.clone(),
            allow_remote: self.allow_remote.clone(),
            show_original_colours: self.show_original_colours.clone(),
            remote_banner: self.remote_banner.upgrade()?,
            body_document_cache: self.body_document_cache.clone(),
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ReaderBodyCacheKey {
    raw_hash: u64,
    raw_len: usize,
    allow_remote: bool,
    scheme: String,
    canvas: String,
    text: String,
    link: String,
    sanitizer_version: u32,
    density: ListDensity,
    font_size: i32,
}

struct ReaderBodyDocumentCache {
    key: ReaderBodyCacheKey,
    wrapped_document: String,
}

fn clear_reader_body_document_cache(widgets: &ReaderWidgets) {
    *widgets.body_document_cache.borrow_mut() = None;
}

fn reader_body_cache_key(raw: &str, allow_remote: bool, paint: &CanvasPaint) -> ReaderBodyCacheKey {
    use std::collections::hash_map::DefaultHasher;
    let mut hasher = DefaultHasher::new();
    raw.hash(&mut hasher);
    ReaderBodyCacheKey {
        raw_hash: hasher.finish(),
        raw_len: raw.len(),
        allow_remote,
        scheme: paint.scheme.to_string(),
        canvas: paint.canvas.clone(),
        text: paint.text.clone(),
        link: paint.link.clone(),
        sanitizer_version: SANITIZER_VERSION,
        density: list_density(),
        font_size: reader_font_size(),
    }
}

fn reader_reload_state(widgets: &ReaderWidgets) -> ReaderReloadState {
    ReaderReloadState {
        web_view: widgets.web_view.clone(),
        raw_html: widgets.raw_html.clone(),
        allow_remote: widgets.allow_remote.clone(),
        show_original_colours: widgets.show_original_colours.clone(),
        remote_banner: widgets.remote_banner.clone(),
        body_document_cache: widgets.body_document_cache.clone(),
    }
}

pub fn set_reader_actions_sensitive(widgets: &ReaderWidgets, sensitive: bool) {
    widgets.reply_btn.set_sensitive(sensitive);
    widgets.forward_btn.set_sensitive(sensitive);
    widgets.react_btn.set_sensitive(sensitive);
    widgets.archive_btn.set_sensitive(sensitive);
    widgets.star_btn.set_sensitive(sensitive);
    widgets.delete_btn.set_sensitive(sensitive);
}

pub fn set_react_enabled(widgets: &ReaderWidgets, enabled: bool, tooltip: &str) {
    widgets.react_btn.set_sensitive(enabled);
    widgets.react_btn.set_tooltip_text(Some(tooltip));
}

pub fn prepare_reaction_popover(widgets: &ReaderWidgets, sent_emojis: &[String]) {
    for (button, emoji) in &widgets.reaction_quick {
        let already = sent_emojis.iter().any(|sent| sent == emoji);
        button.set_sensitive(!already);
        if already {
            let tooltip = trf!("You already reacted with {emoji}", emoji = emoji);
            button.set_tooltip_text(Some(&tooltip));
        } else {
            button.set_tooltip_text(reaction_name(emoji));
        }
    }
}

pub fn popup_reaction_popover(widgets: &ReaderWidgets) {
    widgets.reaction_popover.popup();
}

pub fn show_reaction_chips(widgets: &ReaderWidgets, chips: &[alcedo_core::GroupedReaction]) {
    while let Some(child) = widgets.reactions.first_child() {
        widgets.reactions.remove(&child);
    }
    if chips.is_empty() {
        widgets.reactions.set_visible(false);
        return;
    }
    for chip in chips {
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .css_classes(["alcedo-reaction-chip"])
            .build();
        let label_text = if chip.count == 1 {
            chip.emoji.clone()
        } else {
            format!("{} {}", chip.emoji, chip.count)
        };
        let label = gtk::Label::new(Some(&label_text));
        row.append(&label);
        row.set_has_tooltip(true);
        row.set_tooltip_text(Some(&chip.names.join("\n")));
        widgets.reactions.append(&row);
    }
    widgets.reactions.set_visible(true);
}

pub fn sync_reader_star(widgets: &ReaderWidgets, flagged: bool) {
    widgets.star_syncing.set(true);
    widgets.star_btn.set_active(flagged);
    widgets.star_btn.set_icon_name(if flagged {
        "starred-symbolic"
    } else {
        "non-starred-symbolic"
    });
    let label = if flagged { tr!("Unstar") } else { tr!("Star") };
    let action = if flagged { ACTION_UNSTAR } else { ACTION_STAR };
    a11y::name_icon_control(
        &widgets.star_btn,
        &label,
        &commands::command_tooltip(action, &label),
    );
    widgets.star_syncing.set(false);
}

/// Drop Reply / Forward / Compose text on compact layouts. Accessible names stay.
///
/// `label` and `icon-name` are mutually exclusive on `AdwSplitButton`
/// (setting either one clears the other), so switching to icon-only mode
/// must set `icon-name` rather than just clearing the label, or the button
/// ends up with neither text nor icon.
pub fn set_reader_reply_icon_only(widgets: &ReaderWidgets, icon_only: bool) {
    widgets.compose_label.set_visible(!icon_only);
    if icon_only {
        widgets.reply_btn.set_icon_name(REPLY_ICON_NAME);
        widgets.forward_btn.set_icon_name(FORWARD_ICON_NAME);
    } else {
        widgets.reply_btn.set_label(&tr!("Reply"));
        widgets.forward_btn.set_label(&tr!("Forward"));
    }
}

/// Recompute shortcut hints after a keyboard-override change.
pub fn refresh_reader_shortcut_tooltips(widgets: &ReaderWidgets) {
    a11y::name_icon_control(
        &widgets.compose_btn,
        &tr!("New Message"),
        &commands::command_tooltip(ACTION_COMPOSE, &tr!("New Message")),
    );
    a11y::name_icon_control(
        &widgets.reply_btn,
        &tr!("Reply"),
        &commands::command_tooltip(ACTION_REPLY, &tr!("Reply")),
    );
    a11y::name_icon_control(
        &widgets.forward_btn,
        &tr!("Forward"),
        &commands::command_tooltip(ACTION_FORWARD, &tr!("Forward")),
    );
    a11y::name_icon_control(
        &widgets.archive_btn,
        &tr!("Archive"),
        &commands::command_tooltip(ACTION_ARCHIVE, &tr!("Archive")),
    );
    a11y::name_icon_control(
        &widgets.delete_btn,
        &tr!("Delete"),
        &commands::command_tooltip(ACTION_DELETE, &tr!("Delete")),
    );
    sync_reader_star(widgets, widgets.star_btn.is_active());
}

fn create_mail_context(cid_cache: CidCache) -> WebContext {
    let context = create_isolated_context();
    register_cid_scheme(&context, cid_cache);
    context
}

fn wire_navigation_policy(
    web_view: &WebView,
    meeting_banner: &MeetingBanner,
    on_mailto: impl Fn(String) + 'static,
) {
    let on_mailto = Rc::new(on_mailto);
    let banner_handles = meeting_banner.handles();
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

        if allow_internal_mail_navigation(&uri) {
            return false;
        }

        if let Some(address) = mailto_address(&uri) {
            decision.ignore();
            on_mailto(address);
            return true;
        }

        if is_calendar_invite_uri(&uri) && banner_handles.try_present_calendar_link(&uri, view) {
            decision.ignore();
            return true;
        }

        if uri.starts_with("http://") || uri.starts_with("https://") {
            decision.ignore();
            let parent = view.root().and_then(|w| w.downcast::<gtk::Window>().ok());
            let launcher = gtk::UriLauncher::new(&uri);
            launcher.launch(parent.as_ref(), None::<&gtk::gio::Cancellable>, |result| {
                if let Err(err) = result {
                    tracing::warn!(?err, "failed to launch external URI");
                }
            });
            return true;
        }

        decision.ignore();
        true
    });
}

fn wire_reader_link_tooltip(web_view: &WebView) {
    let hovered: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    web_view.set_has_tooltip(true);
    web_view.connect_query_tooltip({
        let hovered = hovered.clone();
        move |_view, _x, _y, keyboard_mode, tooltip| {
            if keyboard_mode {
                return false;
            }
            let Some(uri) = hovered.borrow().clone() else {
                return false;
            };
            let label = gtk::Label::builder()
                .label(&uri)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::Char)
                .max_width_chars(72)
                .xalign(0.0)
                .selectable(false)
                .build();
            tooltip.set_custom(Some(&label));
            true
        }
    });
    web_view.connect_mouse_target_changed({
        let hovered = hovered.clone();
        move |view, hit, _mods| {
            let next = reader_link_tooltip(hit.context_is_link(), hit.link_uri().as_deref());
            if *hovered.borrow() == next {
                return;
            }
            *hovered.borrow_mut() = next;
            view.trigger_tooltip_query();
        }
    });
    web_view.connect_load_changed({
        let hovered = hovered.clone();
        move |view, event| {
            if event != LoadEvent::Started {
                return;
            }
            if hovered.borrow().is_none() {
                return;
            }
            *hovered.borrow_mut() = None;
            view.trigger_tooltip_query();
        }
    });
}

fn allow_internal_mail_navigation(uri: &str) -> bool {
    uri == "about:blank" || uri.starts_with("about:blank#") || uri.starts_with("alcedo-cid:")
}

/// Full destination URI for a hovered reader link. `None` hides the tooltip.
/// `link_uri` is WebKit's resolved href, never the visible label or `title`.
pub(crate) fn reader_link_tooltip(context_is_link: bool, link_uri: Option<&str>) -> Option<String> {
    if !context_is_link {
        return None;
    }
    let uri = link_uri?.trim();
    if uri.is_empty() {
        None
    } else {
        Some(uri.to_string())
    }
}

fn mailto_address(uri: &str) -> Option<String> {
    let parsed = url::Url::parse(uri).ok()?;
    if parsed.scheme() != "mailto" {
        return None;
    }
    let address = parsed.path();
    if address.is_empty() {
        parsed
            .to_string()
            .strip_prefix("mailto:")
            .map(str::to_string)
    } else {
        Some(address.to_string())
    }
}

pub fn update_reader_headers(
    widgets: &ReaderWidgets,
    envelope: &Envelope,
    on_address_click: &Rc<dyn Fn(&gtk::Widget, Address)>,
) {
    widgets.subject.set_label(&envelope.display_subject());
    widgets
        .reply_list_action
        .set_enabled(envelope.routing_headers().list_post.is_some());
    widgets.from.populate(&envelope.from, on_address_click);
    widgets.date.set_label(&format_reader_date(&envelope.date));
    sync_reader_star(widgets, envelope.flagged);
    if show_distinct_address_row(&envelope.sender, &envelope.from) {
        widgets.sender.populate(&envelope.sender, on_address_click);
        widgets.sender.row.set_visible(true);
    } else {
        widgets.sender.reset();
        widgets.sender.row.set_visible(false);
    }
    if show_distinct_address_row(&envelope.reply_to, &envelope.from) {
        widgets
            .reply_to
            .populate(&envelope.reply_to, on_address_click);
        widgets.reply_to.row.set_visible(true);
    } else {
        widgets.reply_to.reset();
        widgets.reply_to.row.set_visible(false);
    }
    widgets.to.populate(&envelope.to, on_address_click);
    widgets.to.row.set_visible(true);
    if envelope.cc.is_empty() {
        widgets.cc.reset();
        widgets.cc.row.set_visible(false);
    } else {
        widgets.cc.populate(&envelope.cc, on_address_click);
        widgets.cc.row.set_visible(true);
    }
}

pub fn update_reader_pgp_banner(
    widgets: &ReaderWidgets,
    id: MessageId,
    status: Option<&PgpStatus>,
) {
    update_pgp_banner_for_message(&widgets.pgp_banner, id, status);
}

pub fn wire_reader_pgp_banner_accept(widgets: &ReaderWidgets, on_accept: impl FnOnce() + 'static) {
    crate::pgp_banner::wire_pgp_banner_accept(&widgets.pgp_banner, on_accept);
}

pub fn update_reader_autocrypt_banner(
    widgets: &ReaderWidgets,
    email: &str,
    on_accept: impl FnOnce() + 'static,
) {
    update_autocrypt_offer_banner(&widgets.pgp_banner, email, on_accept);
}

/// Remote images follow `allow_remote`. Inline `alcedo-cid:` images load even
/// when remote images stay blocked.
pub(crate) fn reader_should_load_images(allow_remote: bool, source: &str) -> bool {
    allow_remote || source.to_ascii_lowercase().contains("alcedo-cid:")
}

fn should_skip_reader_reload(
    current_source: &str,
    prepared_source: &str,
    showing_body: bool,
    current_allow_remote: bool,
    allow_remote: bool,
) -> bool {
    current_source == prepared_source && showing_body && current_allow_remote == allow_remote
}

pub fn update_reader(
    widgets: &ReaderWidgets,
    envelope: &Envelope,
    account_email: &str,
    html: Option<&str>,
    text: &str,
    attachments: &[BodyAttachment],
    dummy_attachment: bool,
    attachments_pending: bool,
    attachments_failed: bool,
    allow_remote: bool,
    on_address_click: &Rc<dyn Fn(&gtk::Widget, Address)>,
    sender: &Sender<crate::app::AppMsg>,
) {
    update_reader_headers(widgets, envelope, on_address_click);
    set_reader_actions_sensitive(widgets, true);
    update_otp_banner(
        &widgets.otp_banner,
        envelope.id,
        &envelope.display_subject(),
        text,
        html,
        crate::settings::show_verification_codes(),
    );
    update_meeting_banner(
        &widgets.meeting_banner,
        envelope.id,
        account_email,
        attachments,
        &widgets.cid_cache,
        sender,
    );
    update_attachment_chips(
        &widgets.attachments_box,
        attachments,
        dummy_attachment,
        attachments_pending,
        attachments_failed,
        &widgets.cid_cache,
        &widgets.web_view,
    );
    let prepared = prepare_body(html, text);
    let skip_reload = should_skip_reader_reload(
        widgets.raw_html.borrow().as_str(),
        &prepared.source,
        widgets.body_stack.visible_child_name().as_deref() == Some("body"),
        widgets.allow_remote.get(),
        allow_remote,
    );
    let load_images = reader_should_load_images(allow_remote, &prepared.source);
    let skip_reload = skip_reload && widgets.settings.is_auto_load_images() == load_images;
    widgets.allow_remote.set(allow_remote);
    // WebKit skips every image scheme except `data:` when this is off, including
    // `alcedo-cid:`. Inline images need it on. Remote images stay out of the
    // HTML until the user allows them.
    widgets.settings.set_auto_load_images(load_images);
    let show_remote = !allow_remote && prepared.is_html && html_has_remote_images(&prepared.source);
    widgets.remote_banner.set_visible(show_remote);
    if *widgets.raw_html.borrow() != prepared.source {
        widgets.show_original_colours.set(false);
        widgets
            .original_colours_action
            .set_state(&false.to_variant());
    }
    *widgets.raw_html.borrow_mut() = prepared.source;

    widgets.content_stack.set_visible_child_name("content");
    show_reader_body_document(widgets);
    if !skip_reload {
        reload_reader_body(&reader_reload_state(widgets));
    }
}

pub fn reload_reader_remote_images(widgets: &ReaderWidgets) {
    widgets.allow_remote.set(true);
    widgets.settings.set_auto_load_images(true);
    reload_reader_body(&reader_reload_state(widgets));
}

pub fn reload_reader_document(widgets: &ReaderWidgets) {
    reload_reader_body(&reader_reload_state(widgets));
}

/// Restyle the mail WebView to match the current GTK color scheme.
///
/// WebKit does not inherit Adwaita colors, and `prefers-color-scheme` follows
/// the OS rather than `AdwStyleManager`, so the wrapper CSS must be rebuilt.
/// Inline author colors are remapped for dark mode before the document is loaded.
pub fn apply_theme(widgets: &ReaderWidgets, _dark: bool) {
    reload_reader_document(widgets);
}

pub fn clear_reader(widgets: &ReaderWidgets) {
    clear_reader_body_document_cache(widgets);
    show_reader_placeholder(widgets, ReaderPlaceholder::Empty);
}

pub fn update_unsubscribe_button(
    widgets: &ReaderWidgets,
    action: Option<&ListUnsubscribe>,
    done: bool,
    pending: bool,
) {
    let visible = action.is_some_and(ListUnsubscribe::has_action);
    widgets.unsubscribe_btn.set_visible(visible);
    if !visible {
        widgets.unsubscribe_btn.set_label(&tr!("Unsubscribe"));
        widgets.unsubscribe_btn.set_sensitive(true);
        return;
    }
    if done {
        widgets.unsubscribe_btn.set_label(&tr!("Unsubscribed"));
        widgets.unsubscribe_btn.set_sensitive(false);
    } else if pending {
        widgets.unsubscribe_btn.set_label(&tr!("Unsubscribe"));
        widgets.unsubscribe_btn.set_sensitive(false);
    } else {
        widgets.unsubscribe_btn.set_label(&tr!("Unsubscribe"));
        widgets.unsubscribe_btn.set_sensitive(true);
    }
}

fn reload_reader_body(state: &ReaderReloadState) {
    let raw = state.raw_html.borrow().clone();
    let original = state.show_original_colours.get();
    let paint = canvas_paint(&state.web_view, original);
    apply_web_view_background(&state.web_view, &paint.rgba);
    if raw.is_empty() {
        *state.body_document_cache.borrow_mut() = None;
        state
            .web_view
            .load_html(&wrap_mail_document("", &paint), Some("about:blank"));
        state.remote_banner.set_visible(false);
        return;
    }

    let allow_remote = state.allow_remote.get();
    let cache_key = reader_body_cache_key(&raw, allow_remote, &paint);
    let wrapped_document = {
        let mut slot = state.body_document_cache.borrow_mut();
        if let Some(cached) = slot.as_ref() {
            if cached.key == cache_key {
                cached.wrapped_document.clone()
            } else {
                let document = build_reader_wrapped_document(&raw, allow_remote, &paint);
                *slot = Some(ReaderBodyDocumentCache {
                    key: cache_key,
                    wrapped_document: document.clone(),
                });
                document
            }
        } else {
            let document = build_reader_wrapped_document(&raw, allow_remote, &paint);
            *slot = Some(ReaderBodyDocumentCache {
                key: cache_key,
                wrapped_document: document.clone(),
            });
            document
        }
    };
    state
        .web_view
        .load_html(&wrapped_document, Some("about:blank"));

    let is_html = looks_like_html(&raw);
    let show_remote = !allow_remote && is_html && html_has_remote_images(&raw);
    state.remote_banner.set_visible(show_remote);
}

fn build_reader_wrapped_document(raw: &str, allow_remote: bool, paint: &CanvasPaint) -> String {
    let is_html = looks_like_html(raw);
    let body = if is_html {
        let sanitized = autolink_html_fragment(&sanitize_html(
            raw,
            SanitizeOptions {
                allow_remote_images: allow_remote,
            },
        ));
        if paint.scheme == "dark" {
            adapt_html_for_dark_mode(&sanitized, &paint.canvas, &paint.text, &paint.link)
        } else {
            sanitized
        }
    } else {
        format!(
            r#"<div class="alcedo-plain">{}</div>"#,
            plaintext_to_fragment(raw)
        )
    };
    wrap_mail_document(&body, paint)
}

pub(crate) struct PreparedBody {
    pub(crate) source: String,
    pub(crate) is_html: bool,
}

/// Choose the best display source: HTML part, HTML-looking text, else plaintext.
///
/// Prefer the text part when it is real HTML and the "HTML" part is mail-parser's
/// `text_to_html` conversion (`<html><body>…<br/>…` or escaped `&lt;br&gt;`).
pub(crate) fn prepare_body(html: Option<&str>, text: &str) -> PreparedBody {
    let html = html.map(str::trim).filter(|value| !value.is_empty());
    let text = text.trim();
    let text_is_html = looks_like_html(text);
    let html_is_generated = html.is_some_and(looks_like_generated_plaintext_html);

    if text_is_html && (html.is_none() || html_is_generated) {
        return PreparedBody {
            source: text.to_string(),
            is_html: true,
        };
    }

    if let Some(html) = html
        && !html_is_generated
    {
        return PreparedBody {
            source: html.to_string(),
            is_html: true,
        };
    }

    if text.is_empty() {
        return PreparedBody {
            source: String::new(),
            is_html: false,
        };
    }

    PreparedBody {
        source: text.to_string(),
        is_html: text_is_html,
    }
}

fn attach_subject_copy_context_menu(subject: &gtk::Label, toast_host: &gtk::Widget) {
    let menu = gtk::gio::Menu::new();
    menu.append(Some(&tr!("Copy Subject")), Some("readersubject.copy"));

    let actions = gtk::gio::SimpleActionGroup::new();
    let copy_action = gtk::gio::SimpleAction::new("copy", None);
    // Weak: the subject owns this action, and `toast_host` is the reader box
    // holding the WebView. Strong captures would keep a closed message
    // window's whole reader alive.
    let subject_for_copy = subject.downgrade();
    let toast_host = toast_host.downgrade();
    copy_action.connect_activate(move |_, _| {
        let (Some(subject), Some(toast_host)) = (subject_for_copy.upgrade(), toast_host.upgrade())
        else {
            return;
        };
        copy_text_to_clipboard(&subject.label());
        show_composer_toast(&toast_host, &tr!("Subject copied"));
    });
    actions.add_action(&copy_action);
    subject.insert_action_group("readersubject", Some(&actions));

    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_has_arrow(false);
    popover.set_parent(subject);
    unparent_popover_on_destroy(subject, &popover);

    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    gesture.connect_pressed(move |gesture, _, x, y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        popover.set_pointing_to(Some(&rect));
        popover.popup();
    });
    subject.add_controller(gesture);
}

fn format_reader_date(date: &DateTime<Utc>) -> String {
    crate::local_time::format_date_time(
        date.with_timezone(&Local).naive_local(),
        // TRANSLATORS: strftime pattern for a message date like "Sep 07, 2026, 09:05 AM".
        &trc!(
            "strftime pattern: short month, day, year, time",
            "%b %d, %Y, %I:%M %p"
        ),
    )
}

fn recipient_expand_icon(expanded: bool) -> &'static str {
    if expanded {
        "pan-up-symbolic"
    } else {
        "pan-down-symbolic"
    }
}

fn recipient_expand_tooltip(expanded: bool, count: usize) -> String {
    if expanded {
        tr!("Show fewer recipients")
    } else if count == 0 {
        tr!("Show all recipients")
    } else {
        ntr!("Show {n} recipient", "Show all {n} recipients", count)
    }
}

fn recipient_expand_visible(has_text: bool, expanded: bool, overflowing: bool) -> bool {
    has_text && (expanded || overflowing)
}

fn recipient_line_overflows(content_height: i32) -> bool {
    content_height > COLLAPSED_RECIPIENT_HEIGHT
}

fn recipient_content_height(chips: &adw::WrapBox) -> i32 {
    let width = chips.width();
    if width <= 0 {
        return 0;
    }
    let (_, nat, _, _) = chips.measure(gtk::Orientation::Vertical, width);
    nat
}

/// Break the child's natural-width size request so a long chip row cannot
/// floor the window / paned minimum width. Horizontal overflow is clipped.
/// Vertical size follows the child unless `max-content-height` is set
/// (`Never` would grow to fit every wrapped line).
fn clip_h_natural(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .propagate_natural_width(false)
        .propagate_natural_height(true)
        .min_content_width(1)
        .hexpand(true)
        .halign(gtk::Align::Fill)
        .kinetic_scrolling(false)
        .css_classes(["alcedo-reader-addr-clip"])
        .child(child)
        .build()
}

impl AddressField {
    fn reset(&self) {
        clear_address_chips(&self.chips);
        self.expanded.set(false);
        apply_recipient_chrome(&self.chips, &self.clip, &self.expand, &self.expanded);
    }

    fn populate(&self, addrs: &[Address], on_address_click: &Rc<dyn Fn(&gtk::Widget, Address)>) {
        populate_address_chips(&self.chips, addrs, on_address_click);
        self.expanded.set(false);
        apply_recipient_chrome(&self.chips, &self.clip, &self.expand, &self.expanded);
    }
}

fn build_recipient_row(initially_visible: bool, prefix_label: &str) -> AddressField {
    let expanded = Rc::new(Cell::new(false));

    let chips = build_address_chips_box();
    let clip = clip_h_natural(&chips);
    let prefix = gtk::Label::builder()
        .label(prefix_label)
        .xalign(0.0)
        .valign(gtk::Align::Start)
        .css_classes(["dim-label", "alcedo-reader-meta"])
        .build();

    let expand_btn = gtk::Button::from_icon_name(recipient_expand_icon(false));
    expand_btn.add_css_class("flat");
    expand_btn.add_css_class("alcedo-reader-recipients-expand");
    expand_btn.set_tooltip_text(Some(&recipient_expand_tooltip(false, 0)));
    expand_btn.set_has_frame(false);
    expand_btn.set_focus_on_click(false);
    expand_btn.set_valign(gtk::Align::Start);
    expand_btn.set_visible(false);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.add_css_class("alcedo-reader-recipients");
    row.set_hexpand(true);
    row.set_halign(gtk::Align::Fill);
    row.set_visible(initially_visible);
    row.append(&prefix);
    row.append(&clip);
    row.append(&expand_btn);

    wire_recipient_row(&chips, &clip, &expand_btn, expanded.clone());
    apply_recipient_chrome(&chips, &clip, &expand_btn, &expanded);

    AddressField {
        row,
        chips,
        clip,
        expand: expand_btn,
        expanded,
    }
}

fn build_address_chips_box() -> adw::WrapBox {
    adw::WrapBox::builder()
        .child_spacing(4)
        .line_spacing(1)
        .line_homogeneous(false)
        .justify(adw::JustifyMode::None)
        // Natural: wrap at each chip's preferred width. Minimum shrinks
        // ellipsized labels to one character so a long Cc row never wraps.
        .wrap_policy(adw::WrapPolicy::Natural)
        .hexpand(true)
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Start)
        .css_classes(["alcedo-reader-addr-chips", "collapsed"])
        .build()
}

fn clear_address_chips(box_: &adw::WrapBox) {
    while let Some(child) = box_.first_child() {
        box_.remove(&child);
    }
}

fn populate_address_chips(
    box_: &adw::WrapBox,
    addrs: &[Address],
    on_address_click: &Rc<dyn Fn(&gtk::Widget, Address)>,
) {
    clear_address_chips(box_);
    for addr in addrs {
        let name = gtk::Label::builder()
            .label(addr.display_name())
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(28)
            .build();
        let chip = gtk::Button::builder()
            .child(&name)
            .css_classes(["flat", "alcedo-reader-addr-chip"])
            .build();
        if let Some(tooltip) = addr.chip_tooltip() {
            chip.set_tooltip_text(Some(&tooltip));
        }
        let addr_for_click = addr.clone();
        let addr_for_menu = addr.clone();
        let on_address_click = on_address_click.clone();
        chip.connect_clicked(move |btn| on_address_click(btn.upcast_ref(), addr_for_click.clone()));
        attach_address_copy_context_menu(&chip, addr_for_menu, chip.upcast_ref());
        box_.append(&chip);
    }
}

fn wire_recipient_row(
    chips: &adw::WrapBox,
    clip: &gtk::ScrolledWindow,
    expand_btn: &gtk::Button,
    expanded: Rc<Cell<bool>>,
) {
    // These handlers live on `expand_btn` and `chips`, so they hold the row's
    // widgets weakly; strong captures would keep the reader alive after close.
    let chips_for_btn = chips.downgrade();
    let clip_for_btn = clip.downgrade();
    let expanded_for_btn = expanded.clone();
    expand_btn.connect_clicked(move |expand_btn| {
        let (Some(chips), Some(clip)) = (chips_for_btn.upgrade(), clip_for_btn.upgrade()) else {
            return;
        };
        expanded_for_btn.set(!expanded_for_btn.get());
        apply_recipient_chrome(&chips, &clip, expand_btn, &expanded_for_btn);
    });

    let refresh_overflow = Rc::new({
        let chips = chips.downgrade();
        let expand_btn = expand_btn.downgrade();
        let expanded = expanded.clone();
        move || {
            let (Some(chips), Some(expand_btn)) = (chips.upgrade(), expand_btn.upgrade()) else {
                return;
            };
            refresh_recipient_expand_visibility(&chips, &expand_btn, expanded.get());
        }
    });
    chips.connect_notify_local(Some("width"), {
        let refresh_overflow = refresh_overflow.clone();
        move |_, _| refresh_overflow()
    });
    chips.connect_notify_local(Some("height"), {
        let refresh_overflow = refresh_overflow.clone();
        move |_, _| refresh_overflow()
    });
}

fn apply_recipient_chrome(
    chips: &adw::WrapBox,
    clip: &gtk::ScrolledWindow,
    expand_btn: &gtk::Button,
    expanded: &Rc<Cell<bool>>,
) {
    let is_expanded = expanded.get();
    if is_expanded {
        chips.remove_css_class("collapsed");
        clip.set_max_content_height(-1);
    } else {
        chips.add_css_class("collapsed");
        clip.set_max_content_height(COLLAPSED_RECIPIENT_HEIGHT);
    }
    clip.set_propagate_natural_height(true);
    expand_btn.set_icon_name(recipient_expand_icon(is_expanded));
    let count = address_chip_count(chips);
    expand_btn.set_tooltip_text(Some(&recipient_expand_tooltip(is_expanded, count)));
    refresh_recipient_expand_visibility(chips, expand_btn, is_expanded);
}

fn refresh_recipient_expand_visibility(
    chips: &adw::WrapBox,
    expand_btn: &gtk::Button,
    expanded: bool,
) {
    let has_text = chips.first_child().is_some();
    let overflowing = recipient_line_overflows(recipient_content_height(chips));
    expand_btn.set_visible(recipient_expand_visible(has_text, expanded, overflowing));
}

fn address_chip_count(chips: &adw::WrapBox) -> usize {
    let mut count = 0;
    let mut child = chips.first_child();
    while let Some(widget) = child {
        count += 1;
        child = widget.next_sibling();
    }
    count
}

fn attachment_icon_name(mime: &str, filename: &str) -> &'static str {
    let mime = mime.to_ascii_lowercase();
    let ext = extension_of(filename);
    let ext = ext.as_deref();

    if mime.starts_with("image/") {
        return "image-x-generic-symbolic";
    }
    if mime.starts_with("audio/") {
        return "audio-x-generic-symbolic";
    }
    if mime.starts_with("video/") {
        return "video-x-generic-symbolic";
    }
    if mime == "application/pdf" || ext == Some("pdf") {
        return "x-office-document-symbolic";
    }
    if mime.contains("spreadsheet")
        || mime.contains("excel")
        || matches!(ext, Some("xls") | Some("xlsx") | Some("ods") | Some("csv"))
    {
        return "x-office-spreadsheet-symbolic";
    }
    if mime.contains("presentation")
        || mime.contains("powerpoint")
        || matches!(ext, Some("ppt") | Some("pptx") | Some("odp"))
    {
        return "x-office-presentation-symbolic";
    }
    if mime.contains("word")
        || mime.contains("wordprocessingml")
        || mime == "application/rtf"
        || matches!(ext, Some("doc") | Some("docx") | Some("odt") | Some("rtf"))
    {
        return "x-office-document-symbolic";
    }
    if mime == "text/calendar"
        || mime == "application/ics"
        || mime == "application/x-ics"
        || ext == Some("ics")
    {
        return "x-office-calendar-symbolic";
    }
    if mime == "text/vcard" || ext == Some("vcf") {
        return "x-office-address-book-symbolic";
    }
    if mime.contains("zip")
        || mime.contains("compressed")
        || mime.contains("tar")
        || mime.contains("gzip")
        || matches!(
            ext,
            Some("zip") | Some("rar") | Some("7z") | Some("tar") | Some("gz")
        )
    {
        return "package-x-generic-symbolic";
    }
    if mime.starts_with("text/") || mime == "application/sql" || ext == Some("sql") {
        return "text-x-generic-symbolic";
    }
    "mail-attachment-symbolic"
}

fn build_attachment_card(
    icon_name: &str,
    filename: &str,
    size_text: &str,
    sensitive: bool,
    risky: bool,
) -> gtk::Button {
    let icon = gtk::Image::from_icon_name(if risky {
        "dialog-warning-symbolic"
    } else {
        icon_name
    });
    icon.add_css_class("alcedo-attachment-card-icon");
    icon.set_pixel_size(16);
    icon.set_valign(gtk::Align::Center);

    let name_label = gtk::Label::builder()
        .label(filename)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(40)
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .css_classes(["alcedo-attachment-card-name"])
        .build();

    let size_label = gtk::Label::builder()
        .label(size_text)
        .xalign(0.0)
        .halign(gtk::Align::Start)
        .css_classes(["alcedo-attachment-card-size"])
        .build();

    let labels = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .build();
    labels.append(&name_label);
    labels.append(&size_label);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .build();
    content.append(&icon);
    content.append(&labels);

    let card = gtk::Button::builder()
        .child(&content)
        .css_classes(["flat", "alcedo-attachment-card"])
        .tooltip_text(format!("{filename} ({size_text})"))
        .sensitive(sensitive)
        .focus_on_click(false)
        .build();
    if risky {
        card.add_css_class("alcedo-attachment-card-risky");
    }
    card
}

fn clear_attachment_chips(box_: &adw::WrapBox) {
    while let Some(child) = box_.first_child() {
        box_.remove(&child);
    }
    box_.set_visible(false);
}

fn save_all_visible(attachment_count: usize) -> bool {
    attachment_count >= 2
}

fn update_attachment_chips(
    box_: &adw::WrapBox,
    attachments: &[BodyAttachment],
    show_dummy: bool,
    attachments_pending: bool,
    attachments_failed: bool,
    cid_cache: &CidCache,
    web_view: &WebView,
) {
    clear_attachment_chips(box_);
    if attachments.is_empty() && !show_dummy && !attachments_pending && !attachments_failed {
        return;
    }

    box_.set_visible(true);
    if attachments.is_empty() {
        if attachments_pending {
            let card = build_attachment_card(
                "mail-attachment-symbolic",
                &tr!("Attachment"),
                "…",
                false,
                false,
            );
            card.set_tooltip_text(Some(&tr!("Attachments are still loading")));
            box_.append(&card);
            return;
        }
        if attachments_failed {
            let card = build_attachment_card(
                "mail-attachment-symbolic",
                &tr!("Attachment"),
                "—",
                false,
                false,
            );
            card.set_tooltip_text(Some(&tr!("Couldn't download the attachment")));
            box_.append(&card);
            return;
        }
        let icon_name = attachment_icon_name("application/pdf", "attachment.pdf");
        let card = build_attachment_card(icon_name, "attachment.pdf", "—", false, false);
        box_.append(&card);
        return;
    }

    let lists = crate::settings::attachment_lists();
    let show_save_all = save_all_visible(attachments.len());
    let on_save_all = show_save_all.then(|| {
        let cid_cache = cid_cache.clone();
        let attachments = attachments.to_vec();
        let parent = box_.clone();
        Rc::new(move || {
            save_all_attachments_from_cache(&cid_cache, &attachments, &parent);
        }) as Rc<dyn Fn()>
    });
    for att in attachments {
        let icon_name = attachment_icon_name(&att.mime, &att.filename);
        let size_text = crate::compose_attachments::format_part_size(att.size);
        let bytes_ready = cid_cache.borrow().contains_key(&att.sha256);
        let risky = lists.attachment_is_risky(&att.filename, &att.mime, None)
            && !risky_attachment_is_enabled(&att.sha256);
        let card = build_attachment_card(icon_name, &att.filename, &size_text, bytes_ready, risky);
        if bytes_ready {
            let is_calendar = is_calendar_part(&att.mime, &att.filename);
            if risky {
                card.set_tooltip_text(Some(&format!(
                    "{} ({size_text})\nMay be unsafe — click to enable, then open or save",
                    att.filename
                )));
            } else if is_calendar {
                card.set_tooltip_text(Some(&format!(
                    "{} ({size_text})\nClick to add to a calendar, right-click for more",
                    att.filename
                )));
            } else {
                card.set_tooltip_text(Some(&format!(
                    "{} ({size_text})\nClick to open, right-click to save",
                    att.filename
                )));
            }
            if is_calendar {
                wire_calendar_attachment_card(&card, cid_cache, att, web_view, on_save_all.clone());
            } else {
                wire_attachment_card(
                    &card,
                    cid_cache,
                    att.sha256.clone(),
                    att.filename.clone(),
                    web_view,
                    on_save_all.clone(),
                );
            }
        }
        box_.append(&card);
    }

    if let Some(on_save_all) = on_save_all {
        let all_ready = attachments
            .iter()
            .all(|att| cid_cache.borrow().contains_key(&att.sha256));
        let save_all = build_save_all_button(all_ready, attachments.len());
        let on_save_all = on_save_all.clone();
        save_all.connect_clicked(move |_| on_save_all());
        box_.append(&save_all);
    }
}

fn build_save_all_button(sensitive: bool, count: usize) -> gtk::Button {
    let icon = gtk::Image::from_icon_name("document-save-symbolic");
    icon.set_pixel_size(16);
    icon.set_valign(gtk::Align::Center);

    let label = gtk::Label::builder()
        .label(tr!("Save all"))
        .css_classes(["alcedo-attachment-save-all-label"])
        .build();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .build();
    content.append(&icon);
    content.append(&label);

    let tooltip = if sensitive {
        ntr!(
            "Save {n} attachment to a folder",
            "Save all {n} attachments to a folder",
            count
        )
    } else {
        tr!("Attachments are still loading")
    };
    gtk::Button::builder()
        .child(&content)
        .css_classes(["flat", "alcedo-attachment-save-all"])
        .tooltip_text(&tooltip)
        .sensitive(sensitive)
        .focus_on_click(false)
        .build()
}

fn wire_calendar_attachment_card(
    card: &gtk::Button,
    cid_cache: &CidCache,
    att: &BodyAttachment,
    web_view: &WebView,
    on_save_all: Option<Rc<dyn Fn()>>,
) {
    let sha256 = att.sha256.clone();
    let filename = att.filename.clone();
    {
        let cid_cache = cid_cache.clone();
        let sha256 = sha256.clone();
        let filename_for_click = filename.clone();
        let web_view = web_view.clone();
        card.connect_clicked(move |card| {
            let Some((bytes, mime)) = cid_cache.borrow().get(&sha256).cloned() else {
                return;
            };
            let cid_cache = cid_cache.clone();
            let sha256 = sha256.clone();
            let filename_for_click = filename_for_click.clone();
            let card_for_click = card.clone();
            let web_view = web_view.clone();
            confirm_risky_attachment_then(
                parent_window(&web_view),
                filename_for_click.clone(),
                &mime,
                sha256.clone(),
                move || match parse_ics(&bytes) {
                    Ok(drafts) if !drafts.is_empty() => {
                        let invite = CalendarInvite {
                            drafts,
                            ics: String::from_utf8_lossy(&bytes).into_owned(),
                            sha256: sha256.clone(),
                            filename: filename_for_click.clone(),
                        };
                        offer_add_to_calendar(&card_for_click, &invite, Some(&cid_cache));
                    }
                    _ => open_attachment_file(
                        &cid_cache,
                        &sha256,
                        &filename_for_click,
                        parent_window(&web_view),
                    ),
                },
            );
        });
    }

    let menu = gtk::gio::Menu::new();
    menu.append(Some(&tr!("Open")), Some("attachment.open"));
    menu.append(Some(&tr!("Save to File…")), Some("attachment.save"));
    if on_save_all.is_some() {
        menu.append(Some(&tr!("Save All…")), Some("attachment.save-all"));
    }

    let actions = gtk::gio::SimpleActionGroup::new();
    let open_action = gtk::gio::SimpleAction::new("open", None);
    {
        let cid_cache = cid_cache.clone();
        let sha256 = sha256.clone();
        let filename = filename.clone();
        let web_view = web_view.clone();
        open_action.connect_activate(move |_, _| {
            open_attachment_file(&cid_cache, &sha256, &filename, parent_window(&web_view));
        });
    }
    actions.add_action(&open_action);

    let save_action = gtk::gio::SimpleAction::new("save", None);
    {
        let cid_cache = cid_cache.clone();
        let sha256 = sha256.clone();
        let filename = filename.clone();
        // Weak: the card owns this action through its action group.
        let card = card.downgrade();
        save_action.connect_activate(move |_, _| {
            let Some(card) = card.upgrade() else {
                return;
            };
            save_attachment_from_cache(&cid_cache, &sha256, &filename, &card);
        });
    }
    actions.add_action(&save_action);
    if let Some(on_save_all) = on_save_all {
        let save_all_action = gtk::gio::SimpleAction::new("save-all", None);
        save_all_action.connect_activate(move |_, _| on_save_all());
        actions.add_action(&save_all_action);
    }
    card.insert_action_group("attachment", Some(&actions));

    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_has_arrow(false);
    popover.set_parent(card);
    unparent_popover_on_destroy(card, &popover);

    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    gesture.connect_pressed(move |gesture, _, x, y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        popover.set_pointing_to(Some(&rect));
        popover.popup();
    });
    card.add_controller(gesture);
}

fn wire_attachment_card(
    card: &gtk::Button,
    cid_cache: &CidCache,
    sha256: String,
    filename: String,
    web_view: &WebView,
    on_save_all: Option<Rc<dyn Fn()>>,
) {
    {
        let cid_cache = cid_cache.clone();
        let sha256 = sha256.clone();
        let filename = filename.clone();
        let web_view = web_view.clone();
        card.connect_clicked(move |_| {
            open_attachment_file(&cid_cache, &sha256, &filename, parent_window(&web_view));
        });
    }

    let menu = gtk::gio::Menu::new();
    menu.append(Some(&tr!("Open")), Some("attachment.open"));
    menu.append(Some(&tr!("Save to File…")), Some("attachment.save"));
    if on_save_all.is_some() {
        menu.append(Some(&tr!("Save All…")), Some("attachment.save-all"));
    }

    let actions = gtk::gio::SimpleActionGroup::new();
    let open_action = gtk::gio::SimpleAction::new("open", None);
    {
        let cid_cache = cid_cache.clone();
        let sha256 = sha256.clone();
        let filename = filename.clone();
        let web_view = web_view.clone();
        open_action.connect_activate(move |_, _| {
            open_attachment_file(&cid_cache, &sha256, &filename, parent_window(&web_view));
        });
    }
    actions.add_action(&open_action);

    let save_action = gtk::gio::SimpleAction::new("save", None);
    {
        let cid_cache = cid_cache.clone();
        let sha256 = sha256.clone();
        let filename = filename.clone();
        // Weak: the card owns this action through its action group.
        let card = card.downgrade();
        save_action.connect_activate(move |_, _| {
            let Some(card) = card.upgrade() else {
                return;
            };
            save_attachment_from_cache(&cid_cache, &sha256, &filename, &card);
        });
    }
    actions.add_action(&save_action);
    if let Some(on_save_all) = on_save_all {
        let save_all_action = gtk::gio::SimpleAction::new("save-all", None);
        save_all_action.connect_activate(move |_, _| on_save_all());
        actions.add_action(&save_all_action);
    }
    card.insert_action_group("attachment", Some(&actions));

    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_has_arrow(false);
    popover.set_parent(card);
    unparent_popover_on_destroy(card, &popover);

    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    gesture.connect_pressed(move |gesture, _, x, y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        popover.set_pointing_to(Some(&rect));
        popover.popup();
    });
    card.add_controller(gesture);
}

fn parent_window(web_view: &WebView) -> Option<gtk::Window> {
    web_view
        .root()
        .and_then(|widget| widget.downcast::<gtk::Window>().ok())
}

fn risky_attachment_is_enabled(sha256: &str) -> bool {
    ENABLED_RISKY_ATTACHMENTS.with(|set| set.borrow().contains(sha256))
}

fn enable_risky_attachment(sha256: &str) {
    ENABLED_RISKY_ATTACHMENTS.with(|set| {
        set.borrow_mut().insert(sha256.to_string());
    });
}

fn confirm_risky_attachment_then(
    parent: Option<gtk::Window>,
    filename: String,
    mime: &str,
    sha256: String,
    then: impl FnOnce() + 'static,
) {
    confirm_risky_attachments_then(parent, vec![(filename, mime.to_string(), sha256)], then);
}

fn confirm_risky_attachments_then(
    parent: Option<gtk::Window>,
    items: Vec<(String, String, String)>,
    then: impl FnOnce() + 'static,
) {
    let lists = crate::settings::attachment_lists();
    let risky: Vec<(String, String)> = items
        .into_iter()
        .filter(|(filename, mime, sha256)| {
            lists.attachment_is_risky(filename, mime, None) && !risky_attachment_is_enabled(sha256)
        })
        .map(|(filename, _mime, sha256)| (filename, sha256))
        .collect();
    if risky.is_empty() {
        then();
        return;
    }

    let (heading, body) = if risky.len() == 1 {
        (
            tr!("This attachment may be unsafe"),
            trf!(
                "{name} can run code or harm this computer. Enable it only if you trust the sender.",
                name = risky[0].0
            ),
        )
    } else {
        let names = risky
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        (
            tr!("Some attachments may be unsafe"),
            trf!(
                "These files can run code or harm this computer. Enable them only if you trust the sender.\n\n{names}",
                names = names
            ),
        )
    };
    let dialog = adw::AlertDialog::builder()
        .heading(&heading)
        .body(&body)
        .build();
    dialog.add_response("cancel", &tr!("Cancel"));
    dialog.add_response("enable", &tr!("Enable"));
    dialog.set_response_appearance("enable", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.choose(
        parent.as_ref(),
        None::<&gtk::gio::Cancellable>,
        move |response| {
            if response.as_str() == "enable" {
                for (_, sha256) in &risky {
                    enable_risky_attachment(sha256);
                }
                then();
            }
        },
    );
}

/// Verified bytes for `requested` from the reader cache (shared by Save,
/// Save all and Open so they apply the same integrity rules).
fn cached_save_items(
    cid_cache: &CidCache,
    requested: &[(String, String)],
) -> Result<Vec<SaveItem>, SaveError> {
    let cache = cid_cache.borrow();
    prepare_save_items(requested, |sha256| cache.get(sha256).cloned())
}

fn save_attachment_from_cache(
    cid_cache: &CidCache,
    sha256: &str,
    filename: &str,
    parent: &impl IsA<gtk::Widget>,
) {
    let requested = [(sha256.to_string(), filename.to_string())];
    let item = match cached_save_items(cid_cache, &requested) {
        Ok(items) => items.into_iter().next(),
        Err(err) => {
            show_composer_toast(parent.as_ref(), &err.user_message());
            return;
        }
    };
    let Some(item) = item else {
        return;
    };
    let parent_widget = parent.as_ref().clone();
    let parent_window = parent_widget
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    let (filename, mime, sha256) = (
        item.filename.clone(),
        item.mime.clone(),
        item.sha256.clone(),
    );
    confirm_risky_attachment_then(parent_window, filename, &mime, sha256, move || {
        save_attachment_to_file(&parent_widget, item);
    });
}

fn save_all_attachments_from_cache(
    cid_cache: &CidCache,
    attachments: &[BodyAttachment],
    parent: &impl IsA<gtk::Widget>,
) {
    let requested: Vec<(String, String)> = attachments
        .iter()
        .map(|att| (att.sha256.clone(), att.filename.clone()))
        .collect();
    let items = match cached_save_items(cid_cache, &requested) {
        Ok(items) => items,
        Err(err) => {
            show_composer_toast(parent.as_ref(), &err.user_message());
            return;
        }
    };

    let parent_widget = parent.as_ref().clone();
    let parent_window = parent_widget
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    let confirm_items = items
        .iter()
        .map(|item| {
            (
                item.filename.clone(),
                item.mime.clone(),
                item.sha256.clone(),
            )
        })
        .collect();
    confirm_risky_attachments_then(parent_window, confirm_items, move || {
        save_attachments_to_folder(&parent_widget, items);
    });
}

fn save_attachments_to_folder(parent: &impl IsA<gtk::Widget>, items: Vec<SaveItem>) {
    let dialog = gtk::FileDialog::new();
    dialog.set_title(&tr!("Save all attachments"));
    let parent_window = parent
        .as_ref()
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    let parent_widget = parent.as_ref().clone();
    ignore_focus_selection_briefly();
    dialog.select_folder(
        parent_window.as_ref(),
        None::<&gtk::gio::Cancellable>,
        move |result| match result {
            Ok(folder) => {
                let Some(dir) = folder.path() else {
                    return;
                };
                // Attachments can be tens of MiB each; write them off the GTK thread.
                gtk::glib::spawn_future_local(async move {
                    let written =
                        crate::blocking_io::run(move || write_items_to_dir(&dir, &items)).await;
                    let message = written.map_or_else(
                        || tr!("Could not save attachments"),
                        |summary| summary.user_message(),
                    );
                    show_composer_toast(&parent_widget, &message);
                });
            }
            Err(err) if is_user_cancelled_dialog(&err) => {}
            Err(err) => {
                tracing::warn!(?err, "save-all folder dialog failed");
            }
        },
    );
}

fn save_attachment_to_file(parent: &impl IsA<gtk::Widget>, item: SaveItem) {
    let dialog = gtk::FileDialog::new();
    dialog.set_title(&tr!("Save attachment"));
    let suggested = suggested_attachment_filename(&item.filename);
    dialog.set_initial_name(Some(&suggested));
    let parent_window = parent
        .as_ref()
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    let parent_widget = parent.as_ref().clone();
    ignore_focus_selection_briefly();
    dialog.save(
        parent_window.as_ref(),
        None::<&gtk::gio::Cancellable>,
        move |result| match result {
            Ok(file) => {
                let Some(path) = file.path() else {
                    return;
                };
                gtk::glib::spawn_future_local(async move {
                    let written =
                        crate::blocking_io::run(move || write_file_atomic(&path, &item.bytes))
                            .await;
                    match written {
                        Some(Ok(())) => {
                            show_composer_toast(&parent_widget, &tr!("Attachment saved"))
                        }
                        Some(Err(err)) => {
                            tracing::warn!(?err, "failed to save attachment");
                            show_composer_toast(&parent_widget, &tr!("Could not save attachment"));
                        }
                        None => {
                            show_composer_toast(&parent_widget, &tr!("Could not save attachment"))
                        }
                    }
                });
            }
            Err(err) if is_user_cancelled_dialog(&err) => {}
            Err(err) => {
                tracing::warn!(?err, "save dialog failed");
            }
        },
    );
}

fn suggested_attachment_filename(filename: &str) -> String {
    sanitize_filename(filename, "attachment")
}

pub fn open_attachment_file(
    cid_cache: &CidCache,
    sha256: &str,
    filename: &str,
    parent: Option<gtk::Window>,
) {
    let Some((_bytes, mime)) = cid_cache.borrow().get(sha256).cloned() else {
        tracing::warn!(sha256, "attachment bytes not in cache");
        return;
    };
    let cid_cache = cid_cache.clone();
    let sha256 = sha256.to_string();
    let filename = filename.to_string();
    confirm_risky_attachment_then(
        parent.clone(),
        filename.clone(),
        &mime,
        sha256.clone(),
        move || {
            launch_attachment_file(&cid_cache, &sha256, &filename, parent);
        },
    );
}

fn launch_attachment_file(
    cid_cache: &CidCache,
    sha256: &str,
    filename: &str,
    parent: Option<gtk::Window>,
) {
    let requested = [(sha256.to_string(), filename.to_string())];
    let item = match cached_save_items(cid_cache, &requested) {
        Ok(items) => items.into_iter().next(),
        Err(err) => {
            tracing::warn!(sha256, ?err, "not opening attachment");
            let dialog = adw::AlertDialog::builder()
                .heading(tr!("Can’t open attachment"))
                .body(err.user_message())
                .build();
            dialog.add_response("ok", "OK");
            dialog.present(parent.as_ref());
            return;
        }
    };
    let Some(SaveItem { bytes, .. }) = item else {
        return;
    };
    let cache_dir = opened_attachments_dir();
    let path = cache_dir.join(attachment_cache_name(sha256, filename));
    gtk::glib::spawn_future_local(async move {
        let file_path = path.clone();
        let written = crate::blocking_io::run(move || {
            create_private_dir_all(&cache_dir)?;
            write_private_file_atomic(&file_path, &bytes)
        })
        .await;
        match written {
            Some(Ok(())) => {}
            Some(Err(err)) => {
                tracing::warn!(?err, "failed to write attachment file");
                return;
            }
            None => return,
        }
        let file = gtk::gio::File::for_path(&path);
        let launcher = gtk::FileLauncher::new(Some(&file));
        ignore_focus_selection_briefly();
        launcher.launch(parent.as_ref(), None::<&gtk::gio::Cancellable>, |result| {
            if let Err(err) = result {
                if is_user_cancelled_dialog(&err) {
                    return;
                }
                tracing::warn!(?err, "failed to open attachment");
            }
        });
    });
}

fn is_user_cancelled_dialog(err: &gtk::glib::Error) -> bool {
    err.matches(gtk::gio::IOErrorEnum::Cancelled)
        || err.matches(gtk::DialogError::Cancelled)
        || err.matches(gtk::DialogError::Dismissed)
}

/// Where "Open" writes attachment copies for the external app: the
/// per-user runtime dir (tmpfs, `0700`, cleared at logout), wiped at startup
/// by [`wipe_opened_attachments`].
fn opened_attachments_dir() -> PathBuf {
    gtk::glib::user_runtime_dir()
        .join("alcedo")
        .join("opened-attachments")
}

/// Delete attachment copies left from earlier runs, including the legacy
/// `~/.cache/alcedo/attachments` location that was never cleaned.
pub fn wipe_opened_attachments() {
    let _ = std::fs::remove_dir_all(opened_attachments_dir());
    let _ = std::fs::remove_dir_all(
        gtk::glib::user_cache_dir()
            .join("alcedo")
            .join("attachments"),
    );
}

fn attachment_cache_name(sha256: &str, filename: &str) -> String {
    let safe = sanitize_filename(filename, "attachment");
    let prefix = sha256.get(..8).unwrap_or("attach");
    format!("{prefix}_{safe}")
}

struct CanvasPaint {
    scheme: &'static str,
    canvas: String,
    text: String,
    link: String,
    rgba: gdk::RGBA,
}

/// Canvas, text, and link colours from the view's CSS, or a light paper
/// canvas when the reader is showing the message's own colours.
fn canvas_paint(widget: &impl IsA<gtk::Widget>, original: bool) -> CanvasPaint {
    let widget = widget.upcast_ref::<gtk::Widget>();
    if original {
        let rgba = gdk::RGBA::new(1.0, 1.0, 1.0, 1.0);
        let link = lookup_css_color(widget, &["accent_color", "accent_bg_color"])
            .unwrap_or_else(|| adw::StyleManager::default().accent_color_rgba());
        return CanvasPaint {
            scheme: "light",
            canvas: rgba_hex(&rgba),
            text: "#1e1e1e".to_string(),
            link: rgba_hex(&link),
            rgba,
        };
    }
    let dark = reader_theme_is_dark();
    let background = lookup_css_color(widget, &["view_bg_color", "window_bg_color"])
        .unwrap_or_else(|| {
            if dark {
                gdk::RGBA::new(0.11, 0.11, 0.11, 1.0)
            } else {
                gdk::RGBA::new(1.0, 1.0, 1.0, 1.0)
            }
        });
    let foreground = lookup_css_color(widget, &["view_fg_color", "window_fg_color"])
        .unwrap_or_else(|| widget.color());
    let link = lookup_css_color(widget, &["accent_color", "accent_bg_color"])
        .unwrap_or_else(|| adw::StyleManager::default().accent_color_rgba());
    CanvasPaint {
        scheme: if dark { "dark" } else { "light" },
        canvas: rgba_hex(&background),
        text: rgba_hex(&foreground),
        link: rgba_hex(&link),
        rgba: background,
    }
}

fn mail_wrapper_css(
    density: ListDensity,
    scheme: &str,
    canvas: &str,
    text: &str,
    link: &str,
    font_px: i32,
) -> String {
    let (body_pad, plain_line_height, body_line_height) = match density {
        ListDensity::Compact => ("4px 8px", "1.25", "1.25"),
        ListDensity::Comfortable => ("6px 10px", "1.3", "1.3"),
        ListDensity::Relaxed => ("12px 16px", "1.45", "1.45"),
    };
    let color_scheme = scheme;
    format!(
        r#"  html {{
    color-scheme: {color_scheme};
  }}
  html, body {{
    margin: 0;
    padding: 0;
    background: {canvas};
    color: {text};
    font: {font_px}px/{body_line_height} system-ui, "Segoe UI", sans-serif;
    overflow-wrap: break-word;
  }}
  body {{ padding: {body_pad}; }}
  p {{
    margin: 1em 0;
  }}
  p.MsoNormal, p.MsoPlainText, p.x_MsoNormal {{
    margin: 0;
  }}
  pre, h1, h2, h3, h4, h5, h6 {{
    margin: 0;
    padding: 0;
  }}
  ul, ol {{
    margin: 0.4em 0;
    padding: 0 0 0 1.6em;
  }}
  li {{
    margin: 0.2em 0;
  }}
  img {{ max-width: 100%; height: auto; }}
  table {{ max-width: 100%; }}
  a {{
    color: {link};
    text-decoration: underline;
    cursor: pointer;
  }}
  .alcedo-plain {{
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: {font_px}px;
    line-height: {plain_line_height};
  }}
  blockquote, blockquote.alcedo-quote {{
    margin: 0.35em 0;
    padding-left: 0.75em;
    border-left: 3px solid rgba(127,127,127,0.45);
  }}"#
    )
}

fn wrap_mail_document(body: &str, paint: &CanvasPaint) -> String {
    let css = mail_wrapper_css(
        list_density(),
        paint.scheme,
        &paint.canvas,
        &paint.text,
        &paint.link,
        reader_font_size(),
    );
    let color_scheme = paint.scheme;
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<meta name="color-scheme" content="{color_scheme}">
<style>
{css}
</style>
</head>
<body>{body}</body>
</html>"#
    )
}

fn reader_theme_is_dark() -> bool {
    adw::StyleManager::default().is_dark()
}

fn apply_web_view_background(web_view: &WebView, color: &gdk::RGBA) {
    web_view.set_background_color(color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_wrapper_css_compact() {
        let css = mail_wrapper_css(
            ListDensity::Compact,
            "light",
            "transparent",
            "inherit",
            "#1c71d8",
            14,
        );
        assert!(css.contains("4px 8px"));
        assert!(css.contains("1.25"));
        assert!(css.contains("font: 14px/1.25"));
        assert!(!css.contains("br:last-child"));
        assert!(css.contains("padding: 0 0 0 1.6em"));
        assert!(css.contains("margin: 1em 0"));
        assert!(css.contains("p.MsoNormal"));
        assert!(css.contains("table { max-width: 100%; }"));
        assert!(!css.contains("p, ul, ol"));
        assert!(!css.contains("p + p"));
        assert!(!css.contains("p { line-height"));
        assert!(!css.contains("div { line-height"));
    }

    #[test]
    fn mail_wrapper_css_comfortable() {
        let css = mail_wrapper_css(
            ListDensity::Comfortable,
            "light",
            "transparent",
            "inherit",
            "#1c71d8",
            14,
        );
        assert!(css.contains("6px 10px"));
        assert!(css.contains("1.3"));
        assert!(css.contains("font: 14px/1.3"));
        assert!(!css.contains("br:last-child"));
        assert!(css.contains("padding: 0 0 0 1.6em"));
        assert!(css.contains("margin: 1em 0"));
        assert!(css.contains("p.MsoNormal"));
        assert!(!css.contains("p, ul, ol"));
        assert!(!css.contains("p + p"));
        assert!(!css.contains("p { line-height"));
        assert!(!css.contains("div { line-height"));
    }

    #[test]
    fn mail_wrapper_css_relaxed() {
        let css = mail_wrapper_css(
            ListDensity::Relaxed,
            "light",
            "transparent",
            "inherit",
            "#1c71d8",
            14,
        );
        assert!(css.contains("1.45"));
        assert!(css.contains("font: 14px/1.45"));
        assert!(!css.contains("br:last-child"));
        assert!(css.contains("padding: 0 0 0 1.6em"));
        assert!(css.contains("margin: 1em 0"));
        assert!(css.contains("p.MsoNormal"));
        assert!(!css.contains("p, ul, ol"));
        assert!(!css.contains("p + p"));
        assert!(!css.contains("p { line-height"));
        assert!(!css.contains("div { line-height"));
    }

    #[test]
    fn mail_wrapper_css_dark_uses_supplied_canvas() {
        let css = mail_wrapper_css(
            ListDensity::Comfortable,
            "dark",
            "#303030",
            "#f6f5f4",
            "#3584e4",
            14,
        );
        assert!(css.contains("color-scheme: dark"));
        assert!(css.contains("background: #303030"));
        assert!(css.contains("color: #f6f5f4"));
        assert!(css.contains("color: #3584e4"));
        assert!(!css.contains("#242424"));
        assert!(!css.contains("#eeeeee"));
        assert!(!css.contains("prefers-color-scheme"));
    }

    #[test]
    fn mail_wrapper_css_light_keeps_transparent_canvas() {
        let css = mail_wrapper_css(
            ListDensity::Comfortable,
            "light",
            "transparent",
            "inherit",
            "#1c71d8",
            14,
        );
        assert!(css.contains("color-scheme: light"));
        assert!(css.contains("background: transparent"));
        assert!(css.contains("color: inherit"));
        assert!(css.contains("color: #1c71d8"));
        assert!(!css.contains("background: #242424"));
    }

    #[test]
    fn mail_wrapper_css_uses_reader_font_px() {
        let css = mail_wrapper_css(
            ListDensity::Comfortable,
            "light",
            "transparent",
            "inherit",
            "#1c71d8",
            18,
        );
        assert!(css.contains("font: 18px/1.3"));
        assert!(css.contains("font-size: 18px"));
    }

    #[test]
    fn multi_placeholder_title_pluralizes() {
        assert_eq!(multi_placeholder_title(2), "2 messages selected");
        assert_eq!(multi_placeholder_title(10), "10 messages selected");
    }

    #[test]
    fn prepare_body_renders_html_fragment_in_plaintext() {
        let text = "Hi&nbsp;Carter,<br><span>Hope you are doing well.</span>";
        let prepared = prepare_body(None, text);
        assert!(prepared.is_html);
        assert_eq!(prepared.source, text);
    }

    #[test]
    fn prepare_body_prefers_text_over_escaped_mailparser_html() {
        let text = "Hi&nbsp;Carter,<br><span>Hope you are doing well.</span>";
        let html = "<html><body>Hi&nbsp;Carter,&lt;br&gt;&lt;span&gt;Hope you are doing well.&lt;/span&gt;</body></html>";
        let prepared = prepare_body(Some(html), text);
        assert!(prepared.is_html);
        assert_eq!(prepared.source, text);
    }

    #[test]
    fn prepare_body_keeps_real_html_part() {
        let html = "<html><body><p>Hello from HTML</p></body></html>";
        let prepared = prepare_body(Some(html), "Hello from HTML");
        assert!(prepared.is_html);
        assert_eq!(prepared.source, html);
    }

    #[test]
    fn prepare_body_keeps_html_with_escaped_addresses() {
        let html = "<html><body><p>Thanks</p><p>On Jan 1, Alice &lt;alice@example.com&gt; wrote:</p><blockquote>Hello</blockquote></body></html>";
        let text = "Thanks\n\nOn Jan 1, Alice <alice@example.com> wrote:\n\nHello";
        let prepared = prepare_body(Some(html), text);
        assert!(prepared.is_html);
        assert_eq!(prepared.source, html);
    }

    #[test]
    fn prepare_body_keeps_sent_html_list() {
        let html = "<html><body><p>Thanks for the reply. Three follow-ups:</p><ol><li>In gVault I see Financials Monthly, but <strong>March 2026 is missing</strong>.</li><li>You listed me as a <strong>signer and bank user</strong>.</li></ol><p>On Sep 11, Darcel Dookwah, LCAM wrote:</p><blockquote>Good afternoon</blockquote></body></html>";
        let text = "Thanks for the reply. Three follow-ups:\nIn gVault I see Financials Monthly, but March 2026 is missing.\nYou listed me as a signer and bank user.\nOn Sep 11, Darcel Dookwah, LCAM wrote:\nGood afternoon";
        let prepared = prepare_body(Some(html), text);
        assert!(prepared.is_html);
        assert_eq!(prepared.source, html);
        let sanitized = sanitize_html(&prepared.source, SanitizeOptions::default());
        assert!(sanitized.contains("<ol>"), "{sanitized}");
        assert!(sanitized.contains("<li>"), "{sanitized}");
        assert!(
            sanitized.contains("<strong>") || sanitized.contains("<b>"),
            "{sanitized}"
        );
    }

    #[test]
    fn prepare_body_uses_plaintext_instead_of_mailparser_br_wrapper() {
        let html = "<html><body>Hello Guys,<br/><br/>Nice talking</body></html>";
        let text = "Hello Guys,\n\nNice talking";
        let prepared = prepare_body(Some(html), text);
        assert!(!prepared.is_html);
        assert_eq!(prepared.source, text);
    }

    #[test]
    fn mailto_address_extracts_path() {
        assert_eq!(
            mailto_address("mailto:alice@example.com"),
            Some("alice@example.com".to_string())
        );
    }

    #[test]
    fn mailto_address_rejects_http() {
        assert_eq!(mailto_address("https://example.com"), None);
    }

    #[test]
    fn attachment_icon_name_maps_common_types() {
        assert_eq!(
            attachment_icon_name("image/png", "photo.png"),
            "image-x-generic-symbolic"
        );
        assert_eq!(
            attachment_icon_name("application/pdf", "doc.pdf"),
            "x-office-document-symbolic"
        );
        assert_eq!(
            attachment_icon_name("application/octet-stream", "report.xlsx"),
            "x-office-spreadsheet-symbolic"
        );
        assert_eq!(
            attachment_icon_name("application/zip", "archive.zip"),
            "package-x-generic-symbolic"
        );
        assert_eq!(
            attachment_icon_name("text/calendar", "invite.ics"),
            "x-office-calendar-symbolic"
        );
        assert_eq!(
            attachment_icon_name("application/sql", "live fields.sql"),
            "text-x-generic-symbolic"
        );
        assert_eq!(
            attachment_icon_name("application/octet-stream", "notes.bin"),
            "mail-attachment-symbolic"
        );
    }

    #[test]
    fn attachment_cache_name_is_unique_per_hash() {
        assert_eq!(
            attachment_cache_name("abcd1234deadbeef", "invoice.pdf"),
            "abcd1234_invoice.pdf"
        );
        assert_ne!(
            attachment_cache_name("aaaa1111deadbeef", "invoice.pdf"),
            attachment_cache_name("bbbb2222deadbeef", "invoice.pdf")
        );
    }

    #[test]
    fn attachment_cache_name_strips_path_components() {
        let name = attachment_cache_name("abcd1234deadbeef", "../../etc/passwd");
        assert_eq!(name, "abcd1234_passwd");
        assert!(!name.contains(".."));
        assert!(!name.contains('/'));
    }

    #[test]
    fn attachment_cache_name_keeps_unicode_within_the_filesystem_limit() {
        assert_eq!(
            attachment_cache_name("abcd1234deadbeef", "résumé.pdf"),
            "abcd1234_résumé.pdf"
        );
        let name = attachment_cache_name("abcd1234deadbeef", &"日".repeat(200));
        assert!(name.len() <= 255, "{} bytes", name.len());
        assert!(name.starts_with("abcd1234_日"));
    }

    #[test]
    fn suggested_attachment_filename_sanitizes() {
        assert_eq!(suggested_attachment_filename("invoice.pdf"), "invoice.pdf");
        assert_eq!(suggested_attachment_filename("../../etc/passwd"), "passwd");
        assert_eq!(suggested_attachment_filename(""), "attachment");
        assert_eq!(suggested_attachment_filename(".."), "attachment");
        assert_eq!(suggested_attachment_filename("."), "attachment");
        assert_eq!(
            suggested_attachment_filename("my file (1).pdf"),
            "my_file__1_.pdf"
        );
    }

    #[test]
    fn save_all_visible_with_multiple_attachments() {
        assert!(!save_all_visible(0));
        assert!(!save_all_visible(1));
        assert!(save_all_visible(2));
        assert!(save_all_visible(3));
    }

    #[test]
    fn recipient_expand_icon_matches_state() {
        assert_eq!(recipient_expand_icon(false), "pan-down-symbolic");
        assert_eq!(recipient_expand_icon(true), "pan-up-symbolic");
    }

    #[test]
    fn recipient_expand_tooltip_matches_state() {
        assert_eq!(recipient_expand_tooltip(false, 0), "Show all recipients");
        assert_eq!(
            recipient_expand_tooltip(false, 12),
            "Show all 12 recipients"
        );
        assert_eq!(recipient_expand_tooltip(true, 12), "Show fewer recipients");
    }

    #[test]
    fn recipient_expand_visible_matrix() {
        assert!(!recipient_expand_visible(false, false, false));
        assert!(!recipient_expand_visible(false, true, true));
        assert!(!recipient_expand_visible(true, false, false));
        assert!(recipient_expand_visible(true, false, true));
        assert!(recipient_expand_visible(true, true, false));
    }

    #[test]
    fn recipient_line_overflows_when_taller_than_collapsed() {
        assert!(!recipient_line_overflows(0));
        assert!(!recipient_line_overflows(COLLAPSED_RECIPIENT_HEIGHT));
        assert!(recipient_line_overflows(COLLAPSED_RECIPIENT_HEIGHT + 1));
    }

    #[test]
    fn is_user_cancelled_dialog_matches_gio_and_gtk() {
        let cancelled = gtk::glib::Error::new(gtk::gio::IOErrorEnum::Cancelled, "cancelled");
        let dismissed = gtk::glib::Error::new(gtk::DialogError::Dismissed, "dismissed");
        let failed = gtk::glib::Error::new(gtk::DialogError::Failed, "failed");
        assert!(is_user_cancelled_dialog(&cancelled));
        assert!(is_user_cancelled_dialog(&dismissed));
        assert!(!is_user_cancelled_dialog(&failed));
    }

    #[test]
    fn allow_internal_mail_navigation_accepts_blank_anchors() {
        assert!(allow_internal_mail_navigation("about:blank"));
        assert!(allow_internal_mail_navigation("about:blank#section"));
        assert!(allow_internal_mail_navigation("alcedo-cid:abc"));
        assert!(!allow_internal_mail_navigation("https://example.com"));
        assert!(!allow_internal_mail_navigation("about:blank/phishing"));
    }

    #[test]
    fn reader_link_tooltip_not_a_link() {
        assert_eq!(
            reader_link_tooltip(false, Some("https://example.com")),
            None
        );
    }

    #[test]
    fn reader_link_tooltip_missing_uri() {
        assert_eq!(reader_link_tooltip(true, None), None);
    }

    #[test]
    fn reader_link_tooltip_empty_uri() {
        assert_eq!(reader_link_tooltip(true, Some("")), None);
        assert_eq!(reader_link_tooltip(true, Some("   ")), None);
    }

    #[test]
    fn reader_link_tooltip_trims_whitespace() {
        assert_eq!(
            reader_link_tooltip(true, Some("  https://evil.example/a?b=1&c=2  ")),
            Some("https://evil.example/a?b=1&c=2".to_string())
        );
    }

    #[test]
    fn reader_link_tooltip_mailto() {
        assert_eq!(
            reader_link_tooltip(true, Some("mailto:user@example.com?subject=Hello")),
            Some("mailto:user@example.com?subject=Hello".to_string())
        );
    }

    #[test]
    fn reader_link_tooltip_punycode() {
        assert_eq!(
            reader_link_tooltip(true, Some("http://xn--pple-43d.com/login")),
            Some("http://xn--pple-43d.com/login".to_string())
        );
    }

    #[test]
    fn reader_loads_inline_images_while_remote_stays_blocked() {
        let inline = r#"<img src="alcedo-cid://abc">"#;
        assert!(reader_should_load_images(false, inline));
        assert!(!reader_should_load_images(
            false,
            r#"<img src="cid:image002.png@01DD652A.A3420E70">"#
        ));
        assert!(!reader_should_load_images(
            false,
            r#"<p>Hello</p><img src="https://tracker.example/pixel.gif">"#
        ));
        assert!(reader_should_load_images(true, "<p>Hello</p>"));
    }

    #[test]
    fn should_skip_reader_reload_same_source_showing_body_same_allow_remote() {
        assert!(should_skip_reader_reload(
            "<p>hi</p>",
            "<p>hi</p>",
            true,
            false,
            false
        ));
    }

    #[test]
    fn should_skip_reader_reload_allow_remote_flip() {
        assert!(!should_skip_reader_reload(
            "<p>hi</p>",
            "<p>hi</p>",
            true,
            false,
            true
        ));
    }

    #[test]
    fn should_skip_reader_reload_not_showing_body() {
        assert!(!should_skip_reader_reload(
            "<p>hi</p>",
            "<p>hi</p>",
            false,
            false,
            false
        ));
    }

    #[test]
    fn should_skip_reader_reload_source_mismatch() {
        assert!(!should_skip_reader_reload(
            "<p>hi</p>",
            "<p>bye</p>",
            true,
            false,
            false
        ));
    }

    /// Clearing the attachment box (every message change) must free the old
    /// cards and the callbacks wired onto them.
    #[test]
    fn attachment_cards_are_freed_when_cleared() {
        crate::test_helpers::gtk_test_run(|| {
            let sentinel = Rc::new(());
            let on_save_all: Rc<dyn Fn()> = {
                let s = sentinel.clone();
                Rc::new(move || {
                    let _ = &s;
                })
            };
            let box_ = adw::WrapBox::new();
            let cid_cache = alcedo_widgets::new_cid_cache();
            let web_view = WebView::new();
            let plain = build_attachment_card("text-x-generic", "a.txt", "1 KB", true, false);
            wire_attachment_card(
                &plain,
                &cid_cache,
                "sha-a".to_string(),
                "a.txt".to_string(),
                &web_view,
                Some(on_save_all.clone()),
            );
            let invite = build_attachment_card("x-office-calendar", "i.ics", "1 KB", true, false);
            wire_calendar_attachment_card(
                &invite,
                &cid_cache,
                &BodyAttachment {
                    filename: "i.ics".to_string(),
                    mime: "text/calendar".to_string(),
                    sha256: "sha-i".to_string(),
                    size: 1024,
                },
                &web_view,
                Some(on_save_all),
            );
            box_.append(&plain);
            box_.append(&invite);
            let weak_cards = [plain.downgrade(), invite.downgrade()];
            drop((plain, invite));
            clear_attachment_chips(&box_);
            assert!(
                weak_cards.iter().all(|card| card.upgrade().is_none()),
                "card leaked"
            );
            assert_eq!(Rc::strong_count(&sentinel), 1, "card callbacks leaked");
        });
    }

    /// A reader page in a window that is then destroyed (what closing a
    /// message window does) must free its WebView and every callback it holds.
    #[test]
    fn reader_page_is_freed_after_its_window_is_destroyed() {
        crate::test_helpers::gtk_test_run(|| {
            let sentinel = Rc::new(());
            macro_rules! cb {
                (($($arg:pat_param),*)) => {{
                    let s = sentinel.clone();
                    move |$($arg),*| {
                        let _ = &s;
                    }
                }};
            }
            let page = build_reader_page(
                cb!((_uri)),
                cb!(()),
                cb!(()),
                cb!(()),
                cb!(()),
                cb!(()),
                cb!(()),
                cb!(()),
                cb!(()),
                cb!(()),
                cb!(()),
                cb!(()),
                cb!(()),
                cb!(()),
                cb!(()),
                cb!(()),
                cb!(()),
                cb!((_id)),
                Vec::new(),
                cb!(()),
                cb!((_emoji)),
                cb!((_anchor, _addr)),
                alcedo_widgets::new_cid_cache(),
            );
            // Address chips carry their own callbacks and menus; build some.
            let on_address_click: Rc<dyn Fn(&gtk::Widget, Address)> = Rc::new(|_, _| {});
            page.widgets.to.populate(
                &[
                    Address {
                        name: Some("Ada".to_string()),
                        email: "ada@example.com".to_string(),
                    },
                    Address {
                        name: None,
                        email: "bob@example.com".to_string(),
                    },
                ],
                &on_address_click,
            );
            page.widgets.from.populate(
                &[Address {
                    name: None,
                    email: "cat@example.com".to_string(),
                }],
                &on_address_click,
            );
            let weak_root = page.root.downgrade();
            let weak_web_view = page.widgets.web_view.downgrade();
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
            while weak_web_view.upgrade().is_some() && std::time::Instant::now() < deadline {
                context.iteration(false);
            }
            assert!(weak_root.upgrade().is_none(), "reader root leaked");
            let mut leaked: Vec<&str> = weak_widgets
                .iter()
                .filter(|(_, weak)| weak.upgrade().is_some())
                .map(|(name, _)| name.as_str())
                .collect();
            leaked.sort_unstable();
            leaked.dedup();
            assert!(leaked.is_empty(), "reader widgets leaked: {leaked:?}");
            assert!(weak_web_view.upgrade().is_none(), "reader WebView leaked");
            assert_eq!(Rc::strong_count(&sentinel), 1, "reader callbacks leaked");
        });
    }
}
