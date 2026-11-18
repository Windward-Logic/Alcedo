//! What each OAuth mail provider requires, so shared code never checks the vendor.

mod google;
mod microsoft;

pub(crate) use google::GoogleMail;
pub(crate) use microsoft::MicrosoftMail;

/// Server settings and protocol behaviour of one OAuth mail provider.
pub trait MailProvider: Send + Sync {
    fn imap_host(&self) -> &'static str;

    /// Default SMTP host for addresses the provider hosts.
    fn smtp_host(&self) -> &'static str;

    fn smtp_port(&self) -> u16;

    /// True for STARTTLS, false for implicit TLS.
    fn smtp_starttls(&self) -> bool;

    /// SMTP host for a lowercase address domain the provider owns.
    fn smtp_host_for_domain(&self, _domain: &str) -> &'static str {
        self.smtp_host()
    }

    /// Other hosts that may receive this provider's OAuth tokens over SMTP.
    fn alternate_smtp_hosts(&self) -> &'static [&'static str] {
        &[]
    }

    /// True when SMTP must log in with the account's primary sign-in name.
    fn smtp_login_is_sign_in_name(&self) -> bool {
        false
    }

    /// True when the provider files its own Sent copy of mail submitted over SMTP.
    fn files_sent_copy_on_submit(&self) -> bool {
        false
    }
}
