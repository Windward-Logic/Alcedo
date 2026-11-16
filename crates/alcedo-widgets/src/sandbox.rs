//! Ephemeral WebKit sandbox (P8).

use std::cell::RefCell;
use std::rc::Rc;

use alcedo_core::byte_lru::ByteLru;
use alcedo_core::raster_image_mime;
use gtk4::gio;
use gtk4::glib;
use webkit6::prelude::*;
use webkit6::{
    NetworkSession, SecurityManager, Settings, URISchemeRequest, UserContentInjectedFrames,
    UserContentManager, UserScript, UserScriptInjectionTime, WebContext,
};

/// Attachment bytes and MIME keyed by SHA-256 hex (for `alcedo-cid://`
/// URIs), bounded so browsing many large messages cannot grow memory without
/// limit. Evicted entries are re-filled when their message is opened again.
pub type CidCache = Rc<RefCell<ByteLru<String, (Vec<u8>, String)>>>;

/// Budget for the reader's shared [`CidCache`].
pub const CID_CACHE_MAX_BYTES: usize = 256 * 1024 * 1024;

/// The reader's shared attachment cache, bounded to [`CID_CACHE_MAX_BYTES`].
pub fn new_cid_cache() -> CidCache {
    Rc::new(RefCell::new(ByteLru::new(CID_CACHE_MAX_BYTES)))
}

/// An unbounded cache for content that must not be evicted (the composer's
/// own inline images, which exist nowhere else).
pub fn new_unbounded_cid_cache() -> CidCache {
    Rc::new(RefCell::new(ByteLru::new(usize::MAX)))
}

/// Create an isolated [`WebContext`] (separate from mail and other widgets).
pub fn create_isolated_context() -> WebContext {
    WebContext::new()
}

/// Mail reader settings — JavaScript and remote images disabled by default.
pub fn mail_settings() -> Settings {
    let settings = Settings::default();
    settings.set_enable_javascript(false);
    settings.set_auto_load_images(false);
    settings.set_javascript_can_access_clipboard(false);
    settings.set_allow_file_access_from_file_urls(false);
    settings.set_enable_write_console_messages_to_stdout(false);
    settings
}

/// Widget settings — JavaScript enabled in an ephemeral session.
pub fn widget_settings() -> Settings {
    let settings = Settings::default();
    settings.set_enable_javascript(true);
    settings
}

/// Compose editor settings — JS on, clipboard access, images via `alcedo-cid:` only.
pub fn compose_editor_settings() -> Settings {
    let settings = widget_settings();
    settings.set_auto_load_images(true);
    settings.set_javascript_can_access_clipboard(true);
    settings.set_allow_file_access_from_file_urls(false);
    settings.set_enable_write_console_messages_to_stdout(false);
    settings
}

/// Ephemeral network session (no persistent cookies).
pub fn ephemeral_network_session() -> NetworkSession {
    NetworkSession::new_ephemeral()
}

/// Register the internal `alcedo-cid://` scheme on a mail [`WebContext`].
pub fn register_cid_scheme(context: &WebContext, cache: CidCache) {
    if let Some(security) = context.security_manager() {
        register_cid_security(&security);
    }

    context.register_uri_scheme("alcedo-cid", move |request: &URISchemeRequest| {
        serve_cid_request(request, &cache);
    });
}

fn register_cid_security(security: &SecurityManager) {
    // Do not mark this scheme local. Mail HTML is loaded as `about:blank`,
    // which is not a local page, and WebKit refuses non-local pages access
    // to local schemes — the handler never runs and the image stays blank.
    // The handler only serves SHA-256 keys already in this view's cache.
    security.register_uri_scheme_as_secure("alcedo-cid");
}

fn serve_cid_request(request: &URISchemeRequest, cache: &CidCache) {
    let Some(sha256) = sha256_from_request(request) else {
        let mut err = glib::Error::new(gio::IOErrorEnum::NotFound, "missing alcedo-cid target");
        request.finish_error(&mut err);
        return;
    };

    let cache = cache.borrow();
    let Some((bytes, _declared_mime)) = cache.get(&sha256) else {
        let mut err = glib::Error::new(gio::IOErrorEnum::NotFound, "attachment not cached");
        request.finish_error(&mut err);
        return;
    };

    // This scheme exists to show inline images. The sender picks both the
    // attachment bytes (so knows their hash) and the declared type, and a
    // link in the message can navigate the view to this URI. Serving by the
    // declared type would let an attachment labelled, say,
    // `application/xhtml+xml` render as an unsanitized page in the reader.
    // Serve only what is a raster image by content, labelled as such.
    let Some(image_mime) = raster_image_mime(bytes) else {
        let mut err = glib::Error::new(
            gio::IOErrorEnum::PermissionDenied,
            "only raster images are served inline",
        );
        request.finish_error(&mut err);
        return;
    };

    let stream = gio::MemoryInputStream::from_bytes(&glib::Bytes::from(bytes.as_slice()));
    request.finish(&stream, bytes.len() as i64, Some(image_mime));
}

fn sha256_from_request(request: &URISchemeRequest) -> Option<String> {
    let candidate = if let Some(uri) = request.uri() {
        if let Ok(parsed) = url::Url::parse(uri.as_str()) {
            if let Some(host) = parsed.host_str() {
                if !host.is_empty() {
                    Some(host.to_string())
                } else {
                    None
                }
            } else {
                None
            }
            .or_else(|| {
                let path = parsed.path().trim_start_matches('/');
                if path.is_empty() {
                    None
                } else {
                    Some(path.to_string())
                }
            })
        } else {
            None
        }
    } else {
        None
    };

    candidate
        .or_else(|| {
            request
                .path()
                .map(|path| path.trim_start_matches('/').to_string())
                .filter(|path| !path.is_empty())
        })
        .filter(|value| is_sha256_hex(value))
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Build a [`UserContentManager`] with the `alcedo` script-message bridge.
pub fn widget_content_manager(on_message: Rc<dyn Fn(WidgetMessage)>) -> UserContentManager {
    let manager = UserContentManager::new();
    manager.register_script_message_handler("alcedo", None);

    let bridge = UserScript::new(
        WIDGET_BRIDGE_SCRIPT,
        UserContentInjectedFrames::AllFrames,
        UserScriptInjectionTime::Start,
        &[],
        &[],
    );
    manager.add_script(&bridge);

    manager.connect_script_message_received(Some("alcedo"), move |_manager, value| {
        let text = value.to_str().to_string();
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
            && let Some(message) = WidgetMessage::from_json(&json)
        {
            on_message(message);
        }
    });

    manager
}

const WIDGET_BRIDGE_SCRIPT: &str = r#"
(function () {
  window.addEventListener("message", function (event) {
    if (event.source !== window || !event.data) {
      return;
    }
    try {
      var payload = typeof event.data === "string" ? event.data : JSON.stringify(event.data);
      var parsed = JSON.parse(payload);
      if (!parsed || typeof parsed.type !== "string" || parsed.type.indexOf("alcedo.") !== 0) {
        return;
      }
      window.webkit.messageHandlers.alcedo.postMessage(payload);
    } catch (_err) {
      /* ignore malformed widget messages */
    }
  });
})();
"#;

/// Messages accepted from widget JavaScript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WidgetMessage {
    Ready,
    Resize { height: i32 },
}

impl WidgetMessage {
    fn from_json(value: &serde_json::Value) -> Option<Self> {
        let msg_type = value.get("type")?.as_str()?;
        match msg_type {
            "alcedo.ready" => Some(Self::Ready),
            "alcedo.resize" => {
                let height = value.get("height")?.as_i64()? as i32;
                Some(Self::Resize { height })
            }
            _ => None,
        }
    }
}

/// Push a theme update into a widget [`webkit6::WebView`].
pub fn post_theme_to_widget(web_view: &webkit6::WebView, dark: bool) {
    let script = format!("window.postMessage({{type:'alcedo.theme',dark:{dark}}}, '*');");
    web_view.evaluate_javascript(&script, None, None, None::<&gio::Cancellable>, |result| {
        if let Err(err) = result {
            tracing::debug!(?err, "widget theme script failed");
        }
    });
}
