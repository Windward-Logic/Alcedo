//! Preferences, theme, Escape and the command palette.

use super::*;

impl MailModel {
    pub(super) fn update_chrome(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::RefreshPreferences => {
                if self.preferences.is_some() {
                    refresh_account_rows(
                        &self.preferences_ui().accounts,
                        &self.accounts,
                        &self.preferences_ui().dialog,
                        sender.input_sender(),
                        &self.runtime,
                        &self.auth_required,
                        &self.signatures,
                    );
                }
            }
            MailMsg::SetPreferences(preferences) => {
                self.preferences = Some(preferences);
                refresh_account_rows(
                    &self.preferences_ui().accounts,
                    &self.accounts,
                    &self.preferences_ui().dialog,
                    sender.input_sender(),
                    &self.runtime,
                    &self.auth_required,
                    &self.signatures,
                );
            }
            MailMsg::ApplyTheme => {
                let dark = adw::StyleManager::default().is_dark();
                self.composer.html_editor.apply_theme(dark);
                for window in self.composer_windows.values() {
                    window.widgets.html_editor.apply_theme(dark);
                }
                apply_reader_theme(&self.reader, dark);
                for window in self.message_windows.values() {
                    apply_reader_theme(&window.reader, dark);
                }
            }
            MailMsg::ReaderFontSizeChanged => {
                reload_reader_document(&self.reader);
                for window in self.message_windows.values() {
                    reload_reader_document(&window.reader);
                }
            }
            MailMsg::EditorFontSizeChanged => {
                let px = editor_font_size();
                self.composer.html_editor.apply_font_size(px);
                apply_compose_plain_spacing(&self.composer.body_view, px);
                for window in self.composer_windows.values() {
                    window.widgets.html_editor.apply_font_size(px);
                    apply_compose_plain_spacing(&window.widgets.body_view, px);
                }
            }
            MailMsg::VerificationCodesPreferenceChanged => {
                self.refill_open_readers();
            }
            MailMsg::Escape => {
                if self.mail_content_pane == MailContentPane::Composer {
                    sender.input(MailMsg::CancelCompose);
                } else if self.layout.single_pane && self.layout.single_show_detail {
                    self.back_to_messages();
                } else if let MailView::Extension(id) = self.mail_view
                    && self.is_search_backed(self.mail_view)
                {
                    if let Some(saved) = self
                        .extension_view_spec(id)
                        .and_then(|(_, spec)| spec.search_query)
                    {
                        let current = self.search_entry.text().to_string();
                        if current.trim() != saved.trim() {
                            self.search_entry.set_text(&saved);
                            sender.input(MailMsg::Search(saved));
                        }
                    }
                } else if !self.search_entry.text().is_empty() || self.mail_view == MailView::Search
                {
                    self.clear_search();
                    if self.mail_view == MailView::Search {
                        let restore = self
                            .pre_search_mail_view
                            .take()
                            .unwrap_or(MailView::UnifiedInbox);
                        self.mail_view = restore;
                        self.sync_column_layout();
                        match restore {
                            MailView::UnifiedInbox => sender.input(MailMsg::SelectUnifiedInbox),
                            MailView::Folder(folder_id) => {
                                sender.input(MailMsg::SelectFolder(folder_id))
                            }
                            MailView::Search => sender.input(MailMsg::SelectUnifiedInbox),
                            MailView::Extension(id) => sender.input(MailMsg::ShowExtensionView(id)),
                        }
                    }
                } else if selection_count(&self.list_view) > 1 {
                    if let Some(target) = self.selected {
                        let ids = selected_ids(&self.list_view);
                        if ids.contains(&target) {
                            select_ids(&self.list_view, &[target]);
                        } else {
                            self.list_view.selection_model.unselect_all();
                        }
                    } else {
                        self.list_view.selection_model.unselect_all();
                    }
                }
            }
            MailMsg::SetFoldersVisible(visible) => self.set_folders_visible(visible),
            MailMsg::SetAdaptiveLayout {
                max_width_860,
                max_width_560,
            } => self.apply_adaptive_layout(max_width_860, max_width_560),
            MailMsg::BackToMessages => self.back_to_messages(),
            MailMsg::ApplyTitleButtons => self.apply_title_buttons(),
            MailMsg::RefreshShortcutTooltips => self.refresh_shortcut_tooltips(),
            MailMsg::OpenCommandPalette => {
                let spec = self.command_palette_spec();
                let window = self.window.clone();
                let sender = sender.clone();
                let app_sender = self.msg_sender.clone();
                open_command_palette(&self.window, spec, move |activation| match activation {
                    PaletteActivation::Gio(action) => {
                        if let Some((scope, name)) = action.split_once('.') {
                            match scope {
                                "win" => {
                                    if let Some(a) = window.lookup_action(name) {
                                        a.activate(None)
                                    }
                                }
                                "app" => {
                                    if let Some(app) = window.application()
                                        && let Some(a) = app.lookup_action(name)
                                    {
                                        a.activate(None);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    PaletteActivation::GoFolder(folder_id) => {
                        sender.input(MailMsg::SelectFolder(folder_id));
                    }
                    PaletteActivation::ComposeTo(address) => {
                        app_sender.emit(crate::app::AppMsg::ComposeMailto(address));
                    }
                    PaletteActivation::InsertSearch(token) => {
                        sender.input(MailMsg::InsertSearchOperator(token));
                    }
                });
            }
            _ => unreachable!("message not routed to chrome"),
        }
    }

    pub(super) fn command_palette_spec(&self) -> CommandPaletteSpec {
        let folders = self
            .accounts
            .iter()
            .map(|account| {
                (
                    account.email.clone(),
                    self.folders.get(&account.id).cloned().unwrap_or_default(),
                )
            })
            .collect();
        let contacts = self
            .contact_book
            .borrow()
            .contacts
            .iter()
            .filter(|contact| contact.status == ContactStatus::Approved)
            .map(|contact| {
                let display = if contact.display_name.is_empty() {
                    contact.email.clone()
                } else {
                    contact.display_name.clone()
                };
                (contact.email.clone(), display)
            })
            .collect();
        CommandPaletteSpec { folders, contacts }
    }
}

pub(super) fn wire_folder_pane_persist(paned: &gtk::Paned) {
    let debounce = Rc::new(Cell::new(None::<glib::SourceId>));
    let clamping = Rc::new(Cell::new(false));
    paned.connect_notify_local(Some("position"), {
        let debounce = debounce.clone();
        let clamping = clamping.clone();
        move |paned, _| {
            if paned.start_child().is_none_or(|child| !child.is_visible()) {
                return;
            }
            let width = clamp_folder_pane_width(paned.position());
            if paned.position() != width {
                if !clamping.get() {
                    clamping.set(true);
                    paned.set_position(width);
                    clamping.set(false);
                }
                return;
            }
            crate::glib_source::cancel_source(&debounce);
            let debounce_cb = debounce.clone();
            let source_id = glib::timeout_add_local(Duration::from_millis(200), move || {
                let _ = debounce_cb.take();
                set_folder_pane_width(width);
                glib::ControlFlow::Break
            });
            debounce.set(Some(source_id));
        }
    });
}

pub(super) fn apply_reading_pane(paned: &gtk::Paned, pane: ReadingPane) {
    match pane {
        ReadingPane::Right => {
            paned.remove_css_class("alcedo-layout-bottom");
            paned.set_orientation(gtk::Orientation::Horizontal);
            paned.set_position(400);
            paned.set_resize_start_child(false);
            paned.set_resize_end_child(true);
        }
        ReadingPane::Bottom => {
            paned.add_css_class("alcedo-layout-bottom");
            paned.set_orientation(gtk::Orientation::Vertical);
            paned.set_position(380);
            paned.set_resize_start_child(false);
            paned.set_resize_end_child(true);
        }
    }
}
