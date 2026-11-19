//! Account-form validation messages in the user's language.
//!
//! `alcedo_core` validation returns fixed English text, which the forms also
//! use to decide which row to mark as wrong, so the text stays English there
//! and is translated only here, where it is shown.

use alcedo_i18n::tr;

/// `message` from core validation, translated for display. Text this table
/// does not know (for example from another program) is shown as it came.
pub(crate) fn validation_text(message: &str) -> String {
    match message {
        "Email is required" => tr!("Email is required"),
        "Email must contain @" => tr!("Email must contain @"),
        "Email is invalid" => tr!("Email is invalid"),
        "IMAP host is required" => tr!("IMAP host is required"),
        "IMAP host must match the OAuth provider" => tr!("IMAP host must match the OAuth provider"),
        "SMTP host is required" => tr!("SMTP host is required"),
        "SMTP host must match the OAuth provider" => tr!("SMTP host must match the OAuth provider"),
        "IMAP port must be between 1 and 65535" => tr!("IMAP port must be between 1 and 65535"),
        "SMTP port must be between 1 and 65535" => tr!("SMTP port must be between 1 and 65535"),
        "Password is required" => tr!("Password is required"),
        "SMTP password is required" => tr!("SMTP password is required"),
        "OAuth provider is required" => tr!("OAuth provider is required"),
        "OAuth sign-in is required" => tr!("OAuth sign-in is required"),
        "OAuth refresh token is required" => tr!("OAuth refresh token is required"),
        "This address is already the account address" => {
            tr!("This address is already the account address")
        }
        "This address uses Google OAuth" => tr!("This address uses Google OAuth"),
        "This address uses Microsoft OAuth" => tr!("This address uses Microsoft OAuth"),
        "Use an app password for this address, not OAuth" => {
            tr!("Use an app password for this address, not OAuth")
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::validation_text;

    #[test]
    fn known_and_unknown_messages_come_back_as_text() {
        assert_eq!(validation_text("Email is required"), "Email is required");
        assert_eq!(
            validation_text("from another program"),
            "from another program"
        );
    }
}
