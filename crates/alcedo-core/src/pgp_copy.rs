//! User-facing OpenPGP wording for the reader banner and its details dialog.
//! Pure so the security-sensitive copy is unit tested.

use crate::pgp::{PgpSignatureState, PgpStatus, format_fingerprint_display};
use alcedo_i18n::{n_, tr, trf, trm};

/// Banner text plus what the details dialog shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PgpBannerCopy {
    pub title: String,
    /// Show the warning style (the message may not be what it claims).
    pub warning: bool,
    /// Present when there is a signing key to describe.
    pub details: Option<PgpDetails>,
}

/// Rows of the "OpenPGP Details" dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PgpDetails {
    pub verdict: &'static str,
    pub signer: String,
    /// Grouped for reading, e.g. `ABCD 1234 …`.
    pub fingerprint: String,
}

const VERDICT_VALID: &str = n_!("Valid signature");
const VERDICT_UNKNOWN_KEY: &str = n_!("Key not in your keyring");
const SIGNER_NOT_STATED: &str = n_!("Not stated by the key");
const SIGNER_UNKNOWN_KEY: &str = n_!("Unknown — import the sender’s key to verify");

/// Copy for `status`, or `None` when there is nothing to say (plain mail).
pub fn pgp_banner_copy(status: &PgpStatus) -> Option<PgpBannerCopy> {
    let sig = &status.signature;
    let (title, warning) = match (status.encrypted, status.decrypted, sig) {
        (true, false, _) => (
            tr!("This message is encrypted, and Alcedo could not decrypt it."),
            true,
        ),
        (false, _, PgpSignatureState::None) => return None,
        (true, true, PgpSignatureState::None) => (
            tr!(
                "This message is encrypted, but it is not signed, so you cannot be sure who sent it."
            ),
            true,
        ),
        (encrypted, _, PgpSignatureState::Valid { signer_email, .. }) => {
            (valid_title(encrypted, signer_email.as_deref()), false)
        }
        (encrypted, _, PgpSignatureState::Invalid { .. }) => (
            if encrypted {
                tr!("This message is encrypted and signed, but the signature is not valid.")
            } else {
                tr!("This message is signed, but the signature is not valid.")
            },
            true,
        ),
        (encrypted, _, PgpSignatureState::UnknownKey { .. }) => (
            if encrypted {
                tr!("This message is encrypted and signed with a key that is not in your keyring.")
            } else {
                tr!("This message is signed with a key that is not in your keyring.")
            },
            true,
        ),
    };
    Some(PgpBannerCopy {
        title,
        warning,
        details: details_for(sig),
    })
}

fn valid_title(encrypted: bool, signer: Option<&str>) -> String {
    let signer = signer.map(str::trim).filter(|email| !email.is_empty());
    match (encrypted, signer) {
        (false, Some(email)) => trf!("Signed by {email}. The signature is valid.", email = email),
        (false, None) => tr!("This message is signed, and the signature is valid."),
        (true, Some(email)) => trf!(
            "This message is encrypted and signed by {email}. The signature is valid.",
            email = email
        ),
        (true, None) => tr!("This message is encrypted, and the signature is valid."),
    }
}

fn details_for(sig: &PgpSignatureState) -> Option<PgpDetails> {
    match sig {
        PgpSignatureState::Valid {
            fingerprint,
            signer_email,
        } => Some(PgpDetails {
            verdict: trm!(VERDICT_VALID),
            signer: signer_email
                .as_deref()
                .map(str::trim)
                .filter(|email| !email.is_empty())
                .unwrap_or(trm!(SIGNER_NOT_STATED))
                .to_string(),
            fingerprint: format_fingerprint_display(fingerprint),
        }),
        PgpSignatureState::UnknownKey { fingerprint } => Some(PgpDetails {
            verdict: trm!(VERDICT_UNKNOWN_KEY),
            signer: trm!(SIGNER_UNKNOWN_KEY).to_string(),
            fingerprint: format_fingerprint_display(fingerprint),
        }),
        PgpSignatureState::None | PgpSignatureState::Invalid { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP: &str = "0123456789ABCDEF0123456789ABCDEF01234567";

    fn status(encrypted: bool, decrypted: bool, signature: PgpSignatureState) -> PgpStatus {
        PgpStatus {
            encrypted,
            decrypted,
            signature,
        }
    }

    fn valid(email: Option<&str>) -> PgpSignatureState {
        PgpSignatureState::Valid {
            fingerprint: FP.into(),
            signer_email: email.map(Into::into),
        }
    }

    fn unknown() -> PgpSignatureState {
        PgpSignatureState::UnknownKey {
            fingerprint: FP.into(),
        }
    }

    #[test]
    fn plain_mail_has_no_banner() {
        assert_eq!(pgp_banner_copy(&PgpStatus::default()), None);
    }

    #[test]
    fn valid_signature_names_the_signer() {
        let copy = pgp_banner_copy(&status(false, false, valid(Some("ada@example.org")))).unwrap();
        assert_eq!(
            copy.title,
            "Signed by ada@example.org. The signature is valid."
        );
        assert!(!copy.warning);
        let details = copy.details.unwrap();
        assert_eq!(details.signer, "ada@example.org");
        assert_eq!(details.verdict, "Valid signature");
        assert_eq!(details.fingerprint, format_fingerprint_display(FP));
    }

    #[test]
    fn valid_signature_without_email_stays_generic() {
        let copy = pgp_banner_copy(&status(true, true, valid(None))).unwrap();
        assert_eq!(
            copy.title,
            "This message is encrypted, and the signature is valid."
        );
        assert_eq!(copy.details.unwrap().signer, "Not stated by the key");
    }

    #[test]
    fn unknown_key_is_not_called_invalid_when_encrypted() {
        let copy = pgp_banner_copy(&status(true, true, unknown())).unwrap();
        assert_eq!(
            copy.title,
            "This message is encrypted and signed with a key that is not in your keyring."
        );
        assert!(!copy.title.contains("not valid"));
        assert!(copy.warning);
        assert_eq!(copy.details.unwrap().verdict, "Key not in your keyring");
    }

    #[test]
    fn unknown_key_unencrypted_keeps_keyring_wording() {
        let copy = pgp_banner_copy(&status(false, false, unknown())).unwrap();
        assert_eq!(
            copy.title,
            "This message is signed with a key that is not in your keyring."
        );
    }

    #[test]
    fn invalid_signature_warns_without_details() {
        let invalid = PgpSignatureState::Invalid {
            reason: "bad".into(),
        };
        let copy = pgp_banner_copy(&status(false, false, invalid)).unwrap();
        assert_eq!(
            copy.title,
            "This message is signed, but the signature is not valid."
        );
        assert!(copy.warning);
        assert_eq!(copy.details, None);
    }

    #[test]
    fn undecryptable_and_unsigned_encrypted_warn() {
        let copy = pgp_banner_copy(&status(true, false, PgpSignatureState::None)).unwrap();
        assert!(copy.title.contains("could not decrypt"));
        let copy = pgp_banner_copy(&status(true, true, PgpSignatureState::None)).unwrap();
        assert!(copy.title.contains("not signed"));
        assert!(copy.warning);
    }
}
