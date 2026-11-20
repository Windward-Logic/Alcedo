//! Extension views, message actions and address actions in the mail window.

use std::rc::Rc;

use super::message_lookup::index_envelopes;
use super::*;
use crate::extension::{MailViewSpec, MessageSelection, UiExtension};

pub(super) use crate::extension::ViewLeave;

impl MailModel {
    pub(super) fn update_extension_views(
        &mut self,
        msg: MailMsg,
        sender: ComponentSender<MailModel>,
    ) {
        match msg {
            MailMsg::MessageAction(index) => {
                let Some(action) = crate::extension::message_actions().into_iter().nth(index)
                else {
                    return;
                };
                let ids = selected_ids(&self.list_view);
                let by_id = index_envelopes(&self.envelopes);
                let selected: Vec<&Envelope> =
                    ids.iter().filter_map(|id| by_id.get(id).copied()).collect();
                // A context menu opened earlier on a row no longer selected is stale.
                let context = self.context_message_id.filter(|id| ids.contains(id));
                let selection = self.message_selection(context, &selected);
                if selection.target().is_none() && selection.messages.is_empty() {
                    return;
                }
                (action.activate)(&self.ext_ctx, &selection);
            }
            MailMsg::AddressAction {
                index,
                account_id,
                email,
            } => {
                let account_id = self.resolve_address_account(account_id);
                self.run_address_action(index, account_id, email);
            }
            MailMsg::ComposerAddressAction {
                index,
                window_id,
                email,
            } => {
                let account_id = match window_id {
                    None => Some(self.composer.from_account_id()),
                    Some(id) => self
                        .composer_windows
                        .get(&id)
                        .map(|window| window.widgets.from_account_id()),
                }
                .filter(|id| self.accounts.iter().any(|account| account.id == *id));
                self.run_address_action(index, account_id, email);
            }
            MailMsg::ShowExtensionView(id) => self.show_extension_view(id, &sender),
            MailMsg::ExtensionViewsChanged => {
                if let Some(id) = self.pending_extension_restore
                    && self.extension_view_spec(id).is_some()
                {
                    self.pending_extension_restore = None;
                    self.show_extension_view(id, &sender);
                    return;
                }
                if let MailView::Extension(id) = self.mail_view
                    && self.extension_view_spec(id).is_none()
                {
                    sender.input(MailMsg::SelectUnifiedInbox);
                    return;
                }
                if self.is_search_backed(self.mail_view) {
                    self.sync_save_menu(&sender);
                }
                self.sync_extension_chrome();
                self.refresh_list();
                self.rebuild_folder_bar(&sender);
                self.refresh_status_bar();
            }
            MailMsg::ExtensionViewToggled(active) => {
                let MailView::Extension(id) = self.mail_view else {
                    return;
                };
                let Some((extension, spec)) = self.extension_view_spec(id) else {
                    return;
                };
                // Ignore the echo of `sync_extension_chrome` setting the state.
                if spec
                    .header_toggle
                    .is_some_and(|toggle| toggle.active != active)
                {
                    extension.header_toggled(id, active, &self.ext_ctx);
                }
            }
            MailMsg::SearchSaveAction(index) => {
                if let Some(item) = self.search_save_items.get(index) {
                    (item.activate)(&self.ext_ctx);
                }
            }
            MailMsg::FocusSearchEntry => {
                self.search_entry.grab_focus();
            }
            MailMsg::ExtensionToastWidget(toast) => self.toasts.add_toast(toast),
            MailMsg::ExtensionOpenDraft(draft) => {
                self.bind_composer_from(&self.composer, draft.account_id);
                apply_compose_draft(&self.composer, &draft, false);
                self.show_composer();
                present_main_window();
            }
            MailMsg::ExtensionNotify {
                id,
                title,
                body,
                open_message,
            } => {
                self.record(
                    activity_log::KIND_MAIL,
                    ActivityOutcome::Info,
                    title.clone(),
                    body.clone(),
                );
                if !notifications::is_attending_main(&self.window) {
                    notifications::send_named_notification(id, &title, &body, open_message);
                }
            }
            MailMsg::ExtensionToast(text) => {
                self.toasts.add_toast(crate::toast::plain(&text));
            }
            _ => unreachable!("message not routed to extension views"),
        }
    }

    fn run_address_action(&self, index: usize, account_id: Option<AccountId>, email: String) {
        if let Some(action) = crate::extension::address_actions().get(index) {
            (action.activate)(&self.ext_ctx, account_id, email);
        }
    }

    /// The account an address belongs to: the one it was seen in, or else the
    /// selected message's account.
    fn resolve_address_account(&self, account_id: Option<AccountId>) -> Option<AccountId> {
        account_id
            .filter(|id| self.accounts.iter().any(|account| account.id == *id))
            .or_else(|| {
                self.selected
                    .and_then(|message_id| self.envelope_for(message_id).map(|env| env.account_id))
            })
    }

    /// Whether `view` is an extension view filled by a saved search.
    pub(super) fn is_search_backed(&self, view: MailView) -> bool {
        match view {
            MailView::Extension(id) => self
                .extension_view_spec(id)
                .is_some_and(|(_, spec)| spec.search_query.is_some()),
            _ => false,
        }
    }

    /// Mail moved; let extensions refresh what depends on it.
    pub(super) fn notify_mail_changed(&self) {
        for extension in crate::extension::registered() {
            extension.mail_changed(&self.ext_ctx);
        }
    }

    /// Whether `view` behaves like the unified inbox (see `MailViewSpec`).
    pub(super) fn is_inbox_like(&self, view: MailView) -> bool {
        match view {
            MailView::UnifiedInbox => true,
            MailView::Extension(id) => self
                .extension_view_spec(id)
                .is_some_and(|(_, spec)| spec.inbox_like),
            _ => false,
        }
    }

    /// `view`, or the unified inbox when `view` is an inbox-like extension
    /// view, for code that treats both the same way.
    pub(super) fn as_inbox(&self, view: MailView) -> MailView {
        if self.is_inbox_like(view) {
            MailView::UnifiedInbox
        } else {
            view
        }
    }

    /// Tell extensions which single message the reader shows, if any.
    pub(super) fn notify_reader_selection(&self) {
        let message = if selection_count(&self.list_view) == 1 {
            self.reply_message_id()
                .and_then(|id| self.envelope_for(id))
                .cloned()
        } else {
            None
        };
        self.notify_reader_message(message.as_ref());
    }

    fn notify_reader_message(&self, message: Option<&Envelope>) {
        let view = match self.mail_view {
            MailView::Extension(id) => Some(id),
            _ => None,
        };
        for extension in crate::extension::registered() {
            extension.reader_message_changed(view, message, &self.ext_ctx);
        }
    }

    /// The shown view and `selected`, for message actions.
    fn message_selection(
        &self,
        context: Option<MessageId>,
        selected: &[&Envelope],
    ) -> MessageSelection {
        let roles: HashMap<FolderId, SpecialUse> = self
            .folders
            .values()
            .flatten()
            .filter(|folder| folder.special != SpecialUse::Other)
            .map(|folder| (folder.id, folder.special.clone()))
            .collect();
        let folder_roles = selected
            .iter()
            .filter_map(|env| Some((env.folder_id, roles.get(&env.folder_id)?.clone())))
            .collect();
        MessageSelection {
            view: self.shown_extension_view(),
            context,
            messages: selected.iter().map(|env| (*env).clone()).collect(),
            folder_roles,
        }
    }

    fn shown_extension_view(&self) -> Option<ExtensionListId> {
        match self.mail_view {
            MailView::Extension(id) => Some(id),
            _ => None,
        }
    }

    /// Enable each extension message action for `selected`.
    pub(super) fn sync_extension_actions(&self, selected: &[&Envelope]) {
        let actions = crate::extension::message_actions();
        // Skip copying the selection when no action depends on it.
        if actions.iter().all(|action| action.enabled.is_none()) {
            return;
        }
        let selection = self.message_selection(None, selected);
        for (action, gaction) in actions.iter().zip(&self.mail_actions.extension_actions) {
            let enabled = action
                .enabled
                .as_ref()
                .is_none_or(|enabled| enabled(&selection));
            gaction.set_enabled(enabled);
        }
    }

    /// Let the shown extension view handle activating `message`.
    pub(super) fn activate_in_extension_view(&self, message: MessageId) -> bool {
        let Some(id) = self.shown_extension_view() else {
            return false;
        };
        self.extension_view_spec(id)
            .is_some_and(|(extension, _)| extension.activate_in_view(id, message, &self.ext_ctx))
    }

    /// Whether Delete in the shown view belongs to its extension.
    pub(super) fn view_handles_delete(&self) -> bool {
        self.shown_extension_view()
            .and_then(|id| self.extension_view_spec(id))
            .is_some_and(|(_, spec)| spec.handles_delete)
    }

    /// Hand Delete of `ids` to the shown view's extension.
    pub(super) fn delete_in_extension_view(&mut self, ids: &[MessageId]) {
        let Some(id) = self.shown_extension_view() else {
            return;
        };
        let Some((extension, _)) = self.extension_view_spec(id) else {
            return;
        };
        extension.delete_in_view(id, ids, &self.ext_ctx);
        self.remove_envelopes_from_ui(ids, None);
        self.refresh_list();
    }

    /// The extension that owns view `id`, and that view's current spec.
    pub(super) fn extension_view_spec(
        &self,
        id: ExtensionListId,
    ) -> Option<(Rc<dyn UiExtension>, MailViewSpec)> {
        crate::extension::registered()
            .into_iter()
            .find_map(|extension| {
                let spec = extension
                    .mail_views()
                    .into_iter()
                    .find(|spec| spec.id == id)?;
                Some((extension, spec))
            })
    }

    /// Every extension view to list in the sidebar.
    pub(super) fn extension_views(&self) -> Vec<MailViewSpec> {
        crate::extension::registered()
            .iter()
            .flat_map(|extension| extension.mail_views())
            .collect()
    }

    /// Tell extensions the user is leaving their views. `opening` is the view
    /// about to be shown, if any; it is not told it was left, since an
    /// extension may have just primed it (see `views_to_notify`).
    pub(super) fn leave_extension_views(
        &mut self,
        leave: ViewLeave,
        opening: Option<ExtensionListId>,
    ) {
        let current = match self.mail_view {
            MailView::Extension(id) => Some(id),
            _ => None,
        };
        for extension in crate::extension::registered() {
            let ids: Vec<_> = extension.mail_views().iter().map(|spec| spec.id).collect();
            for id in views_to_notify(&ids, current, opening, leave) {
                extension.mail_view_left(id, leave, &self.ext_ctx);
            }
        }
    }

    fn show_extension_view(&mut self, id: ExtensionListId, sender: &ComponentSender<MailModel>) {
        let Some((extension, spec)) = self.extension_view_spec(id) else {
            sender.input(MailMsg::SelectUnifiedInbox);
            return;
        };
        if let Some(query) = spec.search_query {
            self.show_search_view(id, &query, sender);
            return;
        }
        self.clear_search();
        self.pending_account_folder_select = None;
        self.pending_folder_sync = None;
        self.pending_virtual_sync = None;
        if self.mail_view != MailView::Extension(id) {
            self.leave_extension_views(ViewLeave::OtherList, Some(id));
        }
        self.mail_view = MailView::Extension(id);
        self.sync_column_layout();
        self.reset_list_selection();
        self.conversations.clear();
        self.envelopes.clear();
        self.persist_current_folder();
        if self.mock {
            self.apply_mock_list();
        } else {
            extension.mail_view_opened(id, &self.ext_ctx);
        }
        self.sync_extension_chrome();
        self.refresh_list();
        self.rebuild_folder_bar(sender);
        self.refresh_status_bar();
    }

    /// Open a view filled by a saved search: put the query in the search bar
    /// and run it over all mail.
    fn show_search_view(
        &mut self,
        id: ExtensionListId,
        query: &str,
        sender: &ComponentSender<MailModel>,
    ) {
        self.pending_account_folder_select = None;
        self.pending_folder_sync = None;
        self.pending_virtual_sync = None;
        if self.mail_view != MailView::Extension(id) {
            self.leave_extension_views(ViewLeave::OtherList, Some(id));
        }
        self.pre_search_mail_view = None;
        self.mail_view = MailView::Extension(id);
        self.sync_column_layout();
        self.search_scope = SearchScopeKind::All;
        self.search_entry.set_text(query);
        set_last_query_for_toggles(query);
        self.persist_current_folder();
        self.reset_list_selection();
        if self.mock {
            let parsed = parse_search_query(query);
            self.envelopes = self
                .source_envelopes
                .iter()
                .filter(|env| self.mock_envelope_matches_search(env, &parsed, None))
                .cloned()
                .collect();
            self.envelopes.sort_by(|a, b| b.date.cmp(&a.date));
            self.conversations.clear();
            self.refresh_list();
        } else {
            self.cmd.emit(UiCommand::Search {
                query: query.to_string(),
                folder_ids: None,
                mode: self.list_mode,
            });
        }
        self.sync_search_chrome(query, Some(sender));
        self.sync_extension_chrome();
        self.rebuild_folder_bar(sender);
        self.refresh_status_bar();
    }

    /// Columns, sort and header toggle for the current view.
    pub(super) fn sync_extension_chrome(&mut self) {
        let spec = match self.mail_view {
            MailView::Extension(id) => self.extension_view_spec(id).map(|(_, spec)| spec),
            _ => None,
        };
        let show_folder_column =
            matches!(self.mail_view, MailView::Search | MailView::Extension(_));
        reapply_message_list_column_visibility(&self.list_view, show_folder_column);

        let newest_first = spec.as_ref().is_some_and(|spec| spec.sort_newest_first);
        if newest_first {
            apply_newest_first_sort(&self.list_view);
            self.newest_first_sort_on = true;
        } else if self.newest_first_sort_on {
            apply_saved_message_list_sort(&self.list_view);
            self.newest_first_sort_on = false;
        }
        self.sync_deleted_column();

        let toggle = spec.and_then(|spec| spec.header_toggle);
        self.view_toggle.set_visible(toggle.is_some());
        self.view_toggle.set_sensitive(toggle.is_some());
        if let Some(toggle) = toggle {
            self.view_toggle.set_tooltip_text(Some(&toggle.tooltip));
            self.view_toggle.set_active(toggle.active);
            if toggle.busy {
                self.view_toggle.add_css_class("alcedo-sync-spinning");
            } else {
                self.view_toggle.remove_css_class("alcedo-sync-spinning");
            }
        } else {
            self.view_toggle.remove_css_class("alcedo-sync-spinning");
        }
    }
}

/// Which of an extension's views to tell "left". A search only interrupts the
/// view being shown; the view being opened is never told it was left, or an
/// extension that set up state before `show_view` would lose it.
fn views_to_notify(
    ids: &[ExtensionListId],
    current: Option<ExtensionListId>,
    opening: Option<ExtensionListId>,
    leave: ViewLeave,
) -> Vec<ExtensionListId> {
    ids.iter()
        .copied()
        .filter(|id| Some(*id) != opening)
        .filter(|id| leave != ViewLeave::Search || current == Some(*id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: ExtensionListId = ExtensionListId(uuid::Uuid::from_u128(1));
    const B: ExtensionListId = ExtensionListId(uuid::Uuid::from_u128(2));

    #[test]
    fn opening_view_is_not_told_it_was_left() {
        let ids = [A, B];
        assert_eq!(
            views_to_notify(&ids, None, Some(A), ViewLeave::OtherList),
            vec![B]
        );
    }

    #[test]
    fn other_list_notifies_every_view_when_nothing_is_opening() {
        let ids = [A, B];
        assert_eq!(
            views_to_notify(&ids, None, None, ViewLeave::OtherList),
            vec![A, B]
        );
    }

    #[test]
    fn search_only_interrupts_the_current_view() {
        let ids = [A, B];
        assert_eq!(
            views_to_notify(&ids, Some(B), None, ViewLeave::Search),
            vec![B]
        );
    }
}
