//! TCP and TLS connections for IMAP and other mail protocols (rustls only).

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
        tracing::debug!(%err, "failed to enable TCP keepalive on mail socket");
    }
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    if let Err(err) = sock_ref.set_tcp_user_timeout(Some(TCP_USER_TIMEOUT)) {
        tracing::debug!(%err, "failed to set TCP user timeout on mail socket");
    }
}

/// Open a TCP connection to `host:port`, set up for a long-lived mail session.
pub async fn connect_tcp(host: &str, port: u16) -> Result<TcpStream> {
    let addr = format!("{host}:{port}");
    let tcp = tokio::time::timeout(TCP_CONNECT_TIMEOUT, TcpStream::connect(&addr))
        .await
        .map_err(|_| Error::Network(format!("connect timeout to {addr}")))?
        .map_err(|e| Error::Network(format!("tcp connect to {addr}: {e}")))?;
    configure_tcp(&tcp);
    Ok(tcp)
}

/// Start TLS on `tcp` with system root CAs (plus the bundled Mozilla roots) and
/// SNI set to `host`. The rustls config is shared.
pub async fn upgrade_to_tls(host: &str, tcp: TcpStream) -> Result<ImapTransport> {
    let connector = TlsConnector::from(alcedo_tls::client_config());
    let server_name = ServerName::try_from(host.to_string())
        .map_err(|e| Error::Network(format!("invalid TLS server name {host:?}: {e}")))?;

    tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, connector.connect(server_name, tcp))
        .await
        .map_err(|_| Error::Network(format!("tls handshake timeout with {host}")))?
        .map_err(|e| Error::Network(format!("tls handshake with {host}: {e}")))
}

/// Connect to `host:port` over implicit TLS.
pub async fn connect_tls(host: &str, port: u16) -> Result<ImapTransport> {
    let tcp = connect_tcp(host, port).await?;
    upgrade_to_tls(host, tcp).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    async fn loopback_listener() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("local addr").port();
        (listener, port)
    }

    #[tokio::test]
    async fn connect_tcp_sets_up_the_socket_for_a_long_session() {
        let (_listener, port) = loopback_listener().await;
        let tcp = connect_tcp("127.0.0.1", port).await.expect("connect");

        assert!(tcp.nodelay().expect("nodelay"));
        let socket = socket2::SockRef::from(&tcp);
        assert!(socket.keepalive().expect("keepalive"));
        #[cfg(target_os = "linux")]
        assert_eq!(
            socket.tcp_user_timeout().expect("user timeout"),
            Some(TCP_USER_TIMEOUT)
        );
    }

    #[tokio::test]
    async fn connect_tcp_reports_a_refused_connection() {
        let (listener, port) = loopback_listener().await;
        drop(listener);

        let err = connect_tcp("127.0.0.1", port).await.expect_err("refused");
        assert!(matches!(err, Error::Network(_)), "{err}");
        assert!(err.to_string().contains("tcp connect to"), "{err}");
    }

    #[tokio::test]
    async fn upgrade_to_tls_rejects_an_invalid_host_name() {
        let (_listener, port) = loopback_listener().await;
        let tcp = connect_tcp("127.0.0.1", port).await.expect("connect");

        let err = upgrade_to_tls("not a host name", tcp)
            .await
            .expect_err("invalid name");
        assert!(err.to_string().contains("invalid TLS server name"), "{err}");
    }
}
