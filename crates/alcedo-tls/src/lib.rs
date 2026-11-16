//! Shared rustls client configuration for IMAP and other TLS
//! connectors.
//!
//! Trust anchors come from the operating system store (so corporate or
//! self-managed CAs installed system-wide work), plus the bundled Mozilla
//! roots as a fallback. The config is built once per process and shared.

use std::sync::{Arc, OnceLock};

/// Install the aws-lc-rs crypto provider once (no-op if one is installed).
pub fn ensure_crypto_provider() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

/// Root store: system certificates plus the bundled webpki roots.
pub fn root_store() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for err in &native.errors {
        tracing::debug!(%err, "error loading a system TLS certificate");
    }
    let (added, ignored) = roots.add_parsable_certificates(native.certs);
    tracing::debug!(added, ignored, "loaded system TLS roots");
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    roots
}

/// DER bytes of the operating system's trusted roots (loaded once).
///
/// For clients that cannot take a rustls `RootCertStore`, such as lettre,
/// which otherwise trusts only the bundled Mozilla roots and so rejects
/// servers behind a corporate or self-managed CA that IMAP accepts.
pub fn native_root_certs_der() -> &'static [Vec<u8>] {
    static CERTS: OnceLock<Vec<Vec<u8>>> = OnceLock::new();
    CERTS.get_or_init(|| {
        rustls_native_certs::load_native_certs()
            .certs
            .into_iter()
            .map(|cert| cert.as_ref().to_vec())
            .collect()
    })
}

/// The process-wide TLS client configuration (built on first use).
pub fn client_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            ensure_crypto_provider();
            Arc::new(
                rustls::ClientConfig::builder()
                    .with_root_certificates(root_store())
                    .with_no_client_auth(),
            )
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_is_built_once_and_includes_bundled_roots() {
        let first = client_config();
        let second = client_config();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(root_store().len() >= webpki_roots::TLS_SERVER_ROOTS.len());
    }
}
