//! First-run welcome page, shown in place of the mail layout when no accounts
//! are configured.

use alcedo_core::welcome::{WELCOME_DESCRIPTION, WELCOME_TITLE, show_welcome_page};

use super::*;

/// Welcome toolbar: primary menu and window controls, plus a status page whose
/// Add Account button opens the existing dialog. The dialog is not opened
/// automatically.
pub(super) fn build_welcome_page(on_add_account: impl Fn() + 'static) -> adw::ToolbarView {
    let header = adw::HeaderBar::new();
    header.set_centering_policy(adw::CenteringPolicy::Loose);
    header.set_show_title(false);
    header.set_show_start_title_buttons(true);
    header.set_show_end_title_buttons(true);
    header.pack_start(&crate::menu::build_primary_menu_button());

    let add_account = gtk::Button::builder()
        .label("Add Account")
        .css_classes(["pill", "suggested-action"])
        .halign(gtk::Align::Center)
        .build();
    add_account.connect_clicked(move |_| on_add_account());

    let page = adw::StatusPage::builder()
        .icon_name("dev.alcedo.Mail")
        .title(WELCOME_TITLE)
        .description(WELCOME_DESCRIPTION)
        .child(&add_account)
        .vexpand(true)
        .hexpand(true)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));
    toolbar.set_vexpand(true);
    toolbar.set_hexpand(true);
    toolbar
}

impl MailModel {
    /// Startup emits every `AccountAdded` before any other sync event. The
    /// first event that is not `AccountAdded` means the account list is complete.
    pub(super) fn observe_sync_for_welcome(&mut self, event: &SyncEvent) {
        if !matches!(event, SyncEvent::AccountAdded(_)) {
            self.accounts_hydrated = true;
        }
    }

    pub(super) fn refresh_welcome_page(&self) {
        let page = if show_welcome_page(self.accounts_hydrated, self.accounts.len(), self.mock) {
            "welcome"
        } else {
            "mail"
        };
        self.root_stack.set_visible_child_name(page);
    }
}
