//! TLS connector for IMAP (port 993, rustls only).

use std::time::Duration;

use alcedo_core::{Error, Result};
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// TLS-wrapped stream for `async-imap` with the `runtime-tokio` feature.
pub type ImapTransport = tokio_rustls::client::TlsStream<TcpStream>;

/// Abort a socket whose sent bytes stay unacked. After suspend the peer is
/// gone but the kernel still reports ESTABLISHED, and retransmits can sit for
/// many minutes. That leaves a folder switch on "Loading…" until TCP gives up.
const TCP_USER_TIMEOUT: Duration = Duration::from_secs(30);
const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// A server that accepts TCP but never finishes the TLS handshake must not hang sync.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

fn configure_tcp(tcp: &TcpStream) {
    let _ = tcp.set_nodelay(true);
    let sock_ref = socket2::SockRef::from(tcp);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(10));
    if let Err(err) = sock_ref.set_tcp_keepalive(&keepalive) {
        tracing::debug!(%err, "failed to enable TCP keepalive on IMAP socket");
    }
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    if let Err(err) = sock_ref.set_tcp_user_timeout(Some(TCP_USER_TIMEOUT)) {
        tracing::debug!(%err, "failed to set TCP user timeout on IMAP socket");
    }
}

/// Connect to `host:port` over TLS with system root CAs (plus the bundled
/// Mozilla roots) and SNI set to `host`. The rustls config is shared.
pub async fn connect_tls(host: &str, port: u16) -> Result<ImapTransport> {
    let connector = TlsConnector::from(alcedo_tls::client_config());
    let addr = format!("{host}:{port}");
    let tcp = tokio::time::timeout(TCP_CONNECT_TIMEOUT, TcpStream::connect(&addr))
        .await
        .map_err(|_| Error::Network(format!("connect timeout to {addr}")))?
        .map_err(|e| Error::Network(format!("tcp connect to {addr}: {e}")))?;
    configure_tcp(&tcp);

    let server_name = ServerName::try_from(host.to_string())
        .map_err(|e| Error::Network(format!("invalid TLS server name {host:?}: {e}")))?;

    tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, connector.connect(server_name, tcp))
        .await
        .map_err(|_| Error::Network(format!("tls handshake timeout with {host}")))?
        .map_err(|e| Error::Network(format!("tls handshake with {host}: {e}")))
}
