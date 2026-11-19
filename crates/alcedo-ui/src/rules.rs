//! Message rules list dialog.

use alcedo_i18n::{ntr, tr, trf, trs};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use alcedo_core::{
    Account, AccountId, Folder, FolderId, MessageRule, RuleExecution, RuleId, ServerRuleView,
    ServerRulesPreview, ServerRulesStatus, rule_summary, server_rule_summary,
};
use relm4::Sender;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use relm4::gtk::gdk;
use relm4::gtk::glib;

use crate::folder_picker::{FolderPickerMode, FolderPickerSpec, open_folder_picker};
use crate::help::{self, TOPIC_RULES};
use crate::icons::{self, RULES_SYMBOLIC, TOOLBAR_PIXEL_SIZE};
use crate::mail::MailMsg;
use crate::rule_editor::{RuleEditorSpec, new_rule_for_account, open_rule_editor};

const RULE_DRAG_PREFIX: &str = "alcedo-rule:";

#[derive(Clone, Debug)]
pub struct RulesDialogSpec {
    pub accounts: Vec<Account>,
    pub selected: AccountId,
    pub folders: HashMap<AccountId, Vec<Folder>>,
    pub rules: Vec<MessageRule>,
    pub server_rules: ServerRulesStatus,
    pub preview: Option<ServerRulesPreview>,
}

/// Open dialog widgets kept in [`crate::mail::MailModel`].
pub struct RulesDialogHandle {
    pub dialog: adw::Dialog,
    pub content_host: gtk::Box,
    pub account: AccountId,
    pub listbox: gtk::ListBox,
    list_scroll: gtk::ScrolledWindow,
    pub toolbar_edit: gtk::Button,
    pub toolbar_delete: gtk::Button,
    pub toolbar_up: gtk::Button,
    pub toolbar_down: gtk::Button,
    pub toolbar_run: gtk::Button,
    pub toolbar_add: gtk::Button,
    switch_sender: Rc<RefCell<Option<Sender<MailMsg>>>>,
    current_rules: Rc<RefCell<Vec<MessageRule>>>,
    current_account: Rc<RefCell<AccountId>>,
}

pub fn open_rules_dialog(
    parent: &impl IsA<gtk::Widget>,
    spec: RulesDialogSpec,
    sender: Sender<MailMsg>,
) -> RulesDialogHandle {
    let close_btn = gtk::Button::builder().label(tr!("Close")).build();

    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let title_label = gtk::Label::builder()
        .label(tr!("Message Rules"))
        .css_classes(["title"])
        .build();
    header.set_title_widget(Some(&title_label));
    header.pack_start(&close_btn);
    header.pack_end(&help::help_button(TOPIC_RULES));

    let content_host = gtk::Box::new(gtk::Orientation::Vertical, 12);

    let listbox = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .css_classes(["boxed-list"])
        .activate_on_single_click(false)
        .build();
    listbox.update_property(&[gtk::accessible::Property::Label(&tr!("Message rules"))]);

    let list_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .hexpand(true)
        .child(&listbox)
        .build();

    let toolbar_edit = flat_toolbar_button("document-edit-symbolic", &tr!("Edit Rule"), false);
    let toolbar_delete = flat_toolbar_button("user-trash-symbolic", &tr!("Delete Rule"), false);
    let toolbar_up = flat_toolbar_button("go-up-symbolic", &tr!("Move Up"), false);
    let toolbar_down = flat_toolbar_button("go-down-symbolic", &tr!("Move Down"), false);
    let toolbar_run = flat_toolbar_button(RULES_SYMBOLIC, &tr!("Run Rules"), true);
    let toolbar_add = flat_toolbar_button("list-add-symbolic", &tr!("Add Rule"), false);

    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    toolbar.append(&toolbar_add);
    toolbar.append(&toolbar_edit);
    toolbar.append(&toolbar_delete);
    toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    toolbar.append(&toolbar_up);
    toolbar.append(&toolbar_down);
    toolbar.append(&toolbar_run);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(18);
    body.set_margin_bottom(18);
    body.set_margin_start(18);
    body.set_margin_end(18);
    body.append(&content_host);
    body.append(&list_scroll);
    body.append(&toolbar);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&body));

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&toast_overlay));

    let dialog = adw::Dialog::builder()
        .title(tr!("Message Rules"))
        .content_width(640)
        .content_height(620)
        .child(&toolbar_view)
        .build();

    let account = spec.selected;
    let mut handle = RulesDialogHandle {
        dialog: dialog.clone(),
        content_host: content_host.clone(),
        account,
        listbox: listbox.clone(),
        list_scroll: list_scroll.clone(),
        toolbar_edit: toolbar_edit.clone(),
        toolbar_delete: toolbar_delete.clone(),
        toolbar_up: toolbar_up.clone(),
        toolbar_down: toolbar_down.clone(),
        toolbar_run: toolbar_run.clone(),
        toolbar_add: toolbar_add.clone(),
        switch_sender: Rc::new(RefCell::new(Some(sender.clone()))),
        current_rules: Rc::new(RefCell::new(spec.rules.clone())),
        current_account: Rc::new(RefCell::new(account)),
    };

    wire_toolbar_actions(&handle, &spec, &sender, parent.as_ref());
    refresh_rules_dialog(&mut handle, &spec, &sender, parent.as_ref());

    close_btn.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });

    dialog.present(Some(parent.as_ref()));
    handle
}

pub fn refresh_rules_dialog(
    handle: &mut RulesDialogHandle,
    spec: &RulesDialogSpec,
    sender: &Sender<MailMsg>,
    parent: &gtk::Widget,
) {
    handle.account = spec.selected;
    *handle.current_account.borrow_mut() = spec.selected;
    *handle.current_rules.borrow_mut() = spec.rules.clone();
    *handle.switch_sender.borrow_mut() = Some(sender.clone());

    while let Some(child) = handle.content_host.first_child() {
        handle.content_host.remove(&child);
    }

    if spec.accounts.is_empty() {
        let add_account = suggested_cta(&tr!("Add Account"));
        let sender_add = sender.clone();
        add_account.connect_clicked(move |_| {
            sender_add.send(MailMsg::AddAccount).ok();
        });
        let page = adw::StatusPage::builder()
            .title(tr!("Message Rules"))
            .description(tr!("Add an account to create rules."))
            .child(&add_account)
            .build();
        handle.content_host.append(&page);
        set_local_list_visible(handle, false);
        handle.toolbar_add.set_visible(false);
        sync_toolbar_state(handle, &spec.rules);
        return;
    }

    handle.toolbar_add.set_visible(true);

    if spec.accounts.len() > 1 {
        let account_labels: Vec<String> = spec
            .accounts
            .iter()
            .map(|a| a.label().to_string())
            .collect();
        let account_model = gtk::StringList::new(
            &account_labels
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        let selected_idx = spec
            .accounts
            .iter()
            .position(|a| a.id == spec.selected)
            .unwrap_or(0) as u32;
        let account_row = adw::ComboRow::builder()
            .title(tr!("Account"))
            .model(&account_model)
            .expression(gtk::PropertyExpression::new(
                gtk::StringObject::static_type(),
                None::<&gtk::Expression>,
                "string",
            ))
            .selected(selected_idx)
            .build();
        account_row.add_css_class("alcedo-rules-account-row");
        expand_account_combo(&account_row);
        let sender_combo = sender.clone();
        let accounts_combo = spec.accounts.clone();
        account_row.connect_notify_local(Some("selected"), move |row, _| {
            let idx = row.selected() as usize;
            if let Some(account) = accounts_combo.get(idx) {
                sender_combo.emit(MailMsg::OpenRulesForAccount(account.id));
            }
        });
        let account_group = adw::PreferencesGroup::new();
        account_group.add(&account_row);
        handle.content_host.append(&account_group);
    }

    let banner_label = gtk::Label::builder()
        .label(banner_message(&spec.server_rules))
        .css_classes(["dim-label"])
        .wrap(true)
        .xalign(0.0)
        .build();
    let banner_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    banner_box.append(&banner_label);

    if matches!(spec.server_rules, ServerRulesStatus::ForeignActive { .. }) {
        let adopt_btn = gtk::Button::builder()
            .label(tr!("Use server instead…"))
            .css_classes(["flat"])
            .halign(gtk::Align::Start)
            .build();
        let sender_adopt = sender.clone();
        let account_adopt = spec.selected;
        adopt_btn.connect_clicked(move |_| {
            let confirm = adw::AlertDialog::builder()
                .heading(tr!("Replace server filters?"))
                .body(
                    tr!("Alcedo will turn on its own server rules. Existing server filters stay on the server but will not run."),
                )
                .build();
            confirm.add_response("cancel", &tr!("Cancel"));
            confirm.add_response("replace", &tr!("Replace"));
            confirm.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
            confirm.set_close_response("cancel");
            let sender_adopt = sender_adopt.clone();
            confirm.choose(
                None::<&gtk::Widget>,
                None::<&gtk::gio::Cancellable>,
                move |response| {
                    if response.as_str() == "replace" {
                        sender_adopt.emit(MailMsg::AdoptServerRules(account_adopt));
                    }
                },
            );
        });
        banner_box.append(&adopt_btn);
    }
    handle.content_host.append(&banner_box);

    let show_server = spec
        .preview
        .as_ref()
        .is_some_and(|p| !p.rules.is_empty() || p.unsupported_count > 0);
    if show_server {
        let preview = spec.preview.as_ref().expect("preview checked");
        let server_group = adw::PreferencesGroup::builder()
            .title(tr!("On the server"))
            .description(tr!(
                "Already running on this account. Alcedo cannot edit them here."
            ))
            .build();
        let server_list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        server_list.update_property(&[gtk::accessible::Property::Label(&tr!("Server rules"))]);

        let folders = spec
            .folders
            .get(&spec.selected)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        for rule in &preview.rules {
            server_list.append(&server_rule_row(rule, folders));
        }
        if preview.unsupported_count > 0 {
            let n = preview.unsupported_count;
            let subtitle = ntr!(
                "{n} more filter Alcedo can’t display",
                "{n} more filters Alcedo can’t display",
                n
            );
            let remainder = adw::ActionRow::builder()
                .title(tr!("Other server filters"))
                .subtitle(&subtitle)
                .activatable(false)
                .css_classes(["alcedo-rule-row", "alcedo-rule-row-server"])
                .build();
            remainder.update_property(&[gtk::accessible::Property::Label(&tr!(
                "Other server filters"
            ))]);
            let info = gtk::Image::from_icon_name("dialog-information-symbolic");
            remainder.add_suffix(&info);
            server_list.append(&remainder);
        }
        server_group.add(&server_list);
        handle.content_host.append(&server_group);
    }

    while let Some(row) = handle.listbox.row_at_index(0) {
        handle.listbox.remove(&row);
    }

    let local_empty = spec.rules.is_empty();
    let show_local_empty_label = local_empty && show_server;
    if show_local_empty_label {
        let none_label = gtk::Label::builder()
            .label(tr!("None on this computer"))
            .css_classes(["dim-label"])
            .halign(gtk::Align::Start)
            .build();
        handle.content_host.append(&none_label);
    }

    if local_empty && !show_server {
        let add_rule = suggested_cta(&tr!("Add Rule"));
        let sender_empty = sender.clone();
        let parent_empty = parent.clone();
        let folders_empty = spec.folders.clone();
        let accounts_empty = spec.accounts.clone();
        let account_empty = spec.selected;
        add_rule.connect_clicked(move |_| {
            open_new_rule_editor(
                &parent_empty,
                account_empty,
                &accounts_empty,
                &folders_empty,
                sender_empty.clone(),
                false,
                None,
            );
        });
        let page = adw::StatusPage::builder()
            .title(tr!("No message rules"))
            .description(tr!(
                "Rules file, star, or delete mail automatically when it arrives."
            ))
            .child(&add_rule)
            .build();
        page.set_paintable(icons::paintable(RULES_SYMBOLIC, 128).as_ref());
        page.set_vexpand(true);
        page.add_css_class("alcedo-rules-empty");
        handle.content_host.append(&page);
        set_local_list_visible(handle, false);
    } else if local_empty {
        set_local_list_visible(handle, false);
    } else {
        set_local_list_visible(handle, true);
        let folders = spec
            .folders
            .get(&spec.selected)
            .cloned()
            .unwrap_or_default();
        let switch_sender = handle.switch_sender.clone();
        let current_rules = handle.current_rules.clone();
        let on_reorder = {
            let listbox = handle.listbox.clone();
            let current_rules = current_rules.clone();
            let current_account = handle.current_account.clone();
            let sender_reorder = sender.clone();
            let toolbar_edit = handle.toolbar_edit.clone();
            let toolbar_delete = handle.toolbar_delete.clone();
            let toolbar_up = handle.toolbar_up.clone();
            let toolbar_down = handle.toolbar_down.clone();
            let toolbar_run = handle.toolbar_run.clone();
            Rc::new(move |dragged, target| {
                let mut ids: Vec<RuleId> = current_rules.borrow().iter().map(|r| r.id).collect();
                let before = ids.clone();
                if !reorder_ids_insert_before(&mut ids, dragged, target) || ids == before {
                    return;
                }
                apply_local_rule_order(&listbox, &current_rules, &ids);
                sync_rules_toolbar(
                    &listbox,
                    &current_rules,
                    &toolbar_edit,
                    &toolbar_delete,
                    &toolbar_up,
                    &toolbar_down,
                    &toolbar_run,
                );
                sender_reorder.emit(MailMsg::ReorderMessageRules {
                    account: *current_account.borrow(),
                    ids,
                });
            })
        };
        for rule in &spec.rules {
            let row = local_rule_row(
                rule,
                &folders,
                &spec.server_rules,
                &spec.rules,
                current_rules.clone(),
                switch_sender.clone(),
                on_reorder.clone(),
            );
            handle.listbox.append(&row);
        }
    }

    sync_toolbar_state(handle, &spec.rules);
}

fn wire_toolbar_actions(
    handle: &RulesDialogHandle,
    spec: &RulesDialogSpec,
    sender: &Sender<MailMsg>,
    parent: &gtk::Widget,
) {
    let listbox = handle.listbox.clone();
    let current_rules = handle.current_rules.clone();
    let current_account = handle.current_account.clone();

    let sync_toolbar = {
        let listbox = listbox.clone();
        let toolbar_edit = handle.toolbar_edit.clone();
        let toolbar_delete = handle.toolbar_delete.clone();
        let toolbar_up = handle.toolbar_up.clone();
        let toolbar_down = handle.toolbar_down.clone();
        let toolbar_run = handle.toolbar_run.clone();
        let current_rules = current_rules.clone();
        Rc::new(move || {
            sync_rules_toolbar(
                &listbox,
                &current_rules,
                &toolbar_edit,
                &toolbar_delete,
                &toolbar_up,
                &toolbar_down,
                &toolbar_run,
            );
        })
    };

    let sync_toolbar_signal = sync_toolbar.clone();
    listbox.connect_row_selected(move |_, _| sync_toolbar_signal());
    sync_toolbar();

    let sender_add = sender.clone();
    let parent_add = parent.clone();
    let folders_add = spec.folders.clone();
    let accounts_add = spec.accounts.clone();
    let current_account_add = handle.current_account.clone();
    handle.toolbar_add.connect_clicked(move |_| {
        open_new_rule_editor(
            &parent_add,
            *current_account_add.borrow(),
            &accounts_add,
            &folders_add,
            sender_add.clone(),
            false,
            None,
        );
    });

    let sender_edit = sender.clone();
    let parent_edit = parent.clone();
    let folders_edit = spec.folders.clone();
    let accounts_edit = spec.accounts.clone();
    let current_rules_edit = current_rules.clone();
    let listbox_edit = handle.listbox.clone();
    handle.toolbar_edit.connect_clicked(move |_| {
        let idx = listbox_edit.selected_row().map(|row| row.index() as usize);
        if let Some(idx) = idx
            && let Some(rule) = current_rules_edit.borrow().get(idx)
        {
            open_existing_rule_editor(
                &parent_edit,
                rule,
                &accounts_edit,
                &folders_edit,
                sender_edit.clone(),
                false,
                None,
            );
        }
    });

    listbox.connect_row_activated({
        let sender_edit = sender.clone();
        let parent_edit = parent.clone();
        let folders_edit = spec.folders.clone();
        let accounts_edit = spec.accounts.clone();
        let current_rules_activate = current_rules.clone();
        move |_, row| {
            let idx = row.index() as usize;
            if let Some(rule) = current_rules_activate.borrow().get(idx) {
                open_existing_rule_editor(
                    &parent_edit,
                    rule,
                    &accounts_edit,
                    &folders_edit,
                    sender_edit.clone(),
                    false,
                    None,
                );
            }
        }
    });

    let sender_delete = sender.clone();
    let current_rules_delete = current_rules.clone();
    let listbox_delete = handle.listbox.clone();
    handle.toolbar_delete.connect_clicked(move |_| {
        let idx = listbox_delete
            .selected_row()
            .map(|row| row.index() as usize);
        if let Some(idx) = idx
            && let Some(rule) = current_rules_delete.borrow().get(idx)
        {
            let name = rule.name.clone();
            let id = rule.id;
            let confirm = adw::AlertDialog::builder()
                .heading(tr!("Delete rule?"))
                .body(trf!(
                    "“{name}” will no longer apply to incoming mail.",
                    name = name
                ))
                .build();
            confirm.add_response("cancel", &tr!("Cancel"));
            confirm.add_response("delete", &tr!("Delete"));
            confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
            confirm.set_close_response("cancel");
            let sender_delete = sender_delete.clone();
            confirm.choose(
                None::<&gtk::Widget>,
                None::<&gtk::gio::Cancellable>,
                move |response| {
                    if response.as_str() == "delete" {
                        sender_delete.emit(MailMsg::DeleteMessageRule(id));
                    }
                },
            );
        }
    });

    let sender_reorder = sender.clone();
    let current_rules_reorder = current_rules.clone();
    let current_account_reorder = current_account.clone();
    let listbox_up = handle.listbox.clone();
    let toolbar_edit_up = handle.toolbar_edit.clone();
    let toolbar_delete_up = handle.toolbar_delete.clone();
    let toolbar_up_btn = handle.toolbar_up.clone();
    let toolbar_down_up = handle.toolbar_down.clone();
    let toolbar_run_up = handle.toolbar_run.clone();
    handle.toolbar_up.connect_clicked(move |_| {
        let idx = listbox_up.selected_row().map(|row| row.index() as usize);
        if let Some(idx) = idx
            && idx > 0
        {
            let mut ids: Vec<RuleId> = current_rules_reorder
                .borrow()
                .iter()
                .map(|r| r.id)
                .collect();
            ids.swap(idx, idx - 1);
            apply_local_rule_order(&listbox_up, &current_rules_reorder, &ids);
            sync_rules_toolbar(
                &listbox_up,
                &current_rules_reorder,
                &toolbar_edit_up,
                &toolbar_delete_up,
                &toolbar_up_btn,
                &toolbar_down_up,
                &toolbar_run_up,
            );
            sender_reorder.emit(MailMsg::ReorderMessageRules {
                account: *current_account_reorder.borrow(),
                ids,
            });
        }
    });

    let sender_reorder_down = sender.clone();
    let current_rules_reorder_down = current_rules.clone();
    let current_account_reorder_down = current_account.clone();
    let listbox_down = handle.listbox.clone();
    let toolbar_edit_down = handle.toolbar_edit.clone();
    let toolbar_delete_down = handle.toolbar_delete.clone();
    let toolbar_up_down = handle.toolbar_up.clone();
    let toolbar_down_btn = handle.toolbar_down.clone();
    let toolbar_run_down = handle.toolbar_run.clone();
    handle.toolbar_down.connect_clicked(move |_| {
        let idx = listbox_down.selected_row().map(|row| row.index() as usize);
        if let Some(idx) = idx {
            let rules_len = current_rules_reorder_down.borrow().len();
            if idx + 1 < rules_len {
                let mut ids: Vec<RuleId> = current_rules_reorder_down
                    .borrow()
                    .iter()
                    .map(|r| r.id)
                    .collect();
                ids.swap(idx, idx + 1);
                apply_local_rule_order(&listbox_down, &current_rules_reorder_down, &ids);
                sync_rules_toolbar(
                    &listbox_down,
                    &current_rules_reorder_down,
                    &toolbar_edit_down,
                    &toolbar_delete_down,
                    &toolbar_up_down,
                    &toolbar_down_btn,
                    &toolbar_run_down,
                );
                sender_reorder_down.emit(MailMsg::ReorderMessageRules {
                    account: *current_account_reorder_down.borrow(),
                    ids,
                });
            }
        }
    });

    let sender_run = sender.clone();
    let parent_run = parent.clone();
    let folders_run = spec.folders.clone();
    let accounts_run = spec.accounts.clone();
    let current_rules_run = current_rules.clone();
    let current_account_run = current_account.clone();
    handle.toolbar_run.connect_clicked(move |_| {
        let account_run = *current_account_run.borrow();
        let account_email = accounts_run
            .iter()
            .find(|a| a.id == account_run)
            .map(|a| a.email.clone())
            .unwrap_or_default();
        let folders = folders_run.get(&account_run).cloned().unwrap_or_default();
        if folders.is_empty() {
            return;
        }
        let spec = FolderPickerSpec {
            mode: FolderPickerMode::Run,
            accounts: vec![(account_email, folders)],
            disabled: HashSet::new(),
            recents: vec![],
        };
        let sender_run = sender_run.clone();
        let current_rules_run = current_rules_run.clone();
        open_folder_picker(&parent_run, spec, move |folder| {
            let enabled_ids: Vec<RuleId> = current_rules_run
                .borrow()
                .iter()
                .filter(|r| r.enabled)
                .map(|r| r.id)
                .collect();
            sender_run.emit(MailMsg::RunMessageRules {
                folder,
                rule_ids: Some(enabled_ids),
            });
        });
    });
}

fn open_new_rule_editor(
    parent: &gtk::Widget,
    account_id: AccountId,
    accounts: &[Account],
    folders: &HashMap<AccountId, Vec<Folder>>,
    sender: Sender<MailMsg>,
    show_run_checkbox: bool,
    run_folder: Option<FolderId>,
) {
    let account_email = accounts
        .iter()
        .find(|a| a.id == account_id)
        .map(|a| a.email.clone())
        .unwrap_or_default();
    let account_folders = folders.get(&account_id).cloned().unwrap_or_default();
    let rule = new_rule_for_account(account_id);
    open_rule_editor(
        parent,
        RuleEditorSpec {
            rule,
            account_email,
            folders: account_folders,
            is_new: true,
            show_run_checkbox,
        },
        move |saved, also_run| {
            let run = if also_run { run_folder } else { None };
            sender.emit(MailMsg::UpsertMessageRule {
                rule: saved,
                run_folder: run,
            });
        },
    );
}

fn open_existing_rule_editor(
    parent: &gtk::Widget,
    rule: &MessageRule,
    accounts: &[Account],
    folders: &HashMap<AccountId, Vec<Folder>>,
    sender: Sender<MailMsg>,
    show_run_checkbox: bool,
    run_folder: Option<FolderId>,
) {
    let account_email = accounts
        .iter()
        .find(|a| a.id == rule.account_id)
        .map(|a| a.email.clone())
        .unwrap_or_default();
    let account_folders = folders.get(&rule.account_id).cloned().unwrap_or_default();
    open_rule_editor(
        parent,
        RuleEditorSpec {
            rule: rule.clone(),
            account_email,
            folders: account_folders,
            is_new: false,
            show_run_checkbox,
        },
        move |saved, also_run| {
            let run = if also_run { run_folder } else { None };
            sender.emit(MailMsg::UpsertMessageRule {
                rule: saved,
                run_folder: run,
            });
        },
    );
}

fn suggested_cta(label: &str) -> gtk::Button {
    gtk::Button::builder()
        .label(label)
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .build()
}

fn set_local_list_visible(handle: &RulesDialogHandle, visible: bool) {
    handle.listbox.set_visible(visible);
    handle.list_scroll.set_visible(visible);
    handle.content_host.set_vexpand(!visible);
}

fn sync_toolbar_state(handle: &RulesDialogHandle, rules: &[MessageRule]) {
    let enabled_count = rules.iter().filter(|r| r.enabled).count();
    handle.toolbar_edit.set_sensitive(false);
    handle.toolbar_delete.set_sensitive(false);
    handle.toolbar_up.set_sensitive(false);
    handle.toolbar_down.set_sensitive(false);
    handle.toolbar_run.set_sensitive(enabled_count > 0);
}

fn flat_toolbar_button(icon: &str, label: &str, bundled: bool) -> gtk::Button {
    let btn = gtk::Button::builder()
        .css_classes(["flat"])
        .tooltip_text(label)
        .build();
    btn.update_property(&[gtk::accessible::Property::Label(label)]);
    if bundled {
        let image = gtk::Image::new();
        icons::set_image(&image, icon, TOOLBAR_PIXEL_SIZE);
        btn.set_child(Some(&image));
    } else {
        btn.set_icon_name(icon);
    }
    btn
}

/// Give the account ComboRow’s dropdown the leftover row width so addresses
/// are not squeezed into a suffix the title column would otherwise keep empty.
fn expand_account_combo(row: &adw::ComboRow) {
    fn find_dropdown(widget: &gtk::Widget) -> Option<gtk::DropDown> {
        if let Ok(dropdown) = widget.clone().downcast::<gtk::DropDown>() {
            return Some(dropdown);
        }
        let mut child = widget.first_child();
        while let Some(node) = child {
            if let Some(found) = find_dropdown(&node) {
                return Some(found);
            }
            child = node.next_sibling();
        }
        None
    }

    fn shrink_title_box(widget: &gtk::Widget) {
        if widget.css_name().as_str() == "box" && widget.has_css_class("title") {
            widget.set_hexpand(false);
            widget.set_halign(gtk::Align::Start);
        }
        let mut child = widget.first_child();
        while let Some(node) = child {
            shrink_title_box(&node);
            child = node.next_sibling();
        }
    }

    shrink_title_box(row.upcast_ref());
    let Some(dropdown) = find_dropdown(row.upcast_ref()) else {
        return;
    };
    dropdown.set_hexpand(true);
    dropdown.set_halign(gtk::Align::Fill);
    dropdown.add_css_class("alcedo-rules-account-dropdown");
    let mut parent = dropdown.parent();
    while let Some(widget) = parent {
        if widget == *row {
            break;
        }
        widget.set_hexpand(true);
        parent = widget.parent();
    }
}

fn banner_message(server_rules: &ServerRulesStatus) -> &'static str {
    match server_rules {
        ServerRulesStatus::Ready => {
            trs!("These rules run on the server, including when Alcedo is closed.")
        }
        ServerRulesStatus::ForeignActive { .. } => {
            trs!("This account already has server filters. Alcedo rules run on this computer.")
        }
        ServerRulesStatus::Unavailable | ServerRulesStatus::Unknown => {
            trs!("These rules run on this computer when Alcedo checks the Inbox.")
        }
    }
}

fn list_subtitle(
    rule: &MessageRule,
    folders: &[Folder],
    server_rules: &ServerRulesStatus,
    rules: &[MessageRule],
) -> String {
    let base = rule_summary(rule, folders);
    let has_server = rules.iter().any(|r| r.execution == RuleExecution::Server);
    let has_client = rules.iter().any(|r| r.execution == RuleExecution::Client);
    let mixed = has_server && has_client;
    let show_suffix = matches!(server_rules, ServerRulesStatus::Ready) || mixed;
    if !show_suffix {
        return base;
    }
    let suffix = if rule.execution == RuleExecution::Server {
        trs!(" · On the server")
    } else {
        trs!(" · On this computer")
    };
    format!("{base}{suffix}")
}

fn reorder_ids_insert_before(ids: &mut Vec<RuleId>, dragged: RuleId, target: RuleId) -> bool {
    if dragged == target {
        return false;
    }
    let Some(from) = ids.iter().position(|id| *id == dragged) else {
        return false;
    };
    ids.remove(from);
    let Some(to) = ids.iter().position(|id| *id == target) else {
        return false;
    };
    ids.insert(to, dragged);
    true
}

fn apply_local_rule_order(
    listbox: &gtk::ListBox,
    current_rules: &Rc<RefCell<Vec<MessageRule>>>,
    new_ids: &[RuleId],
) {
    let old_ids: Vec<RuleId> = current_rules.borrow().iter().map(|r| r.id).collect();
    if old_ids == new_ids {
        return;
    }

    let selected_id = listbox
        .selected_row()
        .and_then(|row| old_ids.get(row.index() as usize).copied());

    let mut rows = Vec::new();
    while let Some(row) = listbox.row_at_index(0) {
        listbox.remove(&row);
        rows.push(row);
    }

    if rows.len() == old_ids.len() {
        for id in new_ids {
            if let Some(old_idx) = old_ids.iter().position(|old_id| old_id == id) {
                listbox.append(&rows[old_idx]);
            }
        }
    }

    let old_rules = current_rules.borrow().clone();
    let mut new_rules = Vec::new();
    for id in new_ids {
        if let Some(mut rule) = old_rules.iter().find(|rule| rule.id == *id).cloned() {
            rule.position = i32::try_from(new_rules.len()).unwrap_or(i32::MAX);
            new_rules.push(rule);
        }
    }
    *current_rules.borrow_mut() = new_rules;

    if let Some(id) = selected_id {
        let rules = current_rules.borrow();
        if let Some(idx) = rules.iter().position(|rule| rule.id == id)
            && let Some(row) = listbox.row_at_index(idx as i32)
        {
            listbox.select_row(Some(&row));
        }
    }
}

fn sync_rules_toolbar(
    listbox: &gtk::ListBox,
    current_rules: &Rc<RefCell<Vec<MessageRule>>>,
    toolbar_edit: &gtk::Button,
    toolbar_delete: &gtk::Button,
    toolbar_up: &gtk::Button,
    toolbar_down: &gtk::Button,
    toolbar_run: &gtk::Button,
) {
    let rules = current_rules.borrow();
    let selected_idx = listbox.selected_row().map(|row| row.index() as usize);
    let enabled_count = rules.iter().filter(|r| r.enabled).count();
    let has_selection = selected_idx.is_some();
    toolbar_edit.set_sensitive(has_selection);
    toolbar_delete.set_sensitive(has_selection);
    if let Some(idx) = selected_idx {
        toolbar_up.set_sensitive(idx > 0);
        toolbar_down.set_sensitive(idx + 1 < rules.len());
    } else {
        toolbar_up.set_sensitive(false);
        toolbar_down.set_sensitive(false);
    }
    toolbar_run.set_sensitive(enabled_count > 0);
}

fn attach_rule_drag_source(handle: &gtk::Box, row: &gtk::ListBoxRow, id: RuleId) {
    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::MOVE);
    // Weak: `handle` sits inside `row`, so a strong row here would be a cycle.
    let row_begin = row.downgrade();
    let row_end = row.downgrade();
    source.connect_prepare(move |_, _, _| {
        let payload = format!("{RULE_DRAG_PREFIX}{}", id.0);
        Some(gdk::ContentProvider::for_value(&payload.to_value()))
    });
    source.connect_drag_begin(move |_, _| {
        if let Some(row) = row_begin.upgrade() {
            row.add_css_class("alcedo-rule-row-dragging");
        }
    });
    source.connect_drag_end(move |_, _, _| {
        if let Some(row) = row_end.upgrade() {
            row.remove_css_class("alcedo-rule-row-dragging");
        }
    });
    handle.add_controller(source);
}

fn attach_rule_drop_target(
    row: &gtk::ListBoxRow,
    target_id: RuleId,
    on_reorder: Rc<dyn Fn(RuleId, RuleId)>,
) {
    let drop_target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    let on_reorder_drop = on_reorder.clone();
    drop_target.connect_enter(|target, _, _| {
        crate::folder_bar::set_drop_highlight(target, true);
        gdk::DragAction::MOVE
    });
    drop_target.connect_motion(|target, _, _| {
        crate::folder_bar::set_drop_highlight(target, true);
        gdk::DragAction::MOVE
    });
    drop_target.connect_leave(|target| crate::folder_bar::set_drop_highlight(target, false));
    drop_target.connect_drop(move |target, value, _, _| {
        crate::folder_bar::set_drop_highlight(target, false);
        let Ok(text) = value.get::<String>() else {
            return false;
        };
        let Some(uuid_str) = text.strip_prefix(RULE_DRAG_PREFIX) else {
            return false;
        };
        let Ok(uuid) = uuid::Uuid::parse_str(uuid_str) else {
            return false;
        };
        let dragged = RuleId(uuid);
        if dragged == target_id {
            return false;
        }
        on_reorder_drop(dragged, target_id);
        true
    });
    row.add_controller(drop_target);
}

fn local_rule_row(
    rule: &MessageRule,
    folders: &[Folder],
    server_rules: &ServerRulesStatus,
    all_rules: &[MessageRule],
    current_rules: Rc<RefCell<Vec<MessageRule>>>,
    switch_sender: Rc<RefCell<Option<Sender<MailMsg>>>>,
    on_reorder: Rc<dyn Fn(RuleId, RuleId)>,
) -> gtk::ListBoxRow {
    let subtitle = list_subtitle(rule, folders, server_rules, all_rules);
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title(&rule.name)
        .subtitle(&subtitle)
        .css_classes(["alcedo-rule-row"])
        .activatable(true)
        .build();
    row.update_property(&[gtk::accessible::Property::Label(&rule.name)]);
    if !rule.enabled {
        row.add_css_class("dim-label");
    }

    let handle_img = gtk::Image::builder()
        .icon_name("list-drag-handle-symbolic")
        .pixel_size(16)
        .valign(gtk::Align::Center)
        .css_classes(["alcedo-rule-drag-handle"])
        .build();
    handle_img.update_property(&[gtk::accessible::Property::Label(&tr!("Reorder rule"))]);
    handle_img.set_tooltip_text(Some(&tr!("Reorder rule")));
    handle_img.set_can_target(false);
    let handle_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    handle_box.set_valign(gtk::Align::Center);
    handle_box.append(&handle_img);

    let switch = gtk::Switch::builder()
        .active(rule.enabled)
        .valign(gtk::Align::Center)
        .build();
    switch.update_property(&[gtk::accessible::Property::Label(&trf!(
        "Enable {rule}",
        rule = rule.name
    ))]);
    row.add_prefix(&switch);
    row.add_prefix(&handle_box);

    let rule_id = rule.id;
    let rule_fallback = rule.clone();
    let current_rules_switch = current_rules;
    switch.connect_active_notify(move |sw| {
        let mut updated = current_rules_switch
            .borrow()
            .iter()
            .find(|r| r.id == rule_id)
            .cloned()
            .unwrap_or_else(|| rule_fallback.clone());
        if sw.is_active() == updated.enabled {
            return;
        }
        updated.enabled = sw.is_active();
        if let Some(sender) = switch_sender.borrow().clone() {
            sender.emit(MailMsg::UpsertMessageRule {
                rule: updated,
                run_folder: None,
            });
        }
    });

    let list_row = gtk::ListBoxRow::from(row);
    attach_rule_drag_source(&handle_box, &list_row, rule.id);
    attach_rule_drop_target(&list_row, rule.id, on_reorder);
    list_row
}

fn server_rule_row(rule: &ServerRuleView, folders: &[Folder]) -> gtk::ListBoxRow {
    let subtitle = server_rule_summary(rule, folders);
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title(&rule.name)
        .subtitle(&subtitle)
        .activatable(false)
        .css_classes(["alcedo-rule-row", "alcedo-rule-row-server"])
        .build();
    row.update_property(&[gtk::accessible::Property::Label(&rule.name)]);
    gtk::ListBoxRow::from(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn rule_id(n: u8) -> RuleId {
        RuleId(Uuid::from_bytes([n; 16]))
    }

    #[test]
    fn reorder_ids_insert_before_moves_later_item_before_first() {
        let a = rule_id(1);
        let b = rule_id(2);
        let c = rule_id(3);
        let mut ids = vec![a, b, c];
        assert!(reorder_ids_insert_before(&mut ids, c, a));
        assert_eq!(ids, vec![c, a, b]);
    }

    #[test]
    fn reorder_ids_insert_before_moves_first_before_last() {
        let a = rule_id(1);
        let b = rule_id(2);
        let c = rule_id(3);
        let mut ids = vec![a, b, c];
        assert!(reorder_ids_insert_before(&mut ids, a, c));
        assert_eq!(ids, vec![b, a, c]);
    }

    #[test]
    fn reorder_ids_insert_before_adjacent_identity() {
        let a = rule_id(1);
        let b = rule_id(2);
        let mut ids = vec![a, b];
        let before = ids.clone();
        assert!(reorder_ids_insert_before(&mut ids, a, b));
        assert_eq!(ids, before);
    }

    #[test]
    fn reorder_ids_insert_before_rejects_self() {
        let a = rule_id(1);
        let b = rule_id(2);
        let mut ids = vec![a, b];
        assert!(!reorder_ids_insert_before(&mut ids, a, a));
        assert_eq!(ids, vec![a, b]);
    }

    #[test]
    fn reorder_ids_insert_before_rejects_missing_id() {
        let a = rule_id(1);
        let b = rule_id(2);
        let missing = rule_id(9);
        let mut ids = vec![a, b];
        assert!(!reorder_ids_insert_before(&mut ids, missing, a));
        assert_eq!(ids, vec![a, b]);
    }

    #[test]
    fn reorder_ids_insert_before_two_item_swap() {
        let a = rule_id(1);
        let b = rule_id(2);
        let mut ids = vec![a, b];
        assert!(reorder_ids_insert_before(&mut ids, b, a));
        assert_eq!(ids, vec![b, a]);
    }

    #[test]
    fn server_rule_name_with_markup_characters_is_shown_verbatim() {
        crate::test_helpers::gtk_test_run(|| {
            let name = "Bills & <b>Receipts</b>";
            let rule = ServerRuleView {
                name: name.to_string(),
                match_mode: alcedo_core::RuleMatchMode::All,
                stop_processing: false,
                conditions: Vec::new(),
                actions: Vec::new(),
            };
            let row = server_rule_row(&rule, &[]);
            let texts = crate::test_helpers::displayed_label_texts(&row);
            assert!(texts.iter().any(|text| text == name), "{texts:?}");
        });
    }

    #[test]
    fn rule_row_with_drag_and_drop_is_freed_after_removal() {
        crate::test_helpers::gtk_test_run(|| {
            assert!(crate::test_helpers::row_is_freed_after_removal(
                |row, sentinel| {
                    let handle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                    row.set_child(Some(&handle));
                    let id = RuleId(Uuid::nil());
                    attach_rule_drag_source(&handle, row, id);
                    attach_rule_drop_target(
                        row,
                        id,
                        Rc::new(move |_, _| {
                            let _ = &sentinel;
                        }),
                    );
                }
            ));
        });
    }
}
