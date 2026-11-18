use super::MailProvider;

/// Gmail and Google Workspace.
pub(crate) struct GoogleMail;

impl MailProvider for GoogleMail {
    fn imap_host(&self) -> &'static str {
        "imap.gmail.com"
    }

    fn smtp_host(&self) -> &'static str {
        "smtp.gmail.com"
    }

    fn smtp_port(&self) -> u16 {
        465
    }

    fn smtp_starttls(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_uses_the_address_login_and_needs_a_sent_copy() {
        assert!(!GoogleMail.smtp_login_is_sign_in_name());
        assert!(!GoogleMail.files_sent_copy_on_submit());
        assert_eq!(
            GoogleMail.smtp_host_for_domain("gmail.com"),
            "smtp.gmail.com"
        );
    }
}
