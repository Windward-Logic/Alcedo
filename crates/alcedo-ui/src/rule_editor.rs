//! New / Edit message rule dialog.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use alcedo_core::{
    Folder, FolderId, MessageRule, RuleAction, RuleCondition, RuleField, RuleMatchMode, RuleOp,
    default_stop_processing, validate_rule,
};
use chrono::Utc;
use relm4::adw;
use relm4::adw::prelude::*;
use relm4::gtk;
use uuid::Uuid;

use crate::folder_bar::folder_display_name;
use crate::folder_picker::{FolderPickerMode, FolderPickerSpec, open_folder_picker};

#[derive(Clone, Debug)]
pub struct RuleEditorSpec {
    pub rule: MessageRule,
    pub account_email: String,
    pub folders: Vec<Folder>,
    pub is_new: bool,
    pub show_run_checkbox: bool,
}

const FIELDS: [RuleField; 6] = [
    RuleField::From,
    RuleField::To,
    RuleField::Cc,
    RuleField::Recipient,
    RuleField::Subject,
    RuleField::HasAttachment,
];

const OPS: [RuleOp; 6] = [
    RuleOp::Contains,
    RuleOp::NotContains,
    RuleOp::Is,
    RuleOp::IsNot,
    RuleOp::StartsWith,
    RuleOp::EndsWith,
];

fn field_index(field: RuleField) -> u32 {
    FIELDS.iter().position(|f| *f == field).unwrap_or(0) as u32
}

fn op_index(op: RuleOp) -> u32 {
    OPS.iter().position(|o| *o == op).unwrap_or(0) as u32
}

fn field_at(index: u32) -> RuleField {
    FIELDS
        .get(index as usize)
        .copied()
        .unwrap_or(RuleField::From)
}

fn op_at(index: u32) -> RuleOp {
    OPS.get(index as usize).copied().unwrap_or(RuleOp::Contains)
}

fn field_labels() -> &'static [&'static str] {
    &["From", "To", "Cc", "Recipient", "Subject", "Has attachment"]
}

fn op_labels() -> &'static [&'static str] {
    &[
        "contains",
        "does not contain",
        "is",
        "is not",
        "starts with",
        "ends with",
    ]
}

fn action_labels() -> &'static [&'static str] {
    &[
        "Move to folder",
        "Copy to folder",
        "Mark as read",
        "Mark as unread",
        "Star",
        "Unstar",
        "Move to Trash",
    ]
}

fn action_kind_index(action: &RuleAction) -> u32 {
    match action {
        RuleAction::MoveToFolder(_) => 0,
        RuleAction::CopyToFolder(_) => 1,
        RuleAction::MarkRead => 2,
        RuleAction::MarkUnread => 3,
        RuleAction::Star => 4,
        RuleAction::Unstar => 5,
        RuleAction::MoveToTrash => 6,
    }
}

fn action_at(index: u32, folder_id: FolderId) -> RuleAction {
    match index {
        0 => RuleAction::MoveToFolder(folder_id),
        1 => RuleAction::CopyToFolder(folder_id),
        2 => RuleAction::MarkRead,
        3 => RuleAction::MarkUnread,
        4 => RuleAction::Star,
        5 => RuleAction::Unstar,
        _ => RuleAction::MoveToTrash,
    }
}

fn folder_action_index(index: u32) -> bool {
    index == 0 || index == 1
}

struct ConditionRow {
    row_box: gtk::Box,
    field_combo: gtk::DropDown,
    op_combo: gtk::DropDown,
    value_row: gtk::Entry,
}

struct ActionRow {
    row_box: gtk::Box,
    action_combo: gtk::DropDown,
    folder_btn: gtk::Button,
    folder_id: Rc<RefCell<FolderId>>,
}

/// Open the rule editor dialog.
pub fn open_rule_editor(
    parent: &impl IsA<gtk::Widget>,
    spec: RuleEditorSpec,
    on_save: impl Fn(MessageRule, bool) + 'static,
) {
    let is_new = spec.is_new;
    let account_email = spec.account_email;
    let folders = spec.folders;
    let rule = spec.rule;
    let show_run_checkbox = spec.show_run_checkbox;
    let on_save = Rc::new(on_save);

    let dialog_title = if is_new { "New Rule" } else { "Edit Rule" };

    let name_row = adw::EntryRow::builder().title("Rule name").build();
    name_row.set_text(&rule.name);

    let match_model = gtk::StringList::new(&["All conditions", "Any condition"]);
    let match_row = adw::ComboRow::builder()
        .title("Match")
        .model(&match_model)
        .expression(gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            None::<&gtk::Expression>,
            "string",
        ))
        .selected(match_row_index(&rule.match_mode))
        .build();

    let (conditions_group, conditions_list, add_condition_btn) =
        clause_group("Conditions", "Add Condition");
    let (actions_group, actions_list, add_action_btn) = clause_group("Actions", "Add Action");
    let options_group = adw::PreferencesGroup::new();

    let condition_rows: Rc<RefCell<Vec<ConditionRow>>> = Rc::new(RefCell::new(Vec::new()));
    let action_rows: Rc<RefCell<Vec<ActionRow>>> = Rc::new(RefCell::new(Vec::new()));

    let stop_row = adw::SwitchRow::builder()
        .title("Stop after this rule")
        .subtitle("Do not apply later rules to the same message")
        .active(rule.stop_processing)
        .build();

    let also_apply_row = adw::SwitchRow::builder()
        .title("Also apply to messages in this folder")
        .visible(show_run_checkbox)
        .build();

    options_group.add(&stop_row);
    if show_run_checkbox {
        options_group.add(&also_apply_row);
    }

    let cancel_btn = gtk::Button::builder().label("Cancel").build();
    let save_btn = gtk::Button::builder()
        .label("Save")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();

    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let title_label = gtk::Label::builder()
        .label(dialog_title)
        .css_classes(["title"])
        .build();
    header.set_title_widget(Some(&title_label));
    header.pack_start(&cancel_btn);
    header.pack_end(&save_btn);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(18);
    body.set_margin_bottom(18);
    body.set_margin_start(18);
    body.set_margin_end(18);

    let name_group = adw::PreferencesGroup::new();
    if !account_email.is_empty() {
        let account_row = adw::ActionRow::builder()
            .use_markup(false)
            .title("Account")
            .subtitle(&account_email)
            .activatable(false)
            .build();
        name_group.add(&account_row);
    }
    name_group.add(&name_row);
    body.append(&name_group);

    let match_group = adw::PreferencesGroup::new();
    match_group.add(&match_row);
    body.append(&match_group);

    body.append(&conditions_group);
    body.append(&actions_group);
    body.append(&options_group);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&body));

    let scroller = gtk::ScrolledWindow::builder()
        .child(&toast_overlay)
        .vexpand(true)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder()
        .title(dialog_title)
        .content_width(720)
        .content_height(640)
        .default_widget(&save_btn)
        .focus_widget(&name_row)
        .child(&toolbar)
        .build();

    let update_save = {
        let name_row = name_row.clone();
        let match_row = match_row.clone();
        let stop_row = stop_row.clone();
        let condition_rows = condition_rows.clone();
        let action_rows = action_rows.clone();
        let save_btn = save_btn.clone();
        let rule_id = rule.id;
        let account_id = rule.account_id;
        let rule_position = rule.position;
        let rule_enabled = rule.enabled;
        Rc::new(move || {
            let conditions: Vec<RuleCondition> = condition_rows
                .borrow()
                .iter()
                .map(|row| RuleCondition {
                    field: field_at(row.field_combo.selected()),
                    op: op_at(row.op_combo.selected()),
                    value: row.value_row.text().to_string(),
                })
                .collect();
            let actions: Vec<RuleAction> = action_rows
                .borrow()
                .iter()
                .map(|row| {
                    let idx = row.action_combo.selected();
                    if folder_action_index(idx) {
                        action_at(idx, *row.folder_id.borrow())
                    } else {
                        action_at(idx, FolderId(0))
                    }
                })
                .collect();
            let draft = MessageRule {
                id: rule_id,
                account_id,
                name: name_row.text().to_string(),
                enabled: rule_enabled,
                position: rule_position,
                match_mode: match_mode_at(match_row.selected()),
                stop_processing: stop_row.is_active(),
                conditions,
                actions,
                execution: rule.execution,
                created_at: rule.created_at,
                updated_at: Utc::now(),
            };
            save_btn.set_sensitive(validate_rule(&draft).is_ok());
        })
    };

    let add_condition = {
        let conditions_list = conditions_list.clone();
        let condition_rows = condition_rows.clone();
        let update_save = update_save.clone();
        Rc::new(move || {
            let row = build_condition_row(
                &conditions_list,
                &condition_rows,
                RuleCondition {
                    field: RuleField::From,
                    op: RuleOp::Contains,
                    value: String::new(),
                },
                update_save.clone(),
            );
            condition_rows.borrow_mut().push(row);
            update_save();
        })
    };

    let add_action = {
        let actions_list = actions_list.clone();
        let action_rows = action_rows.clone();
        let parent_widget = parent.as_ref().clone();
        let account_email = account_email.clone();
        let folders = folders.clone();
        let update_save = update_save.clone();
        Rc::new(move || {
            let row = build_action_row(
                &actions_list,
                &action_rows,
                &parent_widget,
                &account_email,
                &folders,
                RuleAction::MoveToFolder(FolderId(0)),
                update_save.clone(),
            );
            action_rows.borrow_mut().push(row);
            update_save();
        })
    };

    for condition in &rule.conditions {
        let row = build_condition_row(
            &conditions_list,
            &condition_rows,
            condition.clone(),
            update_save.clone(),
        );
        condition_rows.borrow_mut().push(row);
    }
    if rule.conditions.is_empty() {
        add_condition();
    }

    for action in &rule.actions {
        let row = build_action_row(
            &actions_list,
            &action_rows,
            parent.as_ref(),
            &account_email,
            &folders,
            action.clone(),
            update_save.clone(),
        );
        action_rows.borrow_mut().push(row);
    }
    if rule.actions.is_empty() {
        add_action();
    }

    add_condition_btn.connect_clicked(move |_| add_condition());
    add_action_btn.connect_clicked(move |_| add_action());

    let update_save_name = update_save.clone();
    name_row.connect_changed(move |_| update_save_name());
    let update_save_match = update_save.clone();
    match_row.connect_notify_local(Some("selected"), move |_, _| update_save_match());
    let update_save_stop = update_save.clone();
    stop_row.connect_active_notify(move |_| update_save_stop());

    let toast_overlay_save = toast_overlay.clone();
    let also_apply_row_save = also_apply_row.clone();
    let name_row_save = name_row.clone();
    let match_row_save = match_row.clone();
    let stop_row_save = stop_row.clone();
    let condition_rows_save = condition_rows.clone();
    let action_rows_save = action_rows.clone();
    let on_save_save = on_save.clone();
    let rule_execution = rule.execution;
    let rule_created = rule.created_at;
    let rule_id_save = rule.id;
    let account_id_save = rule.account_id;
    let rule_position_save = rule.position;
    let rule_enabled_save = rule.enabled;
    let dialog_save = dialog.clone();
    save_btn.connect_clicked(move |_| {
        let conditions: Vec<RuleCondition> = condition_rows_save
            .borrow()
            .iter()
            .map(|row| RuleCondition {
                field: field_at(row.field_combo.selected()),
                op: op_at(row.op_combo.selected()),
                value: row.value_row.text().to_string(),
            })
            .collect();
        let actions: Vec<RuleAction> = action_rows_save
            .borrow()
            .iter()
            .map(|row| {
                let idx = row.action_combo.selected();
                if folder_action_index(idx) {
                    action_at(idx, *row.folder_id.borrow())
                } else {
                    action_at(idx, FolderId(0))
                }
            })
            .collect();
        let actions_for_stop = if actions.is_empty() {
            rule.actions.clone()
        } else {
            actions.clone()
        };
        let draft = MessageRule {
            id: rule_id_save,
            account_id: account_id_save,
            name: name_row_save.text().to_string(),
            enabled: rule_enabled_save,
            position: rule_position_save,
            match_mode: match_mode_at(match_row_save.selected()),
            stop_processing: stop_row_save.is_active(),
            conditions,
            actions,
            execution: rule_execution,
            created_at: rule_created,
            updated_at: Utc::now(),
        };
        if let Err(msg) = validate_rule(&draft) {
            toast_overlay_save.add_toast(crate::toast::plain(msg));
            return;
        }
        if draft.stop_processing != default_stop_processing(&actions_for_stop)
            && stop_row_save.is_active() == default_stop_processing(&actions_for_stop)
        {
            // User explicitly set stop switch; keep their choice.
        }
        let run_folder = also_apply_row_save.is_visible() && also_apply_row_save.is_active();
        on_save_save(draft, run_folder);
        dialog_save.close();
    });

    cancel_btn.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });
    dialog.present(Some(parent.as_ref()));
}

fn clause_group(title: &str, add_tooltip: &str) -> (adw::PreferencesGroup, gtk::Box, gtk::Button) {
    let add_btn = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .css_classes(["flat", "circular"])
        .valign(gtk::Align::Center)
        .tooltip_text(add_tooltip)
        .build();
    add_btn.update_property(&[gtk::accessible::Property::Label(add_tooltip)]);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    list.add_css_class("alcedo-rule-clauses");
    list.set_hexpand(true);
    list.set_margin_top(10);
    list.set_margin_bottom(10);
    list.set_margin_start(12);
    list.set_margin_end(12);

    let group = adw::PreferencesGroup::builder().title(title).build();
    group.add_css_class("alcedo-rule-clause-group");
    group.set_header_suffix(Some(&add_btn));
    group.add(&list);

    (group, list, add_btn)
}

fn match_row_index(mode: &RuleMatchMode) -> u32 {
    match mode {
        RuleMatchMode::All => 0,
        RuleMatchMode::Any => 1,
    }
}

fn match_mode_at(index: u32) -> RuleMatchMode {
    if index == 1 {
        RuleMatchMode::Any
    } else {
        RuleMatchMode::All
    }
}

fn sync_condition_row_visibility(row: &ConditionRow) {
    let field = field_at(row.field_combo.selected());
    let has_attachment = field == RuleField::HasAttachment;
    row.op_combo.set_visible(!has_attachment);
    row.value_row.set_visible(!has_attachment);
}

fn build_condition_row(
    list: &gtk::Box,
    rows: &Rc<RefCell<Vec<ConditionRow>>>,
    condition: RuleCondition,
    update_save: Rc<dyn Fn()>,
) -> ConditionRow {
    let field_model = gtk::StringList::new(field_labels());
    let field_combo = gtk::DropDown::builder()
        .model(&field_model)
        .selected(field_index(condition.field))
        .width_request(160)
        .hexpand(false)
        .valign(gtk::Align::Center)
        .tooltip_text("Field")
        .build();

    let op_model = gtk::StringList::new(op_labels());
    let op_combo = gtk::DropDown::builder()
        .model(&op_model)
        .selected(op_index(condition.op))
        .width_request(150)
        .hexpand(false)
        .valign(gtk::Align::Center)
        .tooltip_text("Operator")
        .build();

    let value_row = gtk::Entry::builder()
        .placeholder_text("Value")
        .hexpand(true)
        .width_request(240)
        .valign(gtk::Align::Center)
        .text(&condition.value)
        .build();

    let remove_btn = gtk::Button::builder()
        .icon_name("list-remove-symbolic")
        .css_classes(["flat", "circular"])
        .valign(gtk::Align::Center)
        .tooltip_text("Remove")
        .build();

    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row_box.set_hexpand(true);
    row_box.append(&field_combo);
    row_box.append(&op_combo);
    row_box.append(&value_row);
    row_box.append(&remove_btn);
    list.append(&row_box);

    sync_condition_row_visibility(&ConditionRow {
        row_box: row_box.clone(),
        field_combo: field_combo.clone(),
        op_combo: op_combo.clone(),
        value_row: value_row.clone(),
    });

    let row_box_field = row_box.clone();
    let op_combo_notify = op_combo.clone();
    let value_row_notify = value_row.clone();
    let update_save_field = update_save.clone();
    field_combo.connect_notify_local(Some("selected"), move |combo, _| {
        let row = ConditionRow {
            row_box: row_box_field.clone(),
            field_combo: combo.clone(),
            op_combo: op_combo_notify.clone(),
            value_row: value_row_notify.clone(),
        };
        sync_condition_row_visibility(&row);
        update_save_field();
    });
    let update_save_op = update_save.clone();
    op_combo.connect_notify_local(Some("selected"), move |_, _| update_save_op());
    let update_save_value = update_save.clone();
    value_row.connect_changed(move |_| update_save_value());

    let list_remove = list.clone();
    let rows_remove = rows.clone();
    let row_box_remove = row_box.clone();
    let update_save_remove = update_save.clone();
    remove_btn.connect_clicked(move |_| {
        list_remove.remove(&row_box_remove);
        rows_remove
            .borrow_mut()
            .retain(|row| row.row_box != row_box_remove);
        update_save_remove();
    });

    ConditionRow {
        row_box,
        field_combo,
        op_combo,
        value_row,
    }
}

fn sync_action_row_visibility(row: &ActionRow, folders: &[Folder]) {
    let idx = row.action_combo.selected();
    let needs_folder = folder_action_index(idx);
    row.folder_btn.set_visible(needs_folder);
    if needs_folder {
        let folder_id = *row.folder_id.borrow();
        let label = if folder_id.0 == 0 {
            "Choose folder…".to_string()
        } else {
            folders
                .iter()
                .find(|f| f.id == folder_id)
                .map(folder_display_name)
                .unwrap_or_else(|| "Missing folder".to_string())
        };
        row.folder_btn.set_label(&label);
    }
}

fn build_action_row(
    list: &gtk::Box,
    rows: &Rc<RefCell<Vec<ActionRow>>>,
    parent: &gtk::Widget,
    account_email: &str,
    folders: &[Folder],
    action: RuleAction,
    update_save: Rc<dyn Fn()>,
) -> ActionRow {
    let action_model = gtk::StringList::new(action_labels());
    let action_combo = gtk::DropDown::builder()
        .model(&action_model)
        .selected(action_kind_index(&action))
        .width_request(180)
        .hexpand(false)
        .valign(gtk::Align::Center)
        .tooltip_text("Action")
        .build();

    let folder_id = Rc::new(RefCell::new(match &action {
        RuleAction::MoveToFolder(id) | RuleAction::CopyToFolder(id) => *id,
        _ => FolderId(0),
    }));

    let folder_btn = gtk::Button::builder()
        .label("Choose folder…")
        .css_classes(["alcedo-rule-folder-btn"])
        .hexpand(true)
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Center)
        .tooltip_text("Folder")
        .build();

    let value_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    value_slot.set_hexpand(true);
    value_slot.set_halign(gtk::Align::Fill);
    value_slot.append(&folder_btn);

    let remove_btn = gtk::Button::builder()
        .icon_name("list-remove-symbolic")
        .css_classes(["flat", "circular"])
        .valign(gtk::Align::Center)
        .tooltip_text("Remove")
        .build();

    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row_box.set_hexpand(true);
    row_box.append(&action_combo);
    row_box.append(&value_slot);
    row_box.append(&remove_btn);
    list.append(&row_box);

    let row = ActionRow {
        row_box: row_box.clone(),
        action_combo: action_combo.clone(),
        folder_btn: folder_btn.clone(),
        folder_id: folder_id.clone(),
    };
    sync_action_row_visibility(&row, folders);

    let folders_sync = folders.to_vec();
    let update_save_combo = update_save.clone();
    let row_box_combo = row_box.clone();
    let folder_btn_combo = folder_btn.clone();
    let folder_id_combo = folder_id.clone();
    action_combo.connect_notify_local(Some("selected"), move |combo, _| {
        let row = ActionRow {
            row_box: row_box_combo.clone(),
            action_combo: combo.clone(),
            folder_btn: folder_btn_combo.clone(),
            folder_id: folder_id_combo.clone(),
        };
        sync_action_row_visibility(&row, &folders_sync);
        update_save_combo();
    });

    let parent_pick = parent.clone();
    let account_email_pick = account_email.to_string();
    let folders_pick = folders.to_vec();
    let folder_id_pick = folder_id.clone();
    let folder_btn_pick = folder_btn.clone();
    let update_save_pick = update_save.clone();
    folder_btn.connect_clicked(move |_| {
        let spec = FolderPickerSpec {
            mode: FolderPickerMode::Move,
            accounts: vec![(account_email_pick.clone(), folders_pick.clone())],
            disabled: HashSet::new(),
            recents: vec![],
        };
        let folder_id_pick = folder_id_pick.clone();
        let folder_btn_pick = folder_btn_pick.clone();
        let folders_pick = folders_pick.clone();
        let update_save_pick = update_save_pick.clone();
        open_folder_picker(&parent_pick, spec, move |chosen| {
            *folder_id_pick.borrow_mut() = chosen;
            let label = folders_pick
                .iter()
                .find(|f| f.id == chosen)
                .map(folder_display_name)
                .unwrap_or_else(|| "Missing folder".to_string());
            folder_btn_pick.set_label(&label);
            update_save_pick();
        });
    });

    let list_remove = list.clone();
    let rows_remove = rows.clone();
    let row_box_remove = row_box.clone();
    let update_save_remove = update_save.clone();
    remove_btn.connect_clicked(move |_| {
        list_remove.remove(&row_box_remove);
        rows_remove
            .borrow_mut()
            .retain(|row| row.row_box != row_box_remove);
        update_save_remove();
    });

    row
}

/// Build a blank rule for the selected account.
pub fn new_rule_for_account(account_id: alcedo_core::AccountId) -> MessageRule {
    let now = Utc::now();
    MessageRule {
        id: alcedo_core::RuleId(Uuid::new_v4()),
        account_id,
        name: String::new(),
        enabled: true,
        position: -1,
        match_mode: RuleMatchMode::All,
        stop_processing: default_stop_processing(&[]),
        conditions: Vec::new(),
        actions: Vec::new(),
        execution: alcedo_core::RuleExecution::Client,
        created_at: now,
        updated_at: now,
    }
}
