//! Main application window (P4) — thin shell routing Mail, Contacts and extension spaces.

use alcedo_i18n::{tr, trs};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use alcedo_core::{
    Address, ContactDraft, ContactId, MessageId, MessageListMode, ParticipationStatus, SyncEvent,
    UiCommand,
};
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::gio;
use relm4::gtk::glib;
use relm4::prelude::*;
use relm4::{Receiver, Sender};

use crate::activity_log::ActivityLog;
use crate::command_palette::{close_palette_if_open, is_palette_open, window_has_foreign_dialog};
use crate::commands::{ShortcutScope, command_catalog, effective_accel};
use crate::contacts::{
    ContactsInit, ContactsModel, ContactsMsg, ContactsOutput, mock_contact_book,
};
use crate::icons;
use crate::mail::{
    InviteResponsePayload, MailInit, MailModel, MailMsg, MailOutput, mock_seed_meeting_invite,
};
use crate::menu::{
    ACTION_COMMAND_PALETTE, ACTION_COMPOSE, ACTION_GO_UNIFIED, ACTION_REPLY_ALL, ACTION_SELECT_ALL,
    ACTION_STAR, install_accels,
};
use crate::mock::MockData;
use crate::notifications;
use crate::preferences::{ACCOUNTS_PAGE_NAME, COMPOSING_PAGE_NAME, build_preferences_dialog};
use crate::rail::{
    RailSpace, apply_rail_layout, set_badge_count, space_rail_button, update_space_selection,
    watch_folder_reveal, wrap_with_badge,
};
use crate::recipient_picker::mock_sent_recipients;
use crate::recipient_row::ContactBook;
use crate::settings::{
    ListDensity, apply_list_density, clamp_rail_width, close_to_tray, message_list_mode,
    on_activity_log_limit_changed, on_close_to_tray_changed,
    on_keyboard_shortcut_overrides_changed, on_message_list_mode_changed,
    on_show_message_preview_changed, rail_visible, rail_width, set_message_list_mode,
    set_rail_visible, set_rail_width, set_show_message_preview, show_message_preview,
};
use crate::status_bar::{StatusBar, build_status_bar};
use crate::tray::{TrayHandle, set_tray_unread, start_tray, stop_tray};

const MAIN_WINDOW_NAME: &str = "alcedo-main";

/// Which pane is visible in the content stack.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContentPane {
    #[default]
    Reader,
    Composer,
    Contacts,
    /// A space an extension added, by name.
    Space(&'static str),
}

/// Application initialization passed from the binary.
pub struct AppInit {
    pub mock: bool,
    pub cmd: Sender<UiCommand>,
    pub events: Receiver<SyncEvent>,
    pub runtime: tokio::runtime::Handle,
}

pub struct AppModel {
    content_pane: ContentPane,
    preferences: Rc<crate::preferences::PreferencesUi>,
    tray: Option<TrayHandle>,
    status_bar: StatusBar,
    activity: Rc<ActivityLog>,
    mail: Controller<MailModel>,
    contacts: Controller<ContactsModel>,
    /// Extension spaces: rail button and "shown" hook, by name.
    spaces: Vec<(&'static str, gtk::Button, Rc<dyn Fn()>)>,
    canvas_stack: gtk::Stack,
    rail: gtk::Box,
    rail_split: gtk::Paned,
    mail_btn: gtk::Button,
    contacts_btn: gtk::Button,
    /// Narrow breakpoint hides the rail without writing the saved preference.
    adaptive_hide_rail: bool,
    mail_unread_badge: gtk::Label,
    last_unread_indicator: Option<u32>,
}

#[derive(Debug, Clone)]
pub enum AppMsg {
    Compose,
    ComposeMailto(String),
    /// Show an extension space by name.
    ShowSpace(&'static str),
    ShowContacts,
    ShowMail,
    ShowReader,
    SetRailVisible(bool),
    /// Hide the rail for a narrow window without changing the saved preference.
    SetAdaptiveRail(bool),
    OpenPreferences,
    OpenComposingPreferences,
    OpenAccountsPreferences,
    OpenContacts,
    AddContactFromAddress(Address),
    ViewContact(ContactId),
    UpsertContact(ContactDraft),
    RespondToInvite {
        message_id: MessageId,
        status: ParticipationStatus,
        ics: String,
    },
    SetSenderRemoteImages {
        email: String,
        allow: bool,
    },
    ThemeChanged,
    ListDensityChanged(ListDensity),
    ListModeChanged(MessageListMode),
    Escape,
    OpenCommandPalette,
    OpenActivityLog,
    SyncEvent(SyncEvent),
    NetworkChanged,
    RefreshStatusBar,
    MainWindowActive,
    Quit,
    RefreshCloseToTray,
    UnifiedUnread(u32),
}

#[relm4::component(pub)]
impl Component for AppModel {
    type Init = AppInit;
    type Input = AppMsg;
    type Output = ();
    type CommandOutput = ();

    view! {
        #[root]
        window = adw::ApplicationWindow {
            set_title: Some("Alcedo"),
            set_icon_name: Some("dev.alcedo.Mail"),
            connect_map => |window| {
                crate::theme::apply_toplevel_icon(window);
            },
            set_widget_name: MAIN_WINDOW_NAME,
            set_default_width: 1280,
            set_default_height: 800,
            set_width_request: 360,
            set_height_request: 480,

            #[wrap(Some)]
            set_content = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                #[name = "toasts"]
                adw::ToastOverlay {
                    set_vexpand: true,
                    set_hexpand: true,

                    #[wrap(Some)]
                    #[name = "rail_split"]
                    set_child = &gtk::Paned {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_wide_handle: true,
                        set_position: 56,
                        set_resize_start_child: false,
                        set_shrink_start_child: false,
                        set_resize_end_child: true,
                        set_shrink_end_child: false,
                        add_css_class: "alcedo-rail-paned",

                        #[wrap(Some)]
                        #[name = "rail"]
                        set_start_child = &gtk::Box {
                            add_css_class: "alcedo-rail",
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 8,
                            set_width_request: 56,
                            set_halign: gtk::Align::Center,

                            #[name = "compose_btn"]
                            gtk::Button {
                                add_css_class: "alcedo-rail-btn",
                                add_css_class: "alcedo-rail-compose",
                                set_hexpand: true,
                                #[wrap(Some)]
                                set_child = &gtk::Box {
                                    set_orientation: gtk::Orientation::Horizontal,
                                    set_spacing: 10,
                                    add_css_class: "alcedo-rail-content",
                                    gtk::Image {
                                        set_pixel_size: 24,
                                    },
                                    gtk::Label {
                                        add_css_class: "alcedo-rail-label",
                                        set_label: trs!("Compose"),
                                        set_xalign: 0.0,
                                        set_halign: gtk::Align::Start,
                                        set_hexpand: true,
                                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                                        set_visible: false,
                                    },
                                },
                                connect_clicked => AppMsg::Compose,
                            },

                            #[name = "mail_btn"]
                            gtk::Button {
                                set_tooltip_text: Some(trs!("Mail")),
                                add_css_class: "flat",
                                add_css_class: "alcedo-rail-btn",
                                set_hexpand: true,
                                #[wrap(Some)]
                                set_child = &gtk::Box {
                                    set_orientation: gtk::Orientation::Horizontal,
                                    set_spacing: 10,
                                    add_css_class: "alcedo-rail-content",
                                    gtk::Image {
                                        set_pixel_size: 24,
                                    },
                                    gtk::Label {
                                        add_css_class: "alcedo-rail-label",
                                        set_label: trs!("Mail"),
                                        set_xalign: 0.0,
                                        set_halign: gtk::Align::Start,
                                        set_hexpand: true,
                                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                                        set_visible: false,
                                    },
                                },
                                connect_clicked => AppMsg::ShowMail,
                            },

                            #[name = "contacts_btn"]
                            gtk::Button {
                                set_tooltip_text: Some(trs!("Contacts")),
                                add_css_class: "flat",
                                add_css_class: "alcedo-rail-btn",
                                set_hexpand: true,
                                #[wrap(Some)]
                                set_child = &gtk::Box {
                                    set_orientation: gtk::Orientation::Horizontal,
                                    set_spacing: 10,
                                    add_css_class: "alcedo-rail-content",
                                    gtk::Image {
                                        set_pixel_size: 24,
                                    },
                                    gtk::Label {
                                        add_css_class: "alcedo-rail-label",
                                        set_label: trs!("Contacts"),
                                        set_xalign: 0.0,
                                        set_halign: gtk::Align::Start,
                                        set_hexpand: true,
                                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                                        set_visible: false,
                                    },
                                },
                                connect_clicked => AppMsg::ShowContacts,
                            },

                            gtk::Box { set_vexpand: true },

                            #[name = "rail_hide_btn"]
                            gtk::Button {
                                set_tooltip_text: Some(trs!("Hide app rail")),
                                add_css_class: "flat",
                                add_css_class: "alcedo-rail-btn",
                                set_hexpand: true,
                                connect_clicked => AppMsg::SetRailVisible(false),
                            },
                        },

                        #[wrap(Some)]
                        #[name = "canvas_stack"]
                        set_end_child = &gtk::Stack {
                            set_hexpand: true,
                            set_vexpand: true,
                            // A homogeneous stack takes the largest page's
                            // minimum. A wide hidden page would set it, and
                            // the window clips content below its minimum,
                            // hiding the title buttons on narrow mail windows.
                            set_hhomogeneous: false,
                            set_vhomogeneous: false,
                        },
                    },
                },

                #[name = "status_bar_slot"]
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_hexpand: true,
                },
            },
        },
    }

    fn init(
        init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let (accounts, folders, envelopes, source_envelopes, bodies) = if init.mock {
            let mock = MockData::new();
            let source_envelopes = mock.envelopes.clone();
            (
                mock.accounts,
                mock.folders,
                mock.envelopes,
                source_envelopes,
                mock.bodies,
            )
        } else {
            (
                Vec::new(),
                HashMap::new(),
                Vec::new(),
                Vec::new(),
                HashMap::new(),
            )
        };

        let cid_cache = alcedo_widgets::new_cid_cache();
        let mut attachments_by_id = HashMap::new();
        if init.mock {
            mock_seed_meeting_invite(&envelopes, &mut attachments_by_id, &cid_cache);
        }
        let contact_book = Rc::new(RefCell::new(if init.mock {
            mock_contact_book()
        } else {
            ContactBook {
                contacts: Vec::new(),
                groups: Vec::new(),
            }
        }));
        let sent_recipients = Rc::new(RefCell::new(if init.mock {
            mock_sent_recipients(&envelopes, &folders, &accounts)
        } else {
            Vec::new()
        }));
        let remote_image_senders = Rc::new(RefCell::new(HashSet::new()));

        let widgets = view_output!();
        icons::set_rail_button_icon(&widgets.compose_btn, icons::COMPOSE);
        icons::set_rail_button_icon(&widgets.mail_btn, icons::MAIL);
        icons::set_rail_button_icon(&widgets.contacts_btn, icons::CONTACTS);
        icons::set_button_icon(
            &widgets.rail_hide_btn,
            icons::RAIL_HIDE,
            icons::TOOLBAR_PIXEL_SIZE,
        );
        crate::a11y::name_icon_control(
            &widgets.compose_btn,
            &tr!("Compose"),
            &crate::commands::command_tooltip(ACTION_COMPOSE, &tr!("Compose")),
        );

        let status_bar = build_status_bar();
        widgets.status_bar_slot.append(&status_bar.root);

        let activity = ActivityLog::new();
        let activity_log_action = gio::SimpleAction::new("activity-log", None);
        let activity_log_sender = sender.clone();
        activity_log_action.connect_activate(move |_, _| {
            activity_log_sender.input(AppMsg::OpenActivityLog);
        });
        root.add_action(&activity_log_action);

        // Before the mail and contacts pages, whose primary menus list the spaces.
        let app_ctx = crate::extension::AppContext::new(crate::extension::AppParts {
            app: sender.input_sender().clone(),
            cmd: init.cmd.clone(),
            window: root.clone(),
            toasts: widgets.toasts.clone(),
            runtime: init.runtime.clone(),
            activity: activity.clone(),
            cid_cache: cid_cache.clone(),
            mock: init.mock,
        });
        let spaces = install_spaces(
            &app_ctx,
            &widgets.rail,
            &widgets.mail_btn,
            &widgets.canvas_stack,
            &sender,
        );
        let mail = MailModel::builder()
            .launch(MailInit {
                mock: init.mock,
                cmd: init.cmd.clone(),
                runtime: init.runtime.clone(),
                toasts: widgets.toasts.clone(),
                status_bar: status_bar.clone(),
                window: root.clone(),
                contact_book: contact_book.clone(),
                sent_recipients: sent_recipients.clone(),
                remote_image_senders: remote_image_senders.clone(),
                cid_cache: cid_cache.clone(),
                accounts: accounts.clone(),
                folders: folders.clone(),
                envelopes: envelopes.clone(),
                source_envelopes: source_envelopes.clone(),
                bodies: bodies.clone(),
                app_sender: sender.input_sender().clone(),
                activity: activity.clone(),
            })
            .forward(sender.input_sender(), MailOutput::into_app_msg);
        widgets.canvas_stack.add_named(mail.widget(), Some("mail"));
        widgets.canvas_stack.set_visible_child_name("mail");

        let contacts = ContactsModel::builder()
            .launch(ContactsInit {
                mock: init.mock,
                cmd: init.cmd.clone(),
                toasts: widgets.toasts.clone(),
                contact_book: contact_book.clone(),
                remote_image_senders: remote_image_senders.clone(),
                activity: activity.clone(),
            })
            .forward(sender.input_sender(), ContactsOutput::into_app_msg);
        widgets
            .canvas_stack
            .add_named(contacts.widget(), Some("contacts"));

        let preferences = Rc::new(build_preferences_dialog(
            sender.input_sender().clone(),
            mail.sender().clone(),
            init.runtime.clone(),
        ));
        mail.emit(MailMsg::SetPreferences(preferences.clone()));

        let mail_sender = mail.sender().clone();
        let mail_is_shown = {
            let stack = widgets.canvas_stack.downgrade();
            move || {
                stack
                    .upgrade()
                    .is_some_and(|stack| stack.visible_child_name().as_deref() == Some("mail"))
            }
        };
        watch_folder_reveal(
            &widgets.rail_split,
            &widgets.rail,
            mail_is_shown,
            {
                let mail_sender = mail_sender.clone();
                move |hovered| mail_sender.emit(MailMsg::RailRevealHover(hovered))
            },
            {
                let mail_sender = mail_sender.clone();
                move |inside| mail_sender.emit(MailMsg::RailPointerInside(inside))
            },
        );
        let rail_visible_state = rail_visible();
        let rail_width_state = rail_width();
        apply_rail_layout(&widgets.rail, rail_width_state);
        widgets.rail.set_visible(rail_visible_state);
        widgets.rail_split.set_position(rail_width_state);

        let mail_content_box = widgets
            .mail_btn
            .child()
            .expect("mail button child")
            .downcast::<gtk::Box>()
            .expect("mail button content box");
        let mail_icon = mail_content_box
            .first_child()
            .expect("mail icon")
            .downcast::<gtk::Image>()
            .expect("mail icon image");
        let mail_unread_badge = wrap_with_badge(&mail_content_box, &mail_icon);

        let mut model = AppModel {
            content_pane: ContentPane::Reader,
            preferences: preferences.clone(),
            tray: None,
            status_bar,
            activity: activity.clone(),
            mail,
            contacts,
            spaces,
            canvas_stack: widgets.canvas_stack.clone(),
            rail: widgets.rail.clone(),
            rail_split: widgets.rail_split.clone(),
            mail_btn: widgets.mail_btn.clone(),
            contacts_btn: widgets.contacts_btn.clone(),
            adaptive_hide_rail: false,
            mail_unread_badge,
            last_unread_indicator: None,
        };

        model.update_rail_highlight();
        wire_rail_pane_persist(&model.rail_split, &model.rail);

        adw::StyleManager::default().connect_notify(Some("dark"), {
            let sender = sender.clone();
            move |_, _| sender.input(AppMsg::ThemeChanged)
        });
        gio::NetworkMonitor::default().connect_network_changed({
            let sender = sender.clone();
            move |_, _| sender.input(AppMsg::NetworkChanged)
        });
        glib::timeout_add_seconds_local(30, {
            let sender = sender.clone();
            move || {
                sender.input(AppMsg::RefreshStatusBar);
                glib::ControlFlow::Continue
            }
        });

        setup_shortcuts(&root, sender.clone(), mail_sender.clone());
        let gtk_app = relm4::main_application();
        install_accels(&gtk_app);
        setup_application_actions(
            &gtk_app,
            &root,
            &sender,
            mail_sender.clone(),
            init.runtime.clone(),
        );
        install_layout_breakpoints(&root, sender.clone(), mail_sender.clone());
        on_keyboard_shortcut_overrides_changed({
            let window = root.clone();
            let sender = sender.clone();
            let mail_sender = mail_sender.clone();
            let compose_btn = widgets.compose_btn.clone();
            move || {
                rebuild_window_shortcuts(&window, sender.clone(), mail_sender.clone());
                install_accels(&relm4::main_application());
                crate::a11y::name_icon_control(
                    &compose_btn,
                    &tr!("Compose"),
                    &crate::commands::command_tooltip(ACTION_COMPOSE, &tr!("Compose")),
                );
                mail_sender.emit(MailMsg::RefreshShortcutTooltips);
            }
        });

        let sender_active = sender.clone();
        root.connect_notify_local(Some("is-active"), move |window, _| {
            if window.is_active() {
                sender_active.input(AppMsg::MainWindowActive);
            }
        });

        let on_quit = {
            let sender = sender.clone();
            std::sync::Arc::new(move || sender.input(AppMsg::Quit))
        };
        apply_close_to_tray(&root, &mut model.tray, on_quit, 0);
        on_close_to_tray_changed({
            let sender = sender.clone();
            move |_| sender.input(AppMsg::RefreshCloseToTray)
        });

        on_activity_log_limit_changed({
            let activity = activity.clone();
            move || activity.trim_to_limit()
        });

        if !init.mock {
            let events = init.events;
            let sender = sender.clone();
            glib::spawn_future_local(async move {
                events
                    .forward(sender.input_sender().clone(), AppMsg::SyncEvent)
                    .await;
            });
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match msg {
            AppMsg::Compose => {
                self.show_pane(ContentPane::Composer);
                self.mail.emit(MailMsg::Compose);
            }
            AppMsg::ComposeMailto(address) => {
                self.show_pane(ContentPane::Composer);
                self.mail.emit(MailMsg::ComposeMailto(address));
            }
            AppMsg::ShowSpace(name) => self.show_pane(ContentPane::Space(name)),
            AppMsg::ShowContacts => {
                self.contacts.emit(ContactsMsg::RefreshContacts);
                self.show_pane(ContentPane::Contacts);
            }
            AppMsg::ShowMail => {
                if matches!(
                    self.content_pane,
                    ContentPane::Space(_) | ContentPane::Contacts
                ) {
                    self.show_pane(ContentPane::Reader);
                }
            }
            AppMsg::ShowReader => self.show_pane(ContentPane::Reader),
            AppMsg::OpenCommandPalette => {
                if is_palette_open() {
                    close_palette_if_open();
                    return;
                }
                if window_has_foreign_dialog(root) {
                    return;
                }
                self.mail.emit(MailMsg::OpenCommandPalette);
            }
            AppMsg::OpenActivityLog => {
                crate::activity_log::present_activity_dialog(root, &self.activity);
            }
            AppMsg::SetRailVisible(visible) => self.apply_rail_visible(visible),
            AppMsg::SetAdaptiveRail(hide) => self.apply_adaptive_rail(hide),
            AppMsg::SetSenderRemoteImages { email, allow } => {
                self.mail
                    .emit(MailMsg::SetSenderRemoteImages { email, allow });
            }
            AppMsg::OpenPreferences => {
                self.mail.emit(MailMsg::RefreshPreferences);
                self.contacts.emit(ContactsMsg::RefreshContacts);
                self.preferences.dialog.present(Some(root));
            }
            AppMsg::OpenComposingPreferences => {
                self.mail.emit(MailMsg::RefreshPreferences);
                self.contacts.emit(ContactsMsg::RefreshContacts);
                self.preferences
                    .dialog
                    .set_visible_page_name(COMPOSING_PAGE_NAME);
                self.preferences.dialog.present(Some(root));
            }
            AppMsg::OpenAccountsPreferences => {
                self.mail.emit(MailMsg::RefreshPreferences);
                self.contacts.emit(ContactsMsg::RefreshContacts);
                self.preferences
                    .dialog
                    .set_visible_page_name(ACCOUNTS_PAGE_NAME);
                self.preferences.dialog.present(Some(root));
            }
            AppMsg::OpenContacts => {
                present_main_window();
                self.contacts.emit(ContactsMsg::RefreshContacts);
                self.show_pane(ContentPane::Contacts);
                if self.preferences.dialog.is_mapped() {
                    self.preferences.dialog.close();
                }
            }
            AppMsg::UpsertContact(draft) => self.contacts.emit(ContactsMsg::UpsertContact(draft)),
            AppMsg::AddContactFromAddress(addr) => {
                self.show_pane(ContentPane::Contacts);
                self.contacts.emit(ContactsMsg::AddContactFromAddress(addr));
            }
            AppMsg::ViewContact(id) => {
                self.show_pane(ContentPane::Contacts);
                self.contacts.emit(ContactsMsg::ViewContact(id));
            }
            AppMsg::RespondToInvite {
                message_id,
                status,
                ics,
            } => {
                self.mail
                    .emit(MailMsg::InviteResponse(InviteResponsePayload {
                        message_id,
                        status,
                        ics,
                    }));
            }
            AppMsg::ThemeChanged => self.mail.emit(MailMsg::ApplyTheme),
            AppMsg::ListDensityChanged(density) => {
                apply_list_density(root, density);
                self.mail.emit(MailMsg::ListDensityChanged(density));
            }
            AppMsg::ListModeChanged(mode) => {
                self.mail.emit(MailMsg::ListModeChanged(mode));
            }
            AppMsg::Escape => {
                if is_palette_open() {
                    close_palette_if_open();
                    return;
                }
                if matches!(
                    self.content_pane,
                    ContentPane::Space(_) | ContentPane::Contacts
                ) {
                    return;
                }
                self.mail.emit(MailMsg::Escape);
            }
            AppMsg::NetworkChanged => {
                self.status_bar
                    .apply_network(&gio::NetworkMonitor::default());
            }
            AppMsg::RefreshStatusBar => self.mail.emit(MailMsg::RefreshStatusBar),
            AppMsg::Quit => {
                notifications::clear_all(root);
                stop_tray(&mut self.tray);
                root.set_hide_on_close(false);
                relm4::main_application().quit();
            }
            AppMsg::RefreshCloseToTray => {
                let on_quit = {
                    let sender = sender.clone();
                    std::sync::Arc::new(move || sender.input(AppMsg::Quit))
                };
                let unread = self.last_unread_indicator.unwrap_or(0);
                apply_close_to_tray(root, &mut self.tray, on_quit, unread);
            }
            AppMsg::MainWindowActive => {
                notifications::clear_attention(root, self.last_unread_indicator.unwrap_or(0));
            }
            AppMsg::UnifiedUnread(count) => {
                set_badge_count(&self.mail_unread_badge, count);
                let previous = self.last_unread_indicator;
                let _ = notifications::apply_unread(root, previous, count);
                self.last_unread_indicator = Some(count);
                if let Some(handle) = self.tray.as_ref() {
                    set_tray_unread(handle, count);
                }
            }
            AppMsg::SyncEvent(event) => self.fan_out_sync_event(event, &sender, root),
        }
    }
}

impl AppModel {
    fn show_pane(&mut self, pane: ContentPane) {
        if let ContentPane::Space(name) = pane {
            let Some((_, _, shown)) = self.spaces.iter().find(|(space, ..)| *space == name) else {
                return;
            };
            let shown = shown.clone();
            self.content_pane = pane;
            self.canvas_stack.set_visible_child_name(name);
            self.update_rail_highlight();
            shown();
            return;
        }
        if pane == ContentPane::Contacts {
            self.content_pane = pane;
            self.contacts.emit(ContactsMsg::Show);
            self.canvas_stack.set_visible_child_name("contacts");
            self.update_rail_highlight();
            return;
        }
        self.canvas_stack.set_visible_child_name("mail");
        self.content_pane = pane;
        match pane {
            ContentPane::Reader => self.mail.emit(MailMsg::ShowReader),
            ContentPane::Composer => self.mail.emit(MailMsg::ShowComposer),
            _ => {}
        }
        self.update_rail_highlight();
    }

    fn apply_rail_visible(&mut self, visible: bool) {
        set_rail_visible(visible);
        if !self.adaptive_hide_rail {
            self.rail.set_visible(visible);
            if visible {
                let width = rail_width();
                self.rail_split.set_position(width);
                apply_rail_layout(&self.rail, width);
            }
        }
        sync_toggle_rail_action(visible);
    }

    fn apply_adaptive_rail(&mut self, hide_rail: bool) {
        self.adaptive_hide_rail = hide_rail;
        if hide_rail {
            self.rail.set_visible(false);
            return;
        }
        let visible = rail_visible();
        self.rail.set_visible(visible);
        if visible {
            let width = rail_width();
            self.rail_split.set_position(width);
            apply_rail_layout(&self.rail, width);
        }
    }

    fn update_rail_highlight(&self) {
        let space = match self.content_pane {
            ContentPane::Contacts => RailSpace::Contacts,
            ContentPane::Space(name) => RailSpace::Space(name),
            ContentPane::Reader | ContentPane::Composer => RailSpace::Mail,
        };
        let buttons: Vec<(&'static str, gtk::Button)> = self
            .spaces
            .iter()
            .map(|(name, button, _)| (*name, button.clone()))
            .collect();
        update_space_selection(&self.mail_btn, &self.contacts_btn, &buttons, space);
    }

    fn fan_out_sync_event(
        &mut self,
        event: SyncEvent,
        _sender: &ComponentSender<Self>,
        _root: &adw::ApplicationWindow,
    ) {
        match event {
            SyncEvent::ContactsChanged { contacts, groups } => {
                self.contacts.emit(ContactsMsg::ContactsChanged {
                    contacts: contacts.clone(),
                    groups: groups.clone(),
                });
                self.mail.emit(MailMsg::Sync(SyncEvent::ContactsChanged {
                    contacts,
                    groups,
                }));
            }
            other => {
                self.mail.emit(MailMsg::Sync(other));
            }
        }
    }
}

/// Add each extension space: a page in `stack`, a rail button after `after`
/// and an application action. Returns the buttons and "shown" hooks.
fn install_spaces(
    ctx: &crate::extension::AppContext,
    rail: &gtk::Box,
    after: &gtk::Button,
    stack: &gtk::Stack,
    sender: &ComponentSender<AppModel>,
) -> Vec<(&'static str, gtk::Button, Rc<dyn Fn()>)> {
    let gtk_app = relm4::main_application();
    let mut previous: gtk::Widget = after.clone().upcast();
    let mut installed = Vec::new();
    let mut entries = Vec::new();
    for space in crate::extension::registered()
        .iter()
        .flat_map(|extension| extension.spaces(ctx))
    {
        stack.add_named(&space.page, Some(space.name));
        let button = space_rail_button(space.title, &space.icon);
        let name = space.name;
        button.connect_clicked({
            let sender = sender.clone();
            move |_| sender.input(AppMsg::ShowSpace(name))
        });
        rail.insert_child_after(&button, Some(&previous));
        previous = button.clone().upcast();
        if let Some(action_name) = space.action.strip_prefix("app.") {
            let action = gio::SimpleAction::new(action_name, None);
            let sender = sender.clone();
            action.connect_activate(move |_, _| sender.input(AppMsg::ShowSpace(name)));
            gtk_app.add_action(&action);
        }
        entries.push(crate::extension::SpaceEntry {
            action: space.action,
            title: space.title,
            palette_icon: space.palette_icon,
        });
        installed.push((name, button, space.shown));
    }
    crate::extension::set_space_entries(entries);
    installed
}

fn sync_toggle_rail_action(visible: bool) {
    let gtk_app = relm4::main_application();
    if let Some(action) = gtk_app.lookup_action("toggle-rail")
        && let Ok(action) = action.downcast::<gio::SimpleAction>()
    {
        let current = action
            .state()
            .map(|variant| variant.get::<bool>().unwrap_or(true))
            .unwrap_or(true);
        if current != visible {
            action.set_state(&glib::Variant::from(visible));
        }
    }
}

fn wire_rail_pane_persist(paned: &gtk::Paned, rail: &gtk::Box) {
    let rail = rail.clone();
    crate::settings::wire_paned_persist(
        paned,
        false,
        clamp_rail_width,
        move |width| apply_rail_layout(&rail, width),
        set_rail_width,
    );
}

fn apply_close_to_tray(
    window: &adw::ApplicationWindow,
    tray: &mut Option<TrayHandle>,
    on_quit: std::sync::Arc<dyn Fn() + Send + Sync>,
    unread: u32,
) {
    let enabled = close_to_tray();
    window.set_hide_on_close(enabled);
    if enabled {
        if tray.is_none() {
            let on_quit = on_quit.clone();
            *tray = Some(start_tray(present_main_window, move || on_quit()));
        }
        if let Some(handle) = tray.as_ref() {
            set_tray_unread(handle, unread);
        }
    } else {
        stop_tray(tray);
    }
}

fn show_about_dialog(window: &adw::ApplicationWindow) {
    let about = adw::AboutDialog::builder()
        .application_name("Alcedo")
        .application_icon("dev.alcedo.Mail")
        .license_type(gtk::License::Gpl30)
        .license("GPL-3.0-or-later")
        .version(env!("CARGO_PKG_VERSION"))
        .build();
    about.present(Some(window));
}

const SHORTCUTS_CONTROLLER_NAME: &str = "alcedo-window-shortcuts";

fn install_layout_breakpoints(
    window: &adw::ApplicationWindow,
    app_sender: ComponentSender<AppModel>,
    mail_sender: relm4::Sender<MailMsg>,
) {
    use alcedo_core::layout_policy::{
        COMPACT_MAX_WIDTH_SP, MEDIUM_MAX_WIDTH_SP, NARROW_MAX_WIDTH_SP, WidthBreakpoints,
        adaptive_mail_layout,
    };

    let compact = Rc::new(Cell::new(false));
    let medium = Rc::new(Cell::new(false));
    let narrow = Rc::new(Cell::new(false));
    // Only one window breakpoint is active at a time, so crossing a limit
    // unapplies the wider breakpoint and then applies the narrower one. Report
    // once per idle so the mail panes are not reparented through the wide
    // layout in between.
    let scheduled = Rc::new(Cell::new(false));
    let report = Rc::new({
        let compact = compact.clone();
        let medium = medium.clone();
        let narrow = narrow.clone();
        let scheduled = scheduled.clone();
        move || {
            scheduled.set(false);
            let applied = WidthBreakpoints {
                compact: compact.get(),
                medium: medium.get(),
                narrow: narrow.get(),
            };
            app_sender.input(AppMsg::SetAdaptiveRail(
                adaptive_mail_layout(applied).hide_rail,
            ));
            mail_sender.emit(MailMsg::SetAdaptiveLayout(applied));
        }
    });
    let emit = Rc::new(move || {
        if scheduled.replace(true) {
            return;
        }
        let report = report.clone();
        glib::idle_add_local_once(move || report());
    });

    let add = |width: f64, flag: Rc<Cell<bool>>| {
        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            width,
            adw::LengthUnit::Sp,
        ));
        let flag_apply = flag.clone();
        let emit_apply = emit.clone();
        breakpoint.connect_apply(move |_| {
            flag_apply.set(true);
            emit_apply();
        });
        let emit_clear = emit.clone();
        breakpoint.connect_unapply(move |_| {
            flag.set(false);
            emit_clear();
        });
        window.add_breakpoint(breakpoint);
    };
    add(COMPACT_MAX_WIDTH_SP, compact);
    add(MEDIUM_MAX_WIDTH_SP, medium);
    add(NARROW_MAX_WIDTH_SP, narrow);
}

fn rebuild_window_shortcuts(
    window: &adw::ApplicationWindow,
    sender: ComponentSender<AppModel>,
    mail_sender: relm4::Sender<MailMsg>,
) {
    let model = window.observe_controllers();
    let mut to_remove = Vec::new();
    for index in 0..model.n_items() {
        let Some(item) = model.item(index) else {
            continue;
        };
        let Ok(controller) = item.downcast::<gtk::ShortcutController>() else {
            continue;
        };
        if controller.name().as_deref() == Some(SHORTCUTS_CONTROLLER_NAME) {
            to_remove.push(controller);
        }
    }
    for controller in to_remove {
        window.remove_controller(&controller);
    }
    setup_shortcuts(window, sender, mail_sender);
}

fn setup_shortcuts(
    window: &adw::ApplicationWindow,
    sender: ComponentSender<AppModel>,
    mail_sender: relm4::Sender<MailMsg>,
) {
    let controller = gtk::ShortcutController::new();
    controller.set_scope(gtk::ShortcutScope::Local);
    controller.set_name(Some(SHORTCUTS_CONTROLLER_NAME));

    let add_mail_shortcut =
        |trigger: &str, mail_sender: relm4::Sender<MailMsg>, msg: MailMsg, gate: ShortcutGate| {
            let shortcut = gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::CallbackAction::new(move |widget, _| {
                    if !shortcut_gate_allows(widget, gate) {
                        return glib::Propagation::Proceed;
                    }
                    mail_sender.emit(msg.clone());
                    glib::Propagation::Stop
                })),
            );
            controller.add_shortcut(shortcut);
        };

    for entry in command_catalog() {
        let Some(trigger) = effective_accel(entry.id) else {
            continue;
        };
        match entry.scope {
            ShortcutScope::ListFocus => {
                if let Some(gio_action) = entry.gio_action {
                    if entry.id == ACTION_COMMAND_PALETTE {
                        let app_sender = sender.clone();
                        let shortcut = gtk::Shortcut::new(
                            gtk::ShortcutTrigger::parse_string(&trigger),
                            Some(gtk::CallbackAction::new(move |widget, _| {
                                if !shortcut_gate_allows(widget, ShortcutGate::ListFocus) {
                                    return glib::Propagation::Proceed;
                                }
                                app_sender.input(AppMsg::OpenCommandPalette);
                                glib::Propagation::Stop
                            })),
                        );
                        controller.add_shortcut(shortcut);
                        continue;
                    }
                    if gio_action == ACTION_STAR {
                        let window = window.clone();
                        let shortcut = gtk::Shortcut::new(
                            gtk::ShortcutTrigger::parse_string(&trigger),
                            Some(gtk::CallbackAction::new(move |widget, _| {
                                if !shortcut_gate_allows(widget, ShortcutGate::ListFocus) {
                                    return glib::Propagation::Proceed;
                                }
                                toggle_star_action(&window);
                                glib::Propagation::Stop
                            })),
                        );
                        controller.add_shortcut(shortcut);
                        continue;
                    }
                    if gio_action == ACTION_REPLY_ALL {
                        let mail_sender = mail_sender.clone();
                        let shortcut = gtk::Shortcut::new(
                            gtk::ShortcutTrigger::parse_string(&trigger),
                            Some(gtk::CallbackAction::new(move |widget, _| {
                                if !shortcut_gate_allows(widget, ShortcutGate::ListFocus)
                                    || focus_in_visible_reader_key_bar(widget)
                                {
                                    return glib::Propagation::Proceed;
                                }
                                mail_sender.emit(MailMsg::ReplyAll);
                                glib::Propagation::Stop
                            })),
                        );
                        controller.add_shortcut(shortcut);
                        continue;
                    }
                    let mail_sender = mail_sender.clone();
                    let window = window.clone();
                    let shortcut = gtk::Shortcut::new(
                        gtk::ShortcutTrigger::parse_string(&trigger),
                        Some(gtk::CallbackAction::new(move |widget, _| {
                            if !shortcut_gate_allows(widget, ShortcutGate::ListFocus) {
                                return glib::Propagation::Proceed;
                            }
                            if let Some(msg) = gio_action_to_mail_msg(gio_action) {
                                mail_sender.emit(msg);
                            } else {
                                activate_gio_action(gio_action, &window);
                            }
                            glib::Propagation::Stop
                        })),
                    );
                    controller.add_shortcut(shortcut);
                }
            }
            ShortcutScope::PaletteChord => {
                let app_sender = sender.clone();
                let shortcut = gtk::Shortcut::new(
                    gtk::ShortcutTrigger::parse_string(&trigger),
                    Some(gtk::CallbackAction::new(move |widget, _| {
                        if is_palette_open() {
                            app_sender.input(AppMsg::OpenCommandPalette);
                            return glib::Propagation::Stop;
                        }
                        if !shortcut_gate_allows(widget, ShortcutGate::PaletteChord) {
                            return glib::Propagation::Proceed;
                        }
                        app_sender.input(AppMsg::OpenCommandPalette);
                        glib::Propagation::Stop
                    })),
                );
                controller.add_shortcut(shortcut);
            }
            ShortcutScope::WindowChord => {
                if let Some(gio_action) = entry.gio_action {
                    let gate = if gio_action == ACTION_SELECT_ALL {
                        ShortcutGate::WindowChord
                    } else {
                        ShortcutGate::WindowChordMail
                    };
                    let mail_sender = mail_sender.clone();
                    let window = window.clone();
                    let shortcut = gtk::Shortcut::new(
                        gtk::ShortcutTrigger::parse_string(&trigger),
                        Some(gtk::CallbackAction::new(move |widget, _| {
                            if !shortcut_gate_allows(widget, gate) {
                                return glib::Propagation::Proceed;
                            }
                            if let Some(msg) = gio_action_to_mail_msg(gio_action) {
                                mail_sender.emit(msg);
                            } else {
                                activate_gio_action(gio_action, &window);
                            }
                            glib::Propagation::Stop
                        })),
                    );
                    controller.add_shortcut(shortcut);
                }
            }
            ShortcutScope::AppAccel => {}
        }
    }

    add_mail_shortcut(
        "<Primary>Return",
        mail_sender.clone(),
        MailMsg::SendCompose,
        ShortcutGate::Never,
    );

    let escape = gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("Escape"),
        Some(gtk::CallbackAction::new(move |widget, _| {
            if is_palette_open() {
                sender.input(AppMsg::Escape);
                return glib::Propagation::Stop;
            }
            // AdwDialog (preferences, alerts, add-account) handles Escape itself.
            // Stopping here left the account editor with no way to go back.
            if window_has_foreign_dialog(widget) {
                return glib::Propagation::Proceed;
            }
            sender.input(AppMsg::Escape);
            glib::Propagation::Stop
        })),
    );
    controller.add_shortcut(escape);

    for trigger in ["Delete", "KP_Delete"] {
        let delete_sender = mail_sender.clone();
        let shortcut = gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string(trigger),
            Some(gtk::CallbackAction::new(move |widget, _| {
                if focus_blocks_list_keys(widget) {
                    return glib::Propagation::Proceed;
                }
                delete_sender.emit(MailMsg::DeleteSelected);
                glib::Propagation::Stop
            })),
        );
        controller.add_shortcut(shortcut);
    }

    let focus_sender = mail_sender.clone();
    let search_shortcut = gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("<Primary>f"),
        Some(gtk::CallbackAction::new(move |_, _| {
            focus_sender.emit(MailMsg::FocusSearch);
            glib::Propagation::Stop
        })),
    );
    controller.add_shortcut(search_shortcut);

    window.add_controller(controller);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShortcutGate {
    Never,
    ListFocus,
    WindowChord,
    WindowChordMail,
    PaletteChord,
}

fn shortcut_gate_allows(widget: &impl IsA<gtk::Widget>, gate: ShortcutGate) -> bool {
    match gate {
        ShortcutGate::Never => true,
        ShortcutGate::ListFocus => focus_is_message_list(widget) && !focus_blocks_list_keys(widget),
        ShortcutGate::WindowChord => !focus_blocks_list_keys(widget),
        ShortcutGate::WindowChordMail => {
            !focus_is_typed_input(widget) && !focus_is_compose_webview(widget)
        }
        ShortcutGate::PaletteChord => palette_chord_allows(
            focus_is_search_entry(widget),
            focus_is_typed_input(widget),
            focus_is_compose_webview(widget),
            window_has_foreign_dialog(widget),
        ),
    }
}

fn toggle_star_action(window: &adw::ApplicationWindow) {
    if let Some(action) = window.lookup_action("mail-star")
        && action.is_enabled()
    {
        action.activate(None);
        return;
    }
    if let Some(action) = window.lookup_action("mail-unstar")
        && action.is_enabled()
    {
        action.activate(None);
    }
}

fn gio_action_to_mail_msg(action: &str) -> Option<MailMsg> {
    use crate::menu::{
        ACTION_ARCHIVE, ACTION_CHOOSE_COPY_FOLDER, ACTION_CHOOSE_MOVE_FOLDER, ACTION_CREATE_RULE,
        ACTION_DELETE, ACTION_DELETE_PERMANENTLY, ACTION_FORWARD, ACTION_MARK_NOT_SPAM,
        ACTION_MARK_READ, ACTION_MARK_SPAM, ACTION_MARK_UNREAD, ACTION_REPLY, ACTION_REPLY_ALL,
        ACTION_SELECT_ALL, ACTION_SYNC_FOLDER,
    };
    Some(match action {
        ACTION_SELECT_ALL => MailMsg::SelectAllMessages,
        ACTION_REPLY => MailMsg::Reply,
        ACTION_REPLY_ALL => MailMsg::ReplyAll,
        ACTION_FORWARD => MailMsg::Forward,
        ACTION_MARK_READ => MailMsg::MarkSelectionRead,
        ACTION_MARK_UNREAD => MailMsg::MarkSelectionUnread,
        ACTION_MARK_SPAM => MailMsg::MarkSelectionSpam,
        ACTION_MARK_NOT_SPAM => MailMsg::MarkSelectionNotSpam,
        ACTION_CHOOSE_MOVE_FOLDER => MailMsg::ChooseMoveFolder,
        ACTION_CHOOSE_COPY_FOLDER => MailMsg::ChooseCopyFolder,
        ACTION_CREATE_RULE => MailMsg::CreateRuleFromMessage,
        ACTION_ARCHIVE => MailMsg::ArchiveSelected,
        ACTION_DELETE => MailMsg::DeleteSelected,
        ACTION_DELETE_PERMANENTLY => MailMsg::DeletePermanentlySelected,
        ACTION_SYNC_FOLDER => MailMsg::RequestSync,
        ACTION_GO_UNIFIED => MailMsg::SelectUnifiedInbox,
        _ => return None,
    })
}

fn activate_gio_action(action: &str, window: &adw::ApplicationWindow) {
    let Some((scope, name)) = action.split_once('.') else {
        return;
    };
    match scope {
        "win" => {
            if let Some(gio_action) = window.lookup_action(name) {
                gio_action.activate(None);
            }
        }
        "app" => {
            if let Some(app) = window.application()
                && let Some(gio_action) = app.lookup_action(name)
            {
                gio_action.activate(None);
            }
        }
        _ => {}
    }
}

fn setup_application_actions(
    gtk_app: &gtk::Application,
    window: &adw::ApplicationWindow,
    sender: &ComponentSender<AppModel>,
    mail_sender: relm4::Sender<MailMsg>,
    runtime: tokio::runtime::Handle,
) {
    let compose_action = gio::SimpleAction::new("compose", None);
    let compose_sender = sender.clone();
    compose_action.connect_activate(move |_, _| compose_sender.input(AppMsg::Compose));
    gtk_app.add_action(&compose_action);

    let add_account_action = gio::SimpleAction::new("add-account", None);
    let add_account_sender = mail_sender.clone();
    add_account_action.connect_activate(move |_, _| add_account_sender.emit(MailMsg::AddAccount));
    gtk_app.add_action(&add_account_action);

    let quit_action = gio::SimpleAction::new("quit", None);
    let quit_sender = sender.clone();
    quit_action.connect_activate(move |_, _| quit_sender.input(AppMsg::Quit));
    gtk_app.add_action(&quit_action);

    let show_window_action = gio::SimpleAction::new("show-window", None);
    show_window_action.connect_activate(move |_, _| present_main_window());
    gtk_app.add_action(&show_window_action);

    let open_message_action = gio::SimpleAction::new("open-message", Some(glib::VariantTy::INT64));
    let open_message_sender = mail_sender.clone();
    open_message_action.connect_activate(move |_, param| {
        present_main_window();
        let Some(raw) = param.and_then(|value| value.get::<i64>()) else {
            return;
        };
        open_message_sender.emit(MailMsg::OpenNotificationMessage(MessageId(raw)));
    });
    gtk_app.add_action(&open_message_action);

    let copy_verification_code_action =
        gio::SimpleAction::new("copy-verification-code", Some(glib::VariantTy::INT64));
    let copy_verification_code_sender = mail_sender.clone();
    copy_verification_code_action.connect_activate(move |_, param| {
        let Some(raw) = param.and_then(|value| value.get::<i64>()) else {
            return;
        };
        // Copy here so Wayland still has the notification's activation token.
        notifications::copy_pending_notification_otp(raw);
        copy_verification_code_sender.emit(MailMsg::CopyNotifiedVerificationCode(MessageId(raw)));
    });
    gtk_app.add_action(&copy_verification_code_action);

    let mark_notified_read_action =
        gio::SimpleAction::new("mark-notified-message-read", Some(glib::VariantTy::INT64));
    let mark_notified_read_sender = mail_sender.clone();
    mark_notified_read_action.connect_activate(move |_, param| {
        let Some(raw) = param.and_then(|value| value.get::<i64>()) else {
            return;
        };
        notifications::complete_background_activation();
        mark_notified_read_sender.emit(MailMsg::MarkNotifiedMessageRead(MessageId(raw)));
    });
    gtk_app.add_action(&mark_notified_read_action);

    let delete_notified_action =
        gio::SimpleAction::new("delete-notified-message", Some(glib::VariantTy::INT64));
    let delete_notified_sender = mail_sender.clone();
    delete_notified_action.connect_activate(move |_, param| {
        let Some(raw) = param.and_then(|value| value.get::<i64>()) else {
            return;
        };
        notifications::complete_background_activation();
        delete_notified_sender.emit(MailMsg::DeleteNotifiedMessage(MessageId(raw)));
    });
    gtk_app.add_action(&delete_notified_action);

    let preferences_action = gio::SimpleAction::new("preferences", None);
    let prefs_sender = sender.clone();
    preferences_action.connect_activate(move |_, _| prefs_sender.input(AppMsg::OpenPreferences));
    gtk_app.add_action(&preferences_action);

    let composing_prefs_action = gio::SimpleAction::new("preferences-composing", None);
    let composing_prefs_sender = sender.clone();
    composing_prefs_action.connect_activate(move |_, _| {
        composing_prefs_sender.input(AppMsg::OpenComposingPreferences)
    });
    gtk_app.add_action(&composing_prefs_action);

    let contacts_action = gio::SimpleAction::new("contacts", None);
    let contacts_sender = sender.clone();
    contacts_action.connect_activate(move |_, _| contacts_sender.input(AppMsg::OpenContacts));
    gtk_app.add_action(&contacts_action);

    let about_action = gio::SimpleAction::new("about", None);
    let about_window = window.clone();
    about_action.connect_activate(move |_, _| show_about_dialog(&about_window));
    gtk_app.add_action(&about_action);

    let shortcuts_action = gio::SimpleAction::new("keyboard-shortcuts", None);
    let shortcuts_window = window.clone();
    shortcuts_action.connect_activate(move |_, _| {
        crate::commands::present_shortcuts_dialog(&shortcuts_window);
    });
    gtk_app.add_action(&shortcuts_action);

    let help_action = gio::SimpleAction::new("help", None);
    let help_window = window.clone();
    let help_runtime = runtime.clone();
    help_action.connect_activate(move |_, _| {
        crate::help::present(&help_window, None, help_runtime.clone());
    });
    gtk_app.add_action(&help_action);

    let help_topic_action = gio::SimpleAction::new("help-topic", Some(glib::VariantTy::STRING));
    let help_topic_window = window.clone();
    let help_topic_runtime = runtime.clone();
    help_topic_action.connect_activate(move |_, param| {
        let topic = param.and_then(|value| value.get::<String>());
        crate::help::present(
            &help_topic_window,
            topic.as_deref(),
            help_topic_runtime.clone(),
        );
    });
    gtk_app.add_action(&help_topic_action);

    let preview_action = gio::SimpleAction::new_stateful(
        "toggle-preview",
        None,
        &glib::Variant::from(show_message_preview()),
    );
    let preview_action_for_settings = preview_action.clone();
    preview_action.connect_change_state(move |action, value| {
        let enabled = value
            .map(|variant| variant.get::<bool>().unwrap_or(true))
            .unwrap_or(true);
        set_show_message_preview(enabled);
        action.set_state(&glib::Variant::from(enabled));
    });
    gtk_app.add_action(&preview_action);

    on_show_message_preview_changed(move |enabled| {
        preview_action_for_settings.set_state(&glib::Variant::from(enabled));
    });

    let conversation_mode = message_list_mode() == MessageListMode::Conversation;
    let list_mode_action = gio::SimpleAction::new_stateful(
        "message-list-mode",
        None,
        &glib::Variant::from(conversation_mode),
    );
    let list_mode_action_for_settings = list_mode_action.clone();
    list_mode_action.connect_change_state(move |action, value| {
        let enabled = value
            .map(|variant| variant.get::<bool>().unwrap_or(true))
            .unwrap_or(true);
        set_message_list_mode(if enabled {
            MessageListMode::Conversation
        } else {
            MessageListMode::Flat
        });
        action.set_state(&glib::Variant::from(enabled));
    });
    gtk_app.add_action(&list_mode_action);

    on_message_list_mode_changed(move |mode| {
        list_mode_action_for_settings
            .set_state(&glib::Variant::from(mode == MessageListMode::Conversation));
    });

    let command_palette_action = gio::SimpleAction::new("command-palette", None);
    let palette_sender = sender.clone();
    command_palette_action
        .connect_activate(move |_, _| palette_sender.input(AppMsg::OpenCommandPalette));
    gtk_app.add_action(&command_palette_action);

    let show_mail_action = gio::SimpleAction::new("show-mail", None);
    let show_mail_sender = sender.clone();
    show_mail_action.connect_activate(move |_, _| show_mail_sender.input(AppMsg::ShowMail));
    gtk_app.add_action(&show_mail_action);

    let rail_action =
        gio::SimpleAction::new_stateful("toggle-rail", None, &glib::Variant::from(rail_visible()));
    let rail_sender = sender.clone();
    rail_action.connect_change_state(move |_, value| {
        let enabled = value
            .map(|variant| variant.get::<bool>().unwrap_or(true))
            .unwrap_or(true);
        rail_sender.input(AppMsg::SetRailVisible(enabled));
    });
    gtk_app.add_action(&rail_action);
}

fn palette_chord_allows(search: bool, typed: bool, compose_web: bool, dialog: bool) -> bool {
    !dialog && !compose_web && (search || !typed)
}

fn focus_is_search_entry(widget: &impl IsA<gtk::Widget>) -> bool {
    let Some(root) = widget.root() else {
        return false;
    };
    let Some(focus) = root.focus() else {
        return false;
    };
    focus.is::<gtk::SearchEntry>() || focus.ancestor(gtk::SearchEntry::static_type()).is_some()
}

fn focus_is_typed_input(widget: &impl IsA<gtk::Widget>) -> bool {
    let Some(root) = widget.root() else {
        return false;
    };
    let Some(focus) = root.focus() else {
        return false;
    };
    focus.is::<gtk::TextView>()
        || focus.dynamic_cast_ref::<gtk::Editable>().is_some()
        || focus.ancestor(gtk::TextView::static_type()).is_some()
}

fn focus_is_webview(widget: &impl IsA<gtk::Widget>) -> bool {
    let Some(root) = widget.root() else {
        return false;
    };
    let Some(focus) = root.focus() else {
        return false;
    };
    focus.is::<webkit6::WebView>() || focus.ancestor(webkit6::WebView::static_type()).is_some()
}

fn focus_is_compose_webview(widget: &impl IsA<gtk::Widget>) -> bool {
    if !focus_is_webview(widget) {
        return false;
    }
    let Some(root) = widget.root() else {
        return false;
    };
    let Some(focus) = root.focus() else {
        return false;
    };
    let mut current = Some(focus);
    while let Some(node) = current {
        if node.has_css_class("alcedo-composer") {
            return true;
        }
        current = node.parent();
    }
    false
}

fn focus_blocks_list_keys(widget: &impl IsA<gtk::Widget>) -> bool {
    focus_is_typed_input(widget) || focus_is_webview(widget)
}

fn focus_is_message_list(widget: &impl IsA<gtk::Widget>) -> bool {
    let Some(root) = widget.root() else {
        return false;
    };
    let Some(focus) = root.focus() else {
        return false;
    };
    focus
        .ancestor(gtk::ColumnView::static_type())
        .is_some_and(|view| view.has_css_class("alcedo-msg-list"))
}

/// A visible reader bar marked `alcedo-reader-key-bar` (added by an
/// extension) owns single-key shortcuts such as `a`, so list shortcuts yield.
fn focus_in_visible_reader_key_bar(widget: &impl IsA<gtk::Widget>) -> bool {
    fn walk(widget: &gtk::Widget, found: &mut bool) {
        if *found {
            return;
        }
        if widget.has_css_class("alcedo-reader-key-bar") && widget.is_visible() {
            *found = true;
            return;
        }
        let mut child = widget.first_child();
        while let Some(node) = child {
            walk(&node, found);
            child = node.next_sibling();
        }
    }

    let Some(root) = widget.root() else {
        return false;
    };
    let Some(root_widget) = root.dynamic_cast_ref::<gtk::Widget>() else {
        return false;
    };
    let mut found = false;
    walk(root_widget, &mut found);
    found
}

/// Present the main mail window (tray restore, dash activate, composer from a message window).
pub fn present_main_window() {
    if let Some(window) = main_window() {
        window.present();
    }
}

fn main_window() -> Option<gtk::Window> {
    let gtk_app = relm4::main_application();
    let windows = gtk_app.windows();
    windows
        .iter()
        .find(|window| window.widget_name().as_str() == MAIN_WINDOW_NAME)
        .cloned()
        .or_else(|| gtk_app.active_window())
        .or_else(|| windows.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::palette_chord_allows;

    #[test]
    fn palette_chord_opens_from_search() {
        assert!(palette_chord_allows(true, true, false, false));
    }

    #[test]
    fn palette_chord_blocked_in_compose_and_dialogs() {
        assert!(!palette_chord_allows(false, true, false, false));
        assert!(!palette_chord_allows(true, true, true, false));
        assert!(!palette_chord_allows(true, true, false, true));
    }

    #[test]
    fn palette_chord_opens_when_list_focused() {
        assert!(palette_chord_allows(false, false, false, false));
    }
}
