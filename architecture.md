# Alcedo architecture (locked decisions)

## Product

- **App ID:** `dev.alcedo.Mail`
- **Binary:** `alcedo`
- **Target:** Ubuntu 26.04 LTS, GNOME 50, Wayland
- **Edition / MSRV:** Rust 2024, rustc 1.93

## Stack

- **UI:** Relm4 0.11 (`libadwaita`, `gnome_50`), GTK4, libadwaita
- **Mail HTML:** WebKitGTK 6 (widgets + sandboxed mail view in P8)
- **Async:** Tokio multi-thread runtime in the binary; Relm4 on GLib main loop
- **TLS:** rustls 0.23 only — no OpenSSL / native-tls. IMAP (and any extension that needs TLS) shares one `ClientConfig` from `alcedo-tls` (system trust store via rustls-native-certs, plus webpki-roots as a fallback), built once. reqwest (OAuth) uses `rustls-tls-native-roots`; lettre (SMTP) trusts webpki-roots plus the OS root store (`alcedo_tls::native_root_certs_der`), so it accepts the same self-managed CAs as IMAP.
- **Network timeouts:** every HTTP client sets connect (15 s) and request (60 s) timeouts; IMAP TLS handshakes time out after 20 s; the OAuth loopback drops stalled or oversized requests.
- **Transport security:** credentials and tokens are never sent in cleartext. IMAP is implicit TLS only; SMTP is implicit TLS or mandatory STARTTLS.
- **IMAP:** async-imap, two connections per account when the server allows (Inbox IDLE + command); single-session fallback otherwise
- **SMTP:** lettre with tokio1-rustls-tls on a per-account serial Tokio task (not on the IMAP command FIFO)
- **Store:** rusqlite, one writer thread plus one read-only reader thread (WAL, query_only). Not a connection pool.
- **Secrets:** oo7 (keyring / portal) — never SQLite
- **OAuth:** PKCE + system browser loopback for Google and Microsoft. Google needs a client in `oauth.toml`; Microsoft uses `oauth.toml` first, then Alcedo's built-in public client, which a build can replace with `ALCEDO_MICROSOFT_CLIENT_ID`. Microsoft SMTP logs in with the account's primary sign-in name from the ID token, because SMTP rejects aliases that IMAP accepts. Gmail, Yahoo and generic IMAP use app passwords. Access tokens live in one in-memory cache per (account, purpose) in `alcedo-sync::oauth_token_cache`, refreshed 60 s before expiry under a per-account async mutex; the OAuth client config is re-read only when `oauth.toml` changes. Token endpoint errors are summarized, never logged raw.

## Crate graph

```
alcedo-core   → (serde, uuid, chrono, thiserror)     # domain types, MailProvider profiles, reply planning, routing, private_fs, byte_lru
alcedo-tls    → rustls + rustls-native-certs + webpki-roots
alcedo-store  → core (rusqlite, bundled SQLite)
alcedo-html   → core (ammonia sanitizer, tracker detection)
alcedo-oauth  → core + oo7 + oauth2 + reqwest
alcedo-imap   → core + html + tls
alcedo-smtp   → core (lettre)
alcedo-pgp    → core + sequoia-openpgp + sequoia-autocrypt (Nettle; no OpenSSL)
alcedo-sync   → core, store, imap, smtp, oauth, html, pgp
alcedo-widgets → core + gtk/webkit
alcedo-ui     → core, html, oauth, widgets + relm4 (no Sequoia; PGP via UiCommand/SyncEvent only)
alcedo (bin)  → core, store, imap, smtp, oauth, sync, html, widgets, ui
```

OpenPGP: private keys and passphrases in **oo7** only; public keys and `pgp_sign_outgoing` in SQLite (`pgp_keys`, migration 015). Crypto runs on Tokio `spawn_blocking` in `alcedo-sync`, never on the GTK thread.

## Threading

- GTK thread: Relm4 UI, `UiCommand` out, `SyncEvent` in
- Tokio: per-account command IMAP worker, optional Inbox IDLE session, serial SMTP task
- Store: writer std::thread (migrations, writes) and one reader std::thread (autocommit reads, query_only).

## Local data and privacy

- Data dirs are created `0700`; the database (and its WAL/SHM), stored attachments and compose autosaves are `0600` (`alcedo_core::private_fs`). Older stores are tightened on open.
- Compose autosave: one JSON file per open composer under `<data>/alcedo/compose-drafts/`, written off the GTK thread. Attachment bytes are not autosaved (only their names, so the user can re-attach after a crash). All drafts from a previous session are offered back on start.
- "Open attachment" copies go to `$XDG_RUNTIME_DIR/alcedo/opened-attachments` (tmpfs) and are wiped at startup by the primary instance, along with the legacy `~/.cache/alcedo/attachments`.
- In-memory reader caches (text bodies, HTML bodies, attachment/inline bytes) are size-bounded LRUs; an evicted body is reloaded from the store when the message is opened.
- Outbound tracking pixels and read-receipt requests are off by default and disclosed in the composer when on.
- `ALCEDO_MOCK=1` runs on fixture data and never opens the real store.

## Code layout and known debt

The workspace is split by layer. New logic should land in small, testable modules (for example `alcedo-core::reply_planner`, `routing`, `subject`, `message_id`) and be called from the UI and sync layers rather than added to them.

### `alcedo-ui::mail` (mail window component)

`mail/mod.rs` holds `MailModel`, `MailMsg`, `MailInit`/`MailOutput`, the model's state types and the Relm4 `Component` impl (`view`, `init`, and an `update` that only routes each message group to a submodule). Each feature module adds its own `impl MailModel` block with an `update_<feature>` handler:

- `accounts` – add/edit/remove accounts, re-auth, PGP, identities, signatures, snippets
- `compose`, `drafts`, `send`, `reply` – composer lifecycle, local autosave and IMAP Drafts sync, the send pipeline, reply/forward drafting
- `reader`, `reactions` – reader pane and windows, bodies, images, unsubscribe, raw source; reaction chips and sending (`ReactionMsg`)
- `list`, `list_actions`, `delete` – list population and selection; move/copy/archive/spam/flags; delete, expunge and undo toasts
- `search`, `views`, `extension_views` – search, virtual views, and the list views an extension registers
- `folders`, `folder_ops`, `rules` – folder pane and tree; folder create/rename/delete/empty; rules dialog
- `status`, `chrome`, `window_actions`, `notify`, `calendar` – status bar and activity log; preferences/theme/palette/layout; gio actions and drag-and-drop; notification follow-ups; meeting-invite mock
- `sync_events/` – `handle_sync_event`, split into `list`, `reader`, `accounts` and `send` event groups

### `alcedo-sync::worker` (per-account IMAP worker)

`worker/mod.rs` holds `WorkerCommand`, `WorkerHandle`, `spawn_account_worker`, the worker state types (`WorkerImapState`, `ServerRulesSession`, …), the IMAP timeout helpers and `handle_worker_command`, which dispatches each command to a submodule:

- `session`, `idle`, `queue`, `auth` – account loop, SMTP task and connect; Inbox IDLE and poll waits; command ordering/coalescing/retry; IMAP/SMTP auth and OAuth tokens
- `mailbox_sync`, `watched`, `backfill` – folder list and mailbox sync; watched-folder STATUS polling, paging and body prefetch; header backfills
- `bodies`, `reactions`, `pgp` – opening messages, attachments and `BodyReady`; reaction persistence; incoming/outgoing OpenPGP
- `flags`, `flag_copies`, `moves`, `delete`, `folders` – flag changes; mirroring flags onto a Gmail message's copies in other labels; move/copy and UID resolution; delete/expunge/reconcile; folder operations
- `rules`, `search_import` – client-side rules and the server-rules backend hand-off (`server_rules::ServerRulesBackend`); cross-folder IMAP search import for extensions
- `drafts`, `send`, `sent_copy` – IMAP Drafts; SMTP send; Sent copies, Message-ID dedupe and sent attachments

### `alcedo-store::handle` (store request plumbing)

`handle/mod.rs` holds `StoreHandle`, `open_xdg`/`open_path`, the writer and reader thread spawners and the `send_read`/`send_write` helpers. Each request type lives in three places, all grouped by table in the same order (`// --- <table> ---` sections):

- `requests` – the `ReadRequest`/`WriteRequest` enums and their `kind()` labels
- `dispatch` – `dispatch_read`/`dispatch_write`, which call the table modules in `alcedo-store/src/`
- `accounts`, `folders`, `messages`, `attachments`, `search`, `remote_images`, `contacts`, `rules`, `queued_sends`, `extension`, `pgp`, `identities`, `signatures`, `snippets` – one `impl StoreHandle` block per table with that table's async methods

Adding a request means adding a variant, a `kind()` arm, a dispatch arm and a method, each under that table's section or file.

### `alcedo-ui::message_list` (message list view)

`message_list/mod.rs` holds `MessageRow`, `MessageListView`, list construction (`new_list_view`), selection and refresh bookkeeping, and population. Children (each `use super::*;`, with their items `pub(super)`):

- `columns` – column order, visibility, widths, expand column, saved sort and header chrome. The persistence wiring is `#[cfg(not(test))]` because the test build skips list setup
- `column_picker` – the show/hide and reorder popover
- `sort` – row comparators and sort-key change detection
- `cells` – the `RelmColumn` cell factories and per-row visuals
- `drag` – drag payloads and the handle-cell drag source
- `pane` – selection bar, new-mail chip, empty state, view options and `build_messages_pane`

Remaining debt:

1. `mail/mod.rs` still carries the ~700-line `init` (it must stay inside the `#[relm4::component]` impl); child components would shrink it further.
2. `handle/requests.rs` and `handle/dispatch.rs` (≈1k lines each) are still one flat enum and one match per lane. Nesting a per-table enum (`WriteRequest::Messages(MessagesWrite)`) would let each table own its variants and dispatch, but it changes every call site, so only do it if the flat layout becomes a problem.
3. `message_list/mod.rs` (≈2.5k lines, about 1.1k of which are tests) and `message_list/cells.rs` (≈1.4k lines) are the largest UI files left.

## Mail providers

Provider differences live behind two interfaces. `alcedo_core::MailProvider` (`mail_provider/`) holds pure server settings and protocol rules, and `alcedo_oauth::OAuthFlow` holds browser sign-in and token refresh. `OAuthProvider::profile` and `oauth_flow` are the only places that choose an implementation, so shared code asks about behaviour and does not check the vendor.

## Extension points

The `alcedo` crate is also a library: `alcedo::run(Extensions)` starts the app, and the `alcedo` binary calls it
with `Extensions::default()`. Another binary can depend on the public crates and register extensions. Every hook
has a do-nothing default, so the app runs unchanged with none.

- **UI** (`alcedo-ui::extension::UiExtension`, registered on the GTK thread): composer toolbar tools, Composing
  groups and whole preference pages, help sections and help-window header widgets, mail-window views
  (`MailView::Extension`, filled by `EnvelopeListKind::Extension` lists), message and address actions, and
  extension events. `MailContext` lets hooks send commands, open their views and show toasts.
- **Commands and events** (`alcedo-core::ExtensionMessage`): `UiCommand::Extension` and `SyncEvent::Extension`
  carry a JSON payload addressed to one extension by name.
- **Sync** (`alcedo-sync::extension`): `SyncExtension` handles that extension's commands with a `SyncContext`
  (store, events, a cross-folder IMAP search import). `ServerRulesBackend` lets rules run on the server; without
  one, every rule runs client-side. Its `can_publish` says which rules a publish really sends; only those are
  marked `RuleExecution::Server` and skipped by the client-side pass.
- **Store** (`alcedo-store`): an extension keeps its own tables, created with
  `schema::apply_extension_migrations`, and reads or writes them on the store's threads with
  `StoreHandle::extension_read` / `extension_write`.
- **Text** (`alcedo-i18n`): an extension names its own gettext domain in a crate-root `I18N_DOMAIN`, calls
  `alcedo_i18n::init(domain)` before `alcedo::run`, and runs `scripts/i18n.py` with its own `Project`.
- **Shared helpers:** extensions reuse `alcedo_core::redact_url_userinfo`, `alcedo_widgets::css_color`,
  `alcedo_widgets::unparent_popover_on_destroy`, `alcedo_imap::{connect_tcp, upgrade_to_tls}`,
  `alcedo_oauth::http_client`, `alcedo_ui::{toast, local_time, calendar_clock, calendar_day_select}`,
  `alcedo_ui::help::{help_button, attach_group_help, guide_text}`, `alcedo_ui::test_support` (behind the
  `test-support` feature), `scripts/lib/deb-common.sh`, `scripts/i18n.py` and `scripts/flatpak-cargo-sources.py`
  instead of keeping their own copies.

### Supported extension surface

Most modules are `pub` only because the app is split into crates, so `pub` alone does not make something part of
the extension API. **Stable** items keep their meaning and signatures across 1.x releases: additions come as new
items or as trait methods with a default, and a removal or breaking change is called out in the release notes.
**Incidental** items may change in any release; an extension that uses one pins the exact tag, and the need is a
reason to add a stable hook instead.

| Area | Stable | Incidental |
|---|---|---|
| Start-up | `alcedo::{run, Extensions}` | everything else in `alcedo` |
| UI | `alcedo_ui::extension` (`UiExtension`, `MailContext`, `AppContext`, help and view types); the shared helpers above | `add_account::{launch_auth_url, oauth_provider_config}`, `reader::open_attachment_file`, `composer::{build_composer_page, ComposerHooks, ComposerWidgets}`, `compose_html`, `menu` (`build_primary_menu_button`, action names), `settings` (`wire_paned_persist`, `reader_font_size`), `activity_log`, `notifications`, `recipient_row`, and every other module |
| Messages | `alcedo_core::ExtensionMessage`, `UiCommand::Extension`, `SyncEvent::Extension`, and the domain types they carry | |
| Sync | `alcedo_sync::extension`, `alcedo_sync::server_rules`, `alcedo_sync::oauth_token_cache` | `alcedo_sync::pgp` and the worker |
| Store | `StoreHandle` and its typed methods, `extension_read` / `extension_write`, `schema::apply_extension_migrations`; `schema::{open_connection, migrate}` for tests that need the app's schema | the per-table functions on a `Connection` (`messages`, `search`, `accounts`, `folders`, ...), `schema::{apply_pragmas, migrate_to_version}`, and table layouts |
| Protocols | `alcedo_imap::{connect_tcp, upgrade_to_tls}`, `alcedo_oauth::http_client` | other `alcedo_imap` and `alcedo_oauth` items (for example `imap_quoted_astring`) |
| Text | `alcedo_i18n`, `alcedo_ui::local_time`, `scripts/i18n.py` (`Project`, `main`) | |

The per-table store functions are the most likely to break an extension, because they follow the app's schema:
an extension that calls them inside `extension_read` / `extension_write` must be rebuilt and retested against
every new tag. The incidental UI items above are candidates for stable hooks: OAuth sign-in for non-mail
accounts (`launch_auth_url`, `oauth_provider_config`) belongs in `alcedo-oauth`, and opening an attachment, the
activity log and desktop notifications fit on `AppContext` / `MailContext`.

## v1 non-goals

Yahoo OAuth, SQLCipher, connection pools, embedded OAuth WebView, Windows/macOS, vendor telemetry.
