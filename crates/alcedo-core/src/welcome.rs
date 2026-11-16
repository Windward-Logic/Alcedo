//! First-run welcome visibility. The mail layout stays up until startup has
//! delivered every saved account (the runtime emits those first) and the
//! list is still empty. Mock mode always keeps the mail layout.

/// Whether the welcome page replaces the mail layout.
pub fn show_welcome_page(accounts_hydrated: bool, account_count: usize, mock: bool) -> bool {
    accounts_hydrated && account_count == 0 && !mock
}

/// Status-page title on the first-run welcome page.
pub const WELCOME_TITLE: &str = "Welcome to Alcedo";

/// One-line description under the welcome title.
pub const WELCOME_DESCRIPTION: &str = "Add an email account to read and send mail.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_page_only_when_hydrated_empty_and_not_mock() {
        assert!(!show_welcome_page(false, 0, false));
        assert!(!show_welcome_page(true, 1, false));
        assert!(!show_welcome_page(true, 0, true));
        assert!(show_welcome_page(true, 0, false));
    }

    #[test]
    fn welcome_copy_is_the_first_run_title() {
        assert_eq!(WELCOME_TITLE, "Welcome to Alcedo");
        assert!(!WELCOME_DESCRIPTION.is_empty());
        assert!(!WELCOME_DESCRIPTION.contains('\n'));
    }
}
