use super::MailProvider;

/// SMTP host Microsoft documents for personal Outlook.com, Hotmail and Live mailboxes.
const PERSONAL_SMTP_HOST: &str = "smtp-mail.outlook.com";

/// Domains whose mailboxes are personal Microsoft accounts.
const PERSONAL_DOMAINS: [&str; 3] = ["outlook.com", "hotmail.com", "live.com"];

/// Outlook.com and Microsoft 365.
pub(crate) struct MicrosoftMail;

impl MailProvider for MicrosoftMail {
    fn imap_host(&self) -> &'static str {
        "outlook.office365.com"
    }

    fn smtp_host(&self) -> &'static str {
        "smtp.office365.com"
    }

    fn smtp_port(&self) -> u16 {
        587
    }

    fn smtp_starttls(&self) -> bool {
        true
    }

    fn smtp_host_for_domain(&self, domain: &str) -> &'static str {
        if PERSONAL_DOMAINS.contains(&domain) {
            PERSONAL_SMTP_HOST
        } else {
            self.smtp_host()
        }
    }

    fn alternate_smtp_hosts(&self) -> &'static [&'static str] {
        &[PERSONAL_SMTP_HOST]
    }

    /// SMTP rejects aliases that IMAP accepts.
    fn smtp_login_is_sign_in_name(&self) -> bool {
        true
    }

    /// The copy gets a new Message-ID, so a Message-ID search cannot find it.
    fn files_sent_copy_on_submit(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_domains_use_the_outlook_com_smtp_host() {
        for domain in PERSONAL_DOMAINS {
            assert_eq!(
                MicrosoftMail.smtp_host_for_domain(domain),
                PERSONAL_SMTP_HOST
            );
        }
    }

    #[test]
    fn other_domains_use_the_microsoft_365_smtp_host() {
        assert_eq!(
            MicrosoftMail.smtp_host_for_domain("office365.com"),
            "smtp.office365.com"
        );
        assert_eq!(
            MicrosoftMail.smtp_host_for_domain("company.example"),
            "smtp.office365.com"
        );
    }
}
