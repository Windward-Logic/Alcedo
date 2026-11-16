//! Message rules dialog and server-rule commands.

use super::*;

impl MailModel {
    pub(super) fn update_rules(&mut self, msg: MailMsg, sender: ComponentSender<MailModel>) {
        match msg {
            MailMsg::OpenRules => {
                if self.rules_dialog.is_some() {
                    if let Some(account) = self.rules_account() {
                        self.refresh_rules_dialog(account, &sender);
                    }
                } else {
                    self.open_rules_dialog(&sender);
                }
            }
            MailMsg::OpenRulesForAccount(account) => {
                if self.rules_dialog.is_some() {
                    self.refresh_rules_dialog(account, &sender);
                } else {
                    self.open_rules_dialog_for_account(account, &sender);
                }
            }
            MailMsg::CreateRuleFromMessage => {
                self.create_rule_from_message(&sender);
            }
            MailMsg::UpsertMessageRule { rule, run_folder } => {
                self.cmd.emit(UiCommand::UpsertMessageRule(rule.clone()));
                if let Some(folder) = run_folder {
                    self.cmd.emit(UiCommand::RunMessageRules {
                        account: rule.account_id,
                        folder,
                        rule_ids: Some(vec![rule.id]),
                    });
                }
            }
            MailMsg::DeleteMessageRule(id) => {
                self.cmd.emit(UiCommand::DeleteMessageRule { id });
            }
            MailMsg::ReorderMessageRules { account, ids } => {
                if let Some(rules) = self.rules.get_mut(&account) {
                    let id_set: HashSet<RuleId> = ids.iter().copied().collect();
                    let mut reordered = Vec::new();
                    for id in &ids {
                        if let Some(mut rule) = rules.iter().find(|rule| rule.id == *id).cloned() {
                            rule.position = i32::try_from(reordered.len()).unwrap_or(i32::MAX);
                            reordered.push(rule);
                        }
                    }
                    for rule in rules.iter() {
                        if !id_set.contains(&rule.id) {
                            let mut rule = rule.clone();
                            rule.position = i32::try_from(reordered.len()).unwrap_or(i32::MAX);
                            reordered.push(rule);
                        }
                    }
                    *rules = reordered;
                }
                self.cmd
                    .emit(UiCommand::ReorderMessageRules { account, ids });
            }
            MailMsg::RunMessageRules { folder, rule_ids } => {
                let account = self
                    .folder_for_id(folder)
                    .map(|folder| folder.account_id)
                    .or(self.selected_account);
                if let Some(account) = account {
                    self.cmd.emit(UiCommand::RunMessageRules {
                        account,
                        folder,
                        rule_ids,
                    });
                }
            }
            MailMsg::AdoptServerRules(account) => {
                self.cmd.emit(UiCommand::AdoptServerRules(account));
            }
            MailMsg::RulesDialogClosed => {
                self.rules_dialog = None;
            }
            _ => unreachable!("message not routed to rules"),
        }
    }

    pub(super) fn rules_account(&self) -> Option<AccountId> {
        self.selected_account
            .or_else(|| self.accounts.first().map(|a| a.id))
    }

    pub(super) fn rules_dialog_spec(&self, account: AccountId) -> RulesDialogSpec {
        RulesDialogSpec {
            accounts: self.accounts.clone(),
            selected: account,
            folders: self.folders.clone(),
            rules: self.rules.get(&account).cloned().unwrap_or_default(),
            server_rules: self
                .server_rules
                .get(&account)
                .cloned()
                .unwrap_or(ServerRulesStatus::Unknown),
            preview: self.preview.get(&account).and_then(|p| p.clone()),
        }
    }

    pub(super) fn open_rules_dialog(&mut self, sender: &ComponentSender<MailModel>) {
        let account = self
            .rules_account()
            .unwrap_or_else(|| AccountId(uuid::Uuid::nil()));
        self.open_rules_dialog_for_account(account, sender);
    }

    pub(super) fn open_rules_dialog_for_account(
        &mut self,
        account: AccountId,
        sender: &ComponentSender<MailModel>,
    ) {
        let spec = self.rules_dialog_spec(account);
        let handle = open_rules_dialog(&self.window, spec, sender.input_sender().clone());
        let sender_closed = sender.input_sender().clone();
        handle.dialog.connect_closed(move |_| {
            sender_closed.emit(MailMsg::RulesDialogClosed);
        });
        self.rules_dialog = Some(handle);
    }

    pub(super) fn refresh_rules_dialog(
        &mut self,
        account: AccountId,
        sender: &ComponentSender<MailModel>,
    ) {
        let spec = self.rules_dialog_spec(account);
        let window = self.window.upcast_ref();
        if let Some(handle) = self.rules_dialog.as_mut() {
            refresh_rules_dialog(handle, &spec, sender.input_sender(), window);
        }
    }

    pub(super) fn create_rule_from_message(&self, sender: &ComponentSender<MailModel>) {
        let ids = selected_ids(&self.list_view);
        if ids.is_empty() {
            return;
        }
        let message_id = ids[0];
        let envelope = self
            .envelopes
            .iter()
            .find(|env| env.id == message_id)
            .cloned();
        let Some(envelope) = envelope else {
            return;
        };
        let account_id = envelope.account_id;
        let from = envelope.from.first().cloned();
        let display_name = from
            .as_ref()
            .and_then(|addr| addr.name.as_deref())
            .filter(|name| !name.is_empty())
            .or_else(|| from.as_ref().map(|addr| addr.email.as_str()))
            .unwrap_or("sender")
            .to_string();
        let email = from.map(|addr| addr.email).unwrap_or_default();
        let run_folder = Some(envelope.folder_id);
        let rule = MessageRule {
            id: RuleId(uuid::Uuid::new_v4()),
            account_id,
            name: format!("Mail from {display_name}"),
            enabled: true,
            position: -1,
            match_mode: RuleMatchMode::All,
            stop_processing: true,
            conditions: vec![RuleCondition {
                field: RuleField::From,
                op: RuleOp::Is,
                value: email,
            }],
            actions: vec![RuleAction::MoveToFolder(FolderId(0))],
            execution: alcedo_core::RuleExecution::Client,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let account_email = self.account_email(account_id);
        let folders = self.folders.get(&account_id).cloned().unwrap_or_default();
        let sender_input = sender.input_sender().clone();
        open_rule_editor(
            &self.window,
            RuleEditorSpec {
                rule,
                account_email,
                folders,
                is_new: true,
                show_run_checkbox: true,
            },
            move |saved, also_run| {
                let run = if also_run { run_folder } else { None };
                sender_input.emit(MailMsg::UpsertMessageRule {
                    rule: saved,
                    run_folder: run,
                });
            },
        );
    }
}
