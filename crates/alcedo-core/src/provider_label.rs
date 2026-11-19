//! Friendly names for the Add Account provider list.

use alcedo_i18n::trs;
/// Providers shown in Add Account, in combo order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailProviderKind {
    Google,
    Microsoft365,
    Yahoo,
    Other,
}

impl MailProviderKind {
    pub const ALL: [Self; 4] = [Self::Google, Self::Microsoft365, Self::Yahoo, Self::Other];
}

/// Label for the provider combo. Order matches [`MailProviderKind::ALL`].
pub fn mail_provider_label(kind: MailProviderKind) -> &'static str {
    match kind {
        MailProviderKind::Google => "Google",
        MailProviderKind::Microsoft365 => "Microsoft 365 / Outlook",
        MailProviderKind::Yahoo => "Yahoo",
        MailProviderKind::Other => trs!("Other email"),
    }
}

pub fn mail_provider_labels() -> [&'static str; 4] {
    MailProviderKind::ALL.map(mail_provider_label)
}

/// SMTP security combo, implicit TLS first.
pub fn smtp_security_labels() -> [&'static str; 2] {
    ["SSL/TLS (recommended)", "STARTTLS"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_labels_are_friendly_and_stable() {
        assert_eq!(
            mail_provider_labels(),
            ["Google", "Microsoft 365 / Outlook", "Yahoo", "Other email",]
        );
    }

    #[test]
    fn security_labels_prefer_ssl() {
        assert_eq!(
            smtp_security_labels(),
            ["SSL/TLS (recommended)", "STARTTLS"]
        );
    }
}
