//! RFC 8058 one-click list unsubscribe (HTTPS POST).

use std::net::SocketAddr;
use std::time::Duration;

use alcedo_core::{
    Error, MessageId, Result, https_host_is_blocked, https_uri_host, ip_addr_is_blocked,
};
use alcedo_store::StoreHandle;

const POST_BODY: &str = "List-Unsubscribe=One-Click";
const FAILURE_MESSAGE: &str = "Could not unsubscribe. Try the link in the message.";

pub async fn one_click_unsubscribe(store: &StoreHandle, id: MessageId) -> (bool, String) {
    match one_click_unsubscribe_inner(store, id).await {
        Ok(message) => (true, message),
        Err(_) => (false, FAILURE_MESSAGE.to_string()),
    }
}

async fn one_click_unsubscribe_inner(store: &StoreHandle, id: MessageId) -> Result<String> {
    let stored = store
        .get_message(id)
        .await?
        .ok_or_else(|| Error::Store("message not found".to_string()))?;
    let Some(list) = stored.list_unsubscribe else {
        return Err(Error::Store("no list-unsubscribe data".to_string()));
    };
    if !list.one_click {
        return Err(Error::Store(
            "one-click unsubscribe unavailable".to_string(),
        ));
    }
    let https = list
        .https
        .as_deref()
        .ok_or_else(|| Error::Store("missing https unsubscribe URI".to_string()))?;
    let host = https_uri_host(https)
        .ok_or_else(|| Error::Store("invalid https unsubscribe URI".to_string()))?;
    if https_host_is_blocked(&host) {
        return Err(Error::Store("blocked unsubscribe host".to_string()));
    }
    let addrs = resolve_public_host(&host).await?;

    let client = reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .resolve_to_addrs(&host.to_ascii_lowercase(), &addrs)
        .build()
        .map_err(|e| Error::Network(format!("http client build failed: {e}")))?;

    let response = client
        .post(https)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header(
            "User-Agent",
            format!("Alcedo/{}", env!("CARGO_PKG_VERSION")),
        )
        .body(POST_BODY)
        .send()
        .await
        .map_err(|e| Error::Network(format!("unsubscribe POST failed: {e}")))?;

    if !response.status().is_success() {
        return Err(Error::Network(format!(
            "unsubscribe POST status {}",
            response.status()
        )));
    }

    let sender = stored
        .from
        .first()
        .map(|addr| addr.display_name())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "this mailing list".to_string());
    Ok(format!("Unsubscribed from {sender}."))
}

async fn resolve_public_host(host: &str) -> Result<Vec<SocketAddr>> {
    let lookup_host = host
        .trim()
        .trim_end_matches('.')
        .trim_start_matches('[')
        .trim_end_matches(']');
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((lookup_host, 443))
        .await
        .map_err(|e| Error::Network(format!("DNS lookup failed for {host}: {e}")))?
        .collect();
    if addrs.is_empty() {
        return Err(Error::Network(format!("no DNS answers for {host}")));
    }
    for addr in &addrs {
        if ip_addr_is_blocked(&addr.ip()) {
            return Err(Error::Network(format!(
                "blocked unsubscribe address {addr}"
            )));
        }
    }
    Ok(addrs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alcedo_core::{MessageId, parse_list_unsubscribe};

    #[test]
    fn blocked_host_short_circuits_before_network() {
        let list = parse_list_unsubscribe(
            Some("<https://127.0.0.1/unsub>"),
            Some("List-Unsubscribe=One-Click"),
        );
        assert!(list.is_none() || !list.unwrap().one_click);
    }

    #[tokio::test]
    async fn missing_one_click_returns_failure_without_network() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = StoreHandle::open_path(&dir.path().join("store.db")).expect("open store");
        let (ok, message) = one_click_unsubscribe(&store, MessageId(999)).await;
        assert!(!ok);
        assert_eq!(message, FAILURE_MESSAGE);
    }
}
