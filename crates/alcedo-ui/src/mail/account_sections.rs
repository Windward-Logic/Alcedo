//! Stacked account sections: collapse, order, layout switch and header menu.

use std::rc::Rc;

use crate::folder_bar_accounts::{
    AccountHeaderActions, AccountSection, HiddenAccounts, focus_account_header, order_accounts,
    reorder_account_ids, uses_stacked_accounts,
};
use crate::settings::{set_account_order, set_collapsed_accounts, set_hidden_accounts};

use super::*;

impl MailModel {
    pub(super) fn update_account_sections(
        &mut self,
        msg: MailMsg,
        sender: ComponentSender<MailModel>,
    ) {
        match msg {
            MailMsg::ToggleAccountExpanded(account_id) => {
                if !self.collapsed_accounts.remove(&account_id) {
                    self.collapsed_accounts.insert(account_id);
                }
                set_collapsed_accounts(&self.collapsed_accounts);
                if self.selected_account != Some(account_id) {
                    self.make_account_current(account_id, &sender);
                }
                self.rebuild_folder_bar(&sender);
                focus_account_header(&self.folder_lists.middle, account_id);
            }
            MailMsg::ToggleHiddenAccountsExpanded => {
                self.hidden_accounts_expanded = !self.hidden_accounts_expanded;
                self.rebuild_folder_bar(&sender);
            }
            MailMsg::FolderContextToggleAccountHidden => {
                let Some(FolderMenuTarget::Account(account_id)) = self.folder_menu_target.take()
                else {
                    return;
                };
                self.toggle_account_hidden(account_id);
                self.rebuild_folder_bar(&sender);
            }
            MailMsg::ReorderAccounts(ids) => {
                set_account_order(&ids);
                self.account_order = ids;
                self.sync_account_switcher(&sender);
                self.rebuild_folder_bar(&sender);
            }
            MailMsg::FolderPaneAccountsChanged(mode) => {
                if self.folder_pane_accounts == mode {
                    return;
                }
                self.folder_pane_accounts = mode;
                self.sync_account_switcher(&sender);
                self.rebuild_folder_bar(&sender);
            }
            MailMsg::AccountHeaderContextMenu { account, x, y } => {
                self.popup_folder_context_menu(FolderMenuTarget::Account(account), x, y);
            }
            MailMsg::FolderContextAccountSettings => {
                let Some(FolderMenuTarget::Account(account_id)) = self.folder_menu_target.take()
                else {
                    return;
                };
                let Some(account) = self
                    .accounts
                    .iter()
                    .find(|account| account.id == account_id)
                    .cloned()
                else {
                    return;
                };
                self.open_account_editor(&account, &sender);
            }
            _ => unreachable!("message not routed to account sections"),
        }
    }

    pub(super) fn stacked_accounts_active(&self) -> bool {
        uses_stacked_accounts(self.folder_pane_accounts, self.accounts.len())
    }

    pub(super) fn ordered_accounts(&self) -> Vec<&Account> {
        order_accounts(&self.accounts, &self.account_order)
    }

    pub(super) fn account_sections(&self) -> Vec<AccountSection<'_>> {
        self.ordered_accounts()
            .into_iter()
            .filter(|account| !self.hidden_accounts.contains(&account.id))
            .map(|account| self.account_section(account))
            .collect()
    }

    pub(super) fn hidden_account_sections(&self) -> HiddenAccounts<'_> {
        HiddenAccounts {
            sections: self
                .ordered_accounts()
                .into_iter()
                .filter(|account| self.hidden_accounts.contains(&account.id))
                .map(|account| self.account_section(account))
                .collect(),
            expanded: self.hidden_accounts_expanded,
        }
    }

    fn account_section<'a>(&'a self, account: &'a Account) -> AccountSection<'a> {
        let folders = self
            .folders
            .get(&account.id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        AccountSection {
            account,
            expanded: !self.collapsed_accounts.contains(&account.id),
            folders,
            collapsed: self.collapsed_for_display(account.id, folders),
            personal_expanded: !self.collapsed_personal_folders.contains(&account.id),
            inbox_unread: self
                .unread_by_account
                .get(&account.id)
                .map_or(0, |(inbox, _)| *inbox),
            needs_sign_in: self.auth_required.contains_key(&account.id),
        }
    }

    fn toggle_account_hidden(&mut self, account_id: AccountId) {
        if !self.hidden_accounts.remove(&account_id) {
            self.hidden_accounts.insert(account_id);
        }
        if self.hidden_accounts.is_empty() {
            self.hidden_accounts_expanded = false;
        }
        set_hidden_accounts(&self.hidden_accounts);
    }

    pub(super) fn account_header_actions(
        &self,
        sender: &ComponentSender<MailModel>,
    ) -> AccountHeaderActions {
        let ordered_ids: Vec<AccountId> = self
            .ordered_accounts()
            .iter()
            .map(|account| account.id)
            .collect();
        AccountHeaderActions {
            on_toggle: Rc::new({
                let sender = sender.clone();
                move |account_id| sender.input(MailMsg::ToggleAccountExpanded(account_id))
            }),
            on_context: Rc::new({
                let sender = sender.clone();
                move |account, x, y| {
                    sender.input(MailMsg::AccountHeaderContextMenu { account, x, y })
                }
            }),
            on_reorder: Rc::new({
                let sender = sender.clone();
                move |dragged, target| {
                    if let Some(ids) = reorder_account_ids(&ordered_ids, dragged, target) {
                        sender.input(MailMsg::ReorderAccounts(ids));
                    }
                }
            }),
            on_add_account: Rc::new({
                let sender = sender.clone();
                move || sender.input(MailMsg::AddAccount)
            }),
            on_toggle_hidden_area: Rc::new({
                let sender = sender.clone();
                move || sender.input(MailMsg::ToggleHiddenAccountsExpanded)
            }),
        }
    }
}
