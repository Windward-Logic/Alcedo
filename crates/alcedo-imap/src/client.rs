//! IMAP connection: connect, login, folders, SELECT, UID FETCH/STORE, IDLE entry.

use alcedo_core::{Error, Result, SpecialUse};
use async_imap::{
    self, Client, Session,
    imap_proto::{self, RequestId, Response, Status, types::MailboxDatum},
    types::{Capabilities, Flag, Name, NameAttribute},
};
use chrono::{Datelike, NaiveDate};
use futures::StreamExt;
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

use crate::tls::{self, ImapTransport};
use crate::xoauth2::Xoauth2Authenticator;

/// Upper bound for greeting, LOGIN/AUTHENTICATE and the first CAPABILITY.
const LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// Authentication credentials for IMAP login.
#[derive(Clone)]
pub enum ImapAuth {
    Password(String),
    XOAuth2(String),
}

impl std::fmt::Debug for ImapAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImapAuth::Password(_) => f.debug_tuple("Password").field(&"***").finish(),
            ImapAuth::XOAuth2(_) => f.debug_tuple("XOAuth2").field(&"***").finish(),
        }
    }
}

/// Connection options for one authenticated IMAP session. The account worker may
/// hold two sessions (Inbox IDLE + command) when the server allows it.
#[derive(Clone)]
pub struct ImapConnectOptions {
    pub host: String,
    pub port: u16,
    pub email: String,
    pub username: String,
    pub auth: ImapAuth,
}

impl std::fmt::Debug for ImapConnectOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapConnectOptions")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("email", &self.email)
            .field("username", &self.username)
            .field("auth", &self.auth)
            .finish()
    }
}

/// Server capabilities relevant to sync.
#[derive(Clone, Debug, Default)]
pub struct ImapServerCaps {
    pub idle: bool,
    pub unselect: bool,
    pub condstore: bool,
    pub qresync: bool,
    pub special_use: bool,
    pub supports_move: bool,
    pub uidplus: bool,
    /// Gmail's `X-GM-EXT-1`. Labels are folders, and `DELETE` does not follow RFC 3501.
    pub gmail_ext: bool,
}

/// Folder returned from `LIST`.
#[derive(Clone, Debug)]
pub struct ListedFolder {
    pub imap_name: String,
    pub delimiter: String,
    pub special: SpecialUse,
    pub selectable: bool,
}

/// UIDPLUS `[APPENDUID]` from a successful `APPEND`, when the server sent one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppendedMail {
    pub uidvalidity: Option<u32>,
    pub uid: Option<u32>,
}

/// Result of `SELECT` / `SELECT (CONDSTORE)`.
#[derive(Clone, Debug)]
pub struct SelectedMailbox {
    pub uidvalidity: Option<u32>,
    pub uidnext: Option<u32>,
    pub highest_modseq: Option<u64>,
    pub exists: u32,
}

/// `STATUS` data. `unseen` is the RFC 3501 STATUS UNSEEN count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxStatus {
    pub messages: u32,
    pub uidnext: Option<u32>,
    pub uidvalidity: Option<u32>,
    pub unseen: Option<u32>,
    pub highest_modseq: Option<u64>,
}

pub fn status_poll_items(include_modseq: bool) -> &'static str {
    if include_modseq {
        "(MESSAGES UIDNEXT UIDVALIDITY UNSEEN HIGHESTMODSEQ)"
    } else {
        "(MESSAGES UIDNEXT UIDVALIDITY UNSEEN)"
    }
}

/// One authenticated IMAP session. The account worker may hold two per account.
pub struct ImapClient {
    session: Session<ImapTransport>,
    capabilities: ImapServerCaps,
    email: String,
    /// Mailbox the server has selected on this connection, as far as we know.
    /// `None` after a failed, cancelled or unselected SELECT. The client owns
    /// this so it cannot drift from the connection the way a copy kept by
    /// callers can.
    selected: Option<String>,
}

impl ImapClient {
    /// Connect over TLS, authenticate, and run `CAPABILITY`.
    pub async fn connect(opts: ImapConnectOptions) -> Result<Self> {
        let stream = tls::connect_tls(&opts.host, opts.port).await?;
        let host = opts.host.clone();
        // TCP keepalive cannot catch a server that is alive but never sends its
        // greeting or answers LOGIN, so bound the whole handshake.
        tokio::time::timeout(LOGIN_TIMEOUT, Self::greet_and_login(stream, opts))
            .await
            .map_err(|_| {
                Error::Network(format!("connection timed out during IMAP login to {host}"))
            })?
    }

    async fn greet_and_login(stream: ImapTransport, opts: ImapConnectOptions) -> Result<Self> {
        let mut client = Client::new(stream);

        client
            .read_response()
            .await
            .map_err(|e| Error::Network(format!("imap greeting: {e}")))?
            .ok_or_else(|| Error::Imap("unexpected end of stream at greeting".to_string()))?;

        let session = match opts.auth {
            ImapAuth::Password(password) => client
                .login(&opts.username, &password)
                .await
                .map_err(|(e, _)| Error::Imap(format!("login failed: {e}")))?,
            ImapAuth::XOAuth2(token) => {
                let auth = Xoauth2Authenticator::new(&opts.username, token);
                client
                    .authenticate("XOAUTH2", auth)
                    .await
                    .map_err(|(e, _)| Error::Imap(format!("XOAUTH2 authenticate failed: {e}")))?
            }
        };

        let mut client = Self {
            session,
            capabilities: ImapServerCaps::default(),
            email: opts.email,
            selected: None,
        };
        client.refresh_capabilities().await?;
        Ok(client)
    }

    pub fn email(&self) -> &str {
        &self.email
    }

    pub fn capabilities(&self) -> &ImapServerCaps {
        &self.capabilities
    }

    /// The mailbox currently selected on this connection, if known.
    pub fn selected_mailbox(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    pub fn session_mut(&mut self) -> &mut Session<ImapTransport> {
        &mut self.session
    }

    /// Re-run `CAPABILITY` and update cached flags.
    pub async fn refresh_capabilities(&mut self) -> Result<()> {
        let caps = self
            .session
            .capabilities()
            .await
            .map_err(|e| Error::Imap(format!("CAPABILITY failed: {e}")))?;
        self.capabilities = parse_capabilities(&caps);
        debug!(?self.capabilities, "imap capabilities");
        Ok(())
    }

    /// `LIST "" "*"` with SPECIAL-USE / XLIST attribute mapping.
    pub async fn list_folders(&mut self) -> Result<Vec<ListedFolder>> {
        let mut stream = self
            .session
            .list(None, Some("*"))
            .await
            .map_err(|e| Error::Imap(format!("LIST failed: {e}")))?;

        let mut folders = Vec::new();
        while let Some(item) = stream.next().await {
            let name = item.map_err(|e| Error::Imap(format!("LIST response: {e}")))?;
            folders.push(name_to_folder(&name));
        }
        apply_trash_name_fallback(&mut folders);
        Ok(folders)
    }

    /// `SELECT` with `CONDSTORE` when advertised.
    pub async fn select_mailbox(&mut self, mailbox: &str) -> Result<SelectedMailbox> {
        debug!(mailbox, "wire: SELECT");
        // Forget the old selection first: a SELECT that fails (the server drops
        // back to the authenticated state) or is cancelled by a timeout leaves
        // no known mailbox, so the next caller selects again.
        self.selected = None;
        let mailbox_info = if self.capabilities.condstore {
            self.session
                .select_condstore(mailbox)
                .await
                .map_err(|e| Error::Imap(format!("SELECT (CONDSTORE) {mailbox}: {e}")))?
        } else {
            self.session
                .select(mailbox)
                .await
                .map_err(|e| Error::Imap(format!("SELECT {mailbox}: {e}")))?
        };

        self.selected = Some(mailbox.to_string());
        Ok(SelectedMailbox {
            uidvalidity: mailbox_info.uid_validity,
            uidnext: mailbox_info.uid_next,
            highest_modseq: mailbox_info.highest_modseq,
            exists: mailbox_info.exists,
        })
    }

    /// `STATUS mailbox (UIDNEXT)` without changing the selected mailbox.
    pub async fn status_uidnext(&mut self, mailbox: &str) -> Result<Option<u32>> {
        let mailbox_info = self
            .session
            .status(mailbox, "(UIDNEXT)")
            .await
            .map_err(|e| Error::Imap(format!("STATUS {mailbox} (UIDNEXT): {e}")))?;
        Ok(mailbox_info.uid_next)
    }

    /// `STATUS` for the folder poll. Does not change the selected mailbox.
    pub async fn status_mailbox(
        &mut self,
        mailbox: &str,
        include_modseq: bool,
    ) -> Result<MailboxStatus> {
        let mailbox_info = self
            .session
            .status(mailbox, status_poll_items(include_modseq))
            .await
            .map_err(|e| Error::Imap(format!("STATUS {mailbox}: {e}")))?;
        Ok(MailboxStatus {
            messages: mailbox_info.exists,
            uidnext: mailbox_info.uid_next,
            uidvalidity: mailbox_info.uid_validity,
            unseen: mailbox_info.unseen,
            highest_modseq: mailbox_info.highest_modseq,
        })
    }

    pub async fn create_mailbox(&mut self, mailbox: &str) -> Result<()> {
        self.session
            .create(mailbox)
            .await
            .map_err(|e| Error::Imap(format!("CREATE {mailbox}: {e}")))?;
        Ok(())
    }

    pub async fn rename_mailbox(&mut self, from: &str, to: &str) -> Result<()> {
        self.session
            .rename(from, to)
            .await
            .map_err(|e| Error::Imap(format!("RENAME {from} {to}: {e}")))?;
        Ok(())
    }

    pub async fn delete_mailbox(&mut self, mailbox: &str) -> Result<()> {
        self.session
            .delete(mailbox)
            .await
            .map_err(|e| Error::Imap(format!("DELETE {mailbox}: {e}")))?;
        Ok(())
    }

    /// `NOOP` keepalive for an idle command session.
    pub async fn noop(&mut self) -> Result<()> {
        self.session
            .noop()
            .await
            .map_err(|e| Error::Imap(format!("NOOP: {e}")))?;
        Ok(())
    }

    /// `UNSELECT` when advertised; otherwise returns `Error::Imap` so callers
    /// can fall back to SELECT-ing another mailbox.
    pub async fn unselect(&mut self) -> Result<()> {
        if !self.capabilities.unselect {
            return Err(Error::Imap("UNSELECT not advertised".to_string()));
        }
        self.session
            .run_command_and_check_ok("UNSELECT")
            .await
            .map_err(|e| Error::Imap(format!("UNSELECT: {e}")))?;
        self.selected = None;
        Ok(())
    }

    /// `UID SEARCH` with arbitrary criteria for the selected mailbox.
    pub async fn uid_search(&mut self, criteria: &str) -> Result<Vec<u32>> {
        let criteria = search_criteria_with_charset(criteria);
        let uids = self
            .session
            .uid_search(&criteria)
            .await
            .map_err(|e| Error::Imap(format!("UID SEARCH {criteria}: {e}")))?;
        Ok(uids.into_iter().collect())
    }

    /// `UID SEARCH ALL` for the selected mailbox.
    pub async fn uid_search_all(&mut self) -> Result<Vec<u32>> {
        self.uid_search("ALL").await
    }

    /// True when this UID still exists in the selected mailbox.
    ///
    /// RFC 6851 `UID MOVE` of a UID that is not in the selected mailbox is
    /// success with no action. Callers must not hide the local copy unless
    /// this returns `false`.
    pub async fn uid_is_present(&mut self, uid: u32) -> Result<bool> {
        let uids = self
            .session
            .uid_search(format!("UID {uid}"))
            .await
            .map_err(|e| Error::Imap(format!("UID SEARCH UID {uid}: {e}")))?;
        Ok(uids.iter().any(|found| *found == uid))
    }

    /// `UID STORE` for junk classification keywords on a UID set.
    pub async fn uid_store_junk_keywords(&mut self, uid_set: &str, junk: bool) -> Result<()> {
        for query in junk_keyword_store_commands(junk) {
            let mut stream = self
                .session
                .uid_store(uid_set, query)
                .await
                .map_err(|e| Error::Imap(format!("UID STORE {uid_set}: {e}")))?;
            while stream.next().await.is_some() {}
        }
        Ok(())
    }

    /// `UID STORE` for `\Seen` / `\Flagged` on a UID set (e.g. `"1:*"`).
    pub async fn uid_store_flags_set(
        &mut self,
        uid_set: &str,
        seen: Option<bool>,
        flagged: Option<bool>,
    ) -> Result<()> {
        let mut parts = Vec::new();
        if let Some(true) = seen {
            parts.push("+FLAGS.SILENT (\\Seen)".to_string());
        } else if let Some(false) = seen {
            parts.push("-FLAGS.SILENT (\\Seen)".to_string());
        }
        if let Some(true) = flagged {
            parts.push("+FLAGS.SILENT (\\Flagged)".to_string());
        } else if let Some(false) = flagged {
            parts.push("-FLAGS.SILENT (\\Flagged)".to_string());
        }

        if parts.is_empty() {
            return Ok(());
        }

        for query in parts {
            let mut stream = self
                .session
                .uid_store(uid_set, query)
                .await
                .map_err(|e| Error::Imap(format!("UID STORE {uid_set}: {e}")))?;
            while stream.next().await.is_some() {}
        }
        Ok(())
    }

    /// `UID STORE` for `\Seen` / `\Flagged` on a single UID.
    pub async fn uid_store_flags(
        &mut self,
        uid: u32,
        seen: Option<bool>,
        flagged: Option<bool>,
    ) -> Result<()> {
        self.uid_store_flags_set(&uid.to_string(), seen, flagged)
            .await
    }

    /// Mark every message in the selected mailbox as `\Seen`.
    pub async fn uid_store_seen_all(&mut self) -> Result<()> {
        match self.uid_store_flags_set("1:*", Some(true), None).await {
            Err(Error::Imap(message)) if imap_empty_set_error(&message) => Ok(()),
            other => other,
        }
    }

    /// Flag every message `\Deleted` and expunge the mailbox.
    // Intentionally expunges all \Deleted mail (Empty Trash/Junk); no bystander protection.
    pub async fn uid_expunge_all(&mut self) -> Result<()> {
        debug!("wire: UID STORE 1:* +FLAGS.SILENT (\\Deleted) then EXPUNGE (uid_expunge_all)");
        let uid_set = "1:*";
        {
            let store_result = self
                .session
                .uid_store(uid_set, "+FLAGS.SILENT (\\Deleted)")
                .await;
            match store_result {
                Ok(mut stream) => while stream.next().await.is_some() {},
                Err(err) => {
                    if imap_empty_set_error(&err.to_string()) {
                        return Ok(());
                    }
                    return Err(Error::Imap(format!("UID STORE {uid_set} \\Deleted: {err}")));
                }
            }
        }

        let expunge_result = self.session.expunge().await;
        match expunge_result {
            Ok(stream) => {
                tokio::pin!(stream);
                while stream.next().await.is_some() {}
                Ok(())
            }
            Err(err) => {
                if imap_empty_set_error(&err.to_string()) {
                    Ok(())
                } else {
                    Err(Error::Imap(format!("EXPUNGE: {err}")))
                }
            }
        }
    }

    pub async fn uid_copy(&mut self, uid: u32, dest_mailbox: &str) -> Result<()> {
        debug!(uid, dest_mailbox, "wire: UID COPY");
        self.session
            .uid_copy(uid.to_string(), dest_mailbox)
            .await
            .map_err(|e| Error::Imap(format!("UID COPY {uid}: {e}")))?;
        Ok(())
    }

    /// `UID MOVE` when the server advertises MOVE, otherwise `UID COPY` + delete.
    pub async fn uid_move_or_copy_delete(&mut self, uid: u32, dest_mailbox: &str) -> Result<()> {
        self.uid_move_or_copy_delete_set(&uid.to_string(), dest_mailbox)
            .await
    }

    /// `UID MOVE` / `UID COPY` + delete for a UID set string.
    ///
    /// Some servers advertise MOVE, return tagged OK, and still leave the
    /// source (label-style COPY, delayed expunge, or a no-op). Others, including
    /// Migadu, answer `UID MOVE` with `NO [UNAVAILABLE]` while COPY still works.
    /// Finish leftovers with COPY + `\Deleted` + expunge so delete/move can succeed.
    pub async fn uid_move_or_copy_delete_set(
        &mut self,
        uid_set: &str,
        dest_mailbox: &str,
    ) -> Result<()> {
        self.move_or_copy_delete_set(uid_set, dest_mailbox)
            .await
            .map(|_| ())
    }

    /// Move one UID and confirm it is no longer in the selected mailbox.
    ///
    /// The MOVE path already searches for leftovers, so a clean result there
    /// is the confirmation and a second `UID SEARCH` would repeat it. Only a
    /// fallback or the COPY-and-delete path still needs one.
    pub async fn uid_move_or_copy_delete_verified(
        &mut self,
        uid: u32,
        dest_mailbox: &str,
    ) -> Result<()> {
        let verified = self
            .move_or_copy_delete_set(&uid.to_string(), dest_mailbox)
            .await?;
        if !verified && self.uid_is_present(uid).await? {
            return Err(Error::Imap(format!(
                "UID MOVE {uid} to {dest_mailbox} left the message in the source folder"
            )));
        }
        Ok(())
    }

    /// Returns true when a `UID SEARCH` after the MOVE already showed the set gone.
    async fn move_or_copy_delete_set(&mut self, uid_set: &str, dest_mailbox: &str) -> Result<bool> {
        if self.capabilities.supports_move {
            debug!(uid_set, dest_mailbox, "wire: UID MOVE");
            match self.session.uid_mv(uid_set, dest_mailbox).await {
                Ok(()) => {}
                Err(err) if imap_move_can_fall_back(&err) => {
                    warn!(
                        error = %err,
                        uid_set,
                        dest_mailbox,
                        "UID MOVE refused; falling back to COPY + expunge"
                    );
                }
                Err(err) => {
                    return Err(Error::Imap(format!("UID MOVE {uid_set}: {err}")));
                }
            }
            let leftover = self.uid_search(&format!("UID {uid_set}")).await?;
            if leftover.is_empty() {
                return Ok(true);
            }
            warn!(
                uid_set,
                dest_mailbox,
                leftover = leftover.len(),
                "UID MOVE left source UIDs; falling back to COPY + expunge"
            );
            self.copy_delete_uids(&leftover, dest_mailbox).await?;
            Ok(false)
        } else {
            debug!(
                uid_set,
                dest_mailbox, "wire: UID COPY + delete fallback (no MOVE capability)"
            );
            self.uid_copy_then_delete(uid_set, dest_mailbox).await?;
            Ok(false)
        }
    }

    /// COPY + expunge. A refused set is retried one UID at a time so one stuck
    /// message does not keep the rest of the folder.
    ///
    /// Once a COPY succeeded, only the delete is retried. Copying again would
    /// leave a second copy of every message in the destination.
    async fn copy_delete_uids(&mut self, uids: &[u32], dest_mailbox: &str) -> Result<()> {
        let mut unique = uids.to_vec();
        unique.sort_unstable();
        unique.dedup();
        let mut errors = Vec::new();
        for chunk in unique.chunks(250) {
            let set = format_uid_set(chunk);
            match self.uid_copy_only(&set, dest_mailbox).await {
                Ok(()) => self.delete_copied_chunk(&set, chunk, &mut errors).await,
                Err(err) if chunk.len() == 1 => errors.push(imap_error_detail(&err)),
                Err(_) => {
                    warn!(
                        uid_set = %set,
                        dest_mailbox,
                        "UID COPY set failed; retrying one UID at a time"
                    );
                    for uid in chunk {
                        if let Err(err) = self
                            .uid_copy_then_delete(&uid.to_string(), dest_mailbox)
                            .await
                        {
                            errors.push(imap_error_detail(&err));
                        }
                    }
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(Error::Imap(errors.join("; ")))
        }
    }

    /// Delete a chunk that is already copied, falling back to one UID at a time.
    async fn delete_copied_chunk(&mut self, set: &str, chunk: &[u32], errors: &mut Vec<String>) {
        let Err(err) = self.uid_delete_set(set).await else {
            return;
        };
        if chunk.len() == 1 {
            errors.push(imap_error_detail(&err));
            return;
        }
        warn!(uid_set = %set, error = %err, "delete after COPY failed; retrying one UID at a time");
        for uid in chunk {
            if let Err(err) = self.uid_delete_set(&uid.to_string()).await {
                errors.push(imap_error_detail(&err));
            }
        }
    }

    async fn uid_copy_only(&mut self, uid_set: &str, dest_mailbox: &str) -> Result<()> {
        self.session
            .uid_copy(uid_set, dest_mailbox)
            .await
            .map_err(|e| Error::Imap(format!("UID COPY {uid_set}: {e}")))
    }

    async fn uid_copy_then_delete(&mut self, uid_set: &str, dest_mailbox: &str) -> Result<()> {
        self.uid_copy_only(uid_set, dest_mailbox).await?;
        self.uid_delete_set(uid_set).await
    }

    /// Flag `\Deleted` and expunge this UID from the selected mailbox.
    pub async fn uid_delete(&mut self, uid: u32) -> Result<()> {
        self.uid_delete_set(&uid.to_string()).await
    }

    /// `APPEND` RFC822 bytes with `\Draft \Seen` flags.
    ///
    /// Some servers reject `\Draft` on APPEND; retry with `\Seen` only in that case.
    ///
    /// UIDPLUS servers return the new UID in `[APPENDUID]`. Callers must use that
    /// instead of searching: a follow-up `UID SEARCH` often misses the message
    /// that was just appended.
    pub async fn append_rfc822(&mut self, mailbox: &str, rfc822: &[u8]) -> Result<AppendedMail> {
        match self
            .append_rfc822_with_flags(mailbox, rfc822, r"(\Draft \Seen)")
            .await
        {
            Ok(appended) => Ok(appended),
            Err(err) if append_rejected_draft_flag(&err) => {
                debug!(error = %err, mailbox, "APPEND \\Draft rejected; retrying without it");
                self.append_rfc822_with_flags(mailbox, rfc822, r"(\Seen)")
                    .await
            }
            Err(err) => Err(err),
        }
    }

    /// `APPEND` RFC822 bytes with the given IMAP flags string, e.g. `(\Seen)`.
    pub async fn append_rfc822_with_flags(
        &mut self,
        mailbox: &str,
        rfc822: &[u8],
        flags: &str,
    ) -> Result<AppendedMail> {
        debug!(mailbox, flags, bytes = rfc822.len(), "wire: APPEND");
        let quoted = imap_quoted_astring(mailbox);
        let flags_part = if flags.is_empty() {
            String::new()
        } else {
            format!(" {flags}")
        };
        let id = self
            .session
            .run_command(&format!("APPEND {quoted}{flags_part} {{{}}}", rfc822.len()))
            .await
            .map_err(|e| Error::Imap(format!("APPEND {mailbox}: {e}")))?;

        wait_for_append_continue(&mut self.session, &id, mailbox).await?;

        self.session
            .get_mut()
            .write_all(rfc822)
            .await
            .map_err(|e| Error::Imap(format!("APPEND {mailbox}: {e}")))?;
        self.session
            .get_mut()
            .write_all(b"\r\n")
            .await
            .map_err(|e| Error::Imap(format!("APPEND {mailbox}: {e}")))?;
        self.session
            .get_mut()
            .flush()
            .await
            .map_err(|e| Error::Imap(format!("APPEND {mailbox}: {e}")))?;

        wait_for_append_done(&mut self.session, &id, mailbox).await
    }

    /// Fetch `INTERNALDATE` for a single UID in the selected mailbox.
    pub async fn uid_fetch_internaldate(
        &mut self,
        uid: u32,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        let mut stream = self
            .session
            .uid_fetch(uid.to_string(), "INTERNALDATE")
            .await
            .map_err(|e| Error::Imap(format!("UID FETCH INTERNALDATE {uid}: {e}")))?;

        // Drain the stream: returning early leaves the rest of this
        // command's responses to be read as the next command's.
        let mut found = None;
        while let Some(item) = stream.next().await {
            let fetch =
                item.map_err(|e| Error::Imap(format!("UID FETCH INTERNALDATE item: {e}")))?;
            if found.is_none() && fetch.uid.is_none_or(|got| got == uid) {
                found = fetch
                    .internal_date()
                    .map(|dt| dt.with_timezone(&chrono::Utc));
            }
        }
        Ok(found)
    }

    /// Fetch `(seen, flagged)` for a single UID in the selected mailbox.
    pub async fn uid_fetch_flags(&mut self, uid: u32) -> Result<Option<(bool, bool)>> {
        let mut stream = self
            .session
            .uid_fetch(uid.to_string(), "(FLAGS)")
            .await
            .map_err(|e| Error::Imap(format!("UID FETCH FLAGS {uid}: {e}")))?;

        // Drain the stream (see `uid_fetch_internaldate`) and ignore
        // unsolicited FETCHes for other messages.
        let mut found = None;
        while let Some(item) = stream.next().await {
            let fetch = item.map_err(|e| Error::Imap(format!("UID FETCH FLAGS item: {e}")))?;
            if found.is_none() && fetch.uid.is_none_or(|got| got == uid) {
                found = Some((fetch_is_seen(&fetch), fetch_is_flagged(&fetch)));
            }
        }
        Ok(found)
    }

    /// Messages other than `uid_set` currently flagged \Deleted in the
    /// selected mailbox. Used to protect bystanders from a non-UIDPLUS
    /// whole-mailbox EXPUNGE. Returns empty in the common case.
    async fn deleted_bystanders(&mut self, uid_set: &str) -> Result<Vec<u32>> {
        self.uid_search(&imap_deleted_bystanders_search(uid_set))
            .await
    }

    /// Flag `\Deleted` and expunge a UID set from the selected mailbox.
    pub async fn uid_delete_set(&mut self, uid_set: &str) -> Result<()> {
        debug!(
            uid_set,
            uidplus = self.capabilities.uidplus,
            "wire: UID STORE +FLAGS.SILENT (\\Deleted) then {}",
            if self.capabilities.uidplus {
                "UID EXPUNGE"
            } else {
                "EXPUNGE (whole mailbox!)"
            }
        );
        {
            let store_result = self
                .session
                .uid_store(uid_set, "+FLAGS.SILENT (\\Deleted)")
                .await;
            match store_result {
                Ok(mut stream) => while stream.next().await.is_some() {},
                Err(err) => {
                    if imap_empty_set_error(&err.to_string()) {
                        return Ok(());
                    }
                    return Err(Error::Imap(format!("UID STORE {uid_set} \\Deleted: {err}")));
                }
            }
        }

        if self.capabilities.uidplus {
            let expunge_result = self.session.uid_expunge(uid_set.to_string()).await;
            match expunge_result {
                Ok(stream) => {
                    tokio::pin!(stream);
                    while stream.next().await.is_some() {}
                    Ok(())
                }
                Err(err) => {
                    if imap_empty_set_error(&err.to_string()) {
                        Ok(())
                    } else {
                        Err(Error::Imap(format!("UID EXPUNGE {uid_set}: {err}")))
                    }
                }
            }
        } else {
            let bystanders = match self.deleted_bystanders(uid_set).await {
                Ok(bystanders) => bystanders,
                Err(err) => {
                    warn!(
                        uid_set,
                        error = %err,
                        "deleted bystander SEARCH failed; falling back to whole-mailbox EXPUNGE"
                    );
                    Vec::new()
                }
            };

            if !bystanders.is_empty() {
                for chunk_set in format_uid_set_chunks(&bystanders, 250) {
                    let store_result = self
                        .session
                        .uid_store(&chunk_set, "-FLAGS.SILENT (\\Deleted)")
                        .await;
                    match store_result {
                        Ok(mut stream) => while stream.next().await.is_some() {},
                        Err(err) => {
                            return Err(Error::Imap(format!(
                                "UID STORE {chunk_set} -\\Deleted: {err}"
                            )));
                        }
                    }
                }
            }

            {
                let expunge_result = self.session.expunge().await;
                match expunge_result {
                    Ok(stream) => {
                        tokio::pin!(stream);
                        while stream.next().await.is_some() {}
                    }
                    Err(err) => {
                        if !imap_empty_set_error(&err.to_string()) {
                            return Err(Error::Imap(format!("EXPUNGE: {err}")));
                        }
                    }
                }
            }

            if !bystanders.is_empty() {
                for chunk_set in format_uid_set_chunks(&bystanders, 250) {
                    let store_result = self
                        .session
                        .uid_store(&chunk_set, "+FLAGS.SILENT (\\Deleted)")
                        .await;
                    match store_result {
                        Ok(mut stream) => while stream.next().await.is_some() {},
                        Err(err) => {
                            warn!(
                                chunk_set,
                                error = %err,
                                "re-flag bystander \\Deleted failed after successful expunge"
                            );
                            return Ok(());
                        }
                    }
                }
            }

            Ok(())
        }
    }

    /// Consume the client and return the underlying session for IDLE.
    pub fn into_session(self) -> Session<ImapTransport> {
        self.session
    }

    /// Re-wrap a session returned from [`IdleSession::done`] (or similar) into a client.
    pub fn from_session(
        session: Session<ImapTransport>,
        email: String,
        capabilities: ImapServerCaps,
        selected: Option<String>,
    ) -> Self {
        Self {
            session,
            capabilities,
            email,
            selected,
        }
    }
}

fn parse_capabilities(caps: &Capabilities) -> ImapServerCaps {
    let parsed = ImapServerCaps {
        idle: caps.has_str("IDLE"),
        unselect: caps.has_str("UNSELECT"),
        condstore: caps.has_str("CONDSTORE"),
        qresync: caps.has_str("QRESYNC"),
        special_use: caps.has_str("SPECIAL-USE") || caps.has_str("XLIST"),
        supports_move: caps.has_str("MOVE"),
        uidplus: caps.has_str("UIDPLUS"),
        gmail_ext: caps.has_str("X-GM-EXT-1"),
    };
    debug!(?parsed, "wire: negotiated capabilities");
    parsed
}

fn name_to_folder(name: &Name) -> ListedFolder {
    let delimiter = name.delimiter().unwrap_or("/").to_string();
    let imap_name = name.name().to_string();
    let selectable = !name.attributes().contains(&NameAttribute::NoSelect);
    let special = map_special_use(name.attributes(), &imap_name, &delimiter);

    ListedFolder {
        imap_name,
        delimiter,
        special,
        selectable,
    }
}

fn map_special_use(attrs: &[NameAttribute<'_>], imap_name: &str, delimiter: &str) -> SpecialUse {
    if imap_name.eq_ignore_ascii_case("INBOX") {
        return SpecialUse::Inbox;
    }

    let leaf = match delimiter.chars().next() {
        Some(sep) => imap_name
            .rsplit_once(sep)
            .map_or(imap_name, |(_, leaf)| leaf),
        None => imap_name,
    };
    if leaf.eq_ignore_ascii_case("Snoozed") {
        return SpecialUse::Snoozed;
    }

    for attr in attrs {
        match attr {
            NameAttribute::All => return SpecialUse::All,
            NameAttribute::Archive => return SpecialUse::Archive,
            NameAttribute::Drafts => return SpecialUse::Drafts,
            NameAttribute::Junk => return SpecialUse::Junk,
            NameAttribute::Sent => return SpecialUse::Sent,
            NameAttribute::Trash => return SpecialUse::Trash,
            NameAttribute::Extension(ext) => {
                if ext.eq_ignore_ascii_case("\\Inbox") {
                    return SpecialUse::Inbox;
                }
            }
            _ => {}
        }
    }

    SpecialUse::Other
}

/// Names servers commonly use for Trash when they advertise no `\\Trash`
/// attribute. Matched case-insensitively against the folder's leaf name.
const TRASH_NAME_FALLBACKS: [&str; 5] = [
    "Trash",
    "Deleted Items",
    "Deleted Messages",
    "Deleted",
    "Bin",
];

/// Mark a Trash folder by name, but only when the server flagged none, so a
/// real `\\Trash` attribute always wins. A top-level match beats a nested one
/// and earlier names in `TRASH_NAME_FALLBACKS` beat later ones.
fn apply_trash_name_fallback(folders: &mut [ListedFolder]) {
    if folders.iter().any(|f| f.special == SpecialUse::Trash) {
        return;
    }
    let best = folders
        .iter()
        .enumerate()
        .filter(|(_, f)| f.selectable && f.special == SpecialUse::Other)
        .filter_map(|(index, f)| {
            let (leaf, nested) = match f.delimiter.chars().next() {
                Some(sep) => match f.imap_name.rsplit_once(sep) {
                    Some((_, leaf)) => (leaf, true),
                    None => (f.imap_name.as_str(), false),
                },
                None => (f.imap_name.as_str(), false),
            };
            let rank = TRASH_NAME_FALLBACKS
                .iter()
                .position(|name| leaf.eq_ignore_ascii_case(name))?;
            Some(((nested, rank), index))
        })
        .min_by_key(|(key, _)| *key);
    if let Some((_, index)) = best {
        folders[index].special = SpecialUse::Trash;
    }
}

/// Parse an unsolicited IDLE `EXISTS` count when possible.
pub fn parse_exists_count(resp: &imap_proto::Response<'_>) -> Option<u32> {
    match resp {
        imap_proto::Response::MailboxData(MailboxDatum::Exists(count)) => Some(*count),
        _ => None,
    }
}

/// Parse an unsolicited IDLE `EXPUNGE` sequence number when possible.
pub fn parse_expunge_seq(resp: &imap_proto::Response<'_>) -> Option<u32> {
    match resp {
        imap_proto::Response::Expunge(seq) => Some(*seq),
        _ => None,
    }
}

/// Returns true when a FETCH response indicates `\Seen`.
pub fn fetch_is_seen(fetch: &async_imap::types::Fetch) -> bool {
    fetch.flags().any(|f| matches!(f, Flag::Seen))
}

/// Returns true when a FETCH response indicates `\Flagged`.
pub fn fetch_is_flagged(fetch: &async_imap::types::Fetch) -> bool {
    fetch.flags().any(|f| matches!(f, Flag::Flagged))
}

/// Build a UID FETCH query using RFC 4551 `CHANGEDSINCE` when CONDSTORE is advertised.
///
/// `CHANGEDSINCE` requires CONDSTORE, not QRESYNC. Gating on QRESYNC forced a full
/// `1:*` envelope refetch on every sync for servers that advertise CONDSTORE only.
pub fn uid_fetch_query_with_changedsince(
    base_query: &str,
    modseq: Option<u64>,
    caps: &ImapServerCaps,
) -> String {
    if caps.condstore
        && let Some(modseq) = modseq
    {
        return format!("{base_query} (CHANGEDSINCE {modseq})");
    }
    base_query.to_string()
}

/// RFC 3501 `date-text` for IMAP SEARCH (unpadded day, three-letter month).
pub fn imap_date_text(date: NaiveDate) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}-{}-{}",
        date.day(),
        MONTHS[(date.month() as usize) - 1],
        date.year()
    )
}

/// Inclusive `on or before` filter: `BEFORE` the day after `through`.
pub fn imap_search_before_inclusive(through: NaiveDate) -> String {
    match through.succ_opt() {
        Some(next) => format!("BEFORE {}", imap_date_text(next)),
        None => "ALL".to_string(),
    }
}

/// Sort unique UIDs, compress consecutive ranges, and split into chunks of at most `chunk` UIDs.
pub fn format_uid_set_chunks(uids: &[u32], chunk: usize) -> Vec<String> {
    if uids.is_empty() || chunk == 0 {
        return Vec::new();
    }

    let mut unique = uids.to_vec();
    unique.sort_unstable();
    unique.dedup();

    unique.chunks(chunk).map(format_uid_set).collect()
}

fn format_uid_set(uids: &[u32]) -> String {
    let mut parts = Vec::new();
    let mut range_start = uids[0];
    let mut range_end = uids[0];

    for &uid in &uids[1..] {
        if uid == range_end + 1 {
            range_end = uid;
        } else {
            parts.push(format_uid_range(range_start, range_end));
            range_start = uid;
            range_end = uid;
        }
    }
    parts.push(format_uid_range(range_start, range_end));
    parts.join(",")
}

fn format_uid_range(start: u32, end: u32) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}:{end}")
    }
}

/// Tagged `NO` / `BAD` leaves the session usable. Connection loss does not.
fn imap_move_can_fall_back(err: &async_imap::error::Error) -> bool {
    matches!(
        err,
        async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_)
    )
}

fn imap_error_detail(err: &Error) -> String {
    match err {
        Error::Imap(message) => message.clone(),
        other => other.to_string(),
    }
}

fn imap_empty_set_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("no matching") || lower.contains("no messages")
}

const APPEND_UNTAGGED_LIMIT: usize = 32;

async fn wait_for_append_continue(
    session: &mut Session<ImapTransport>,
    id: &RequestId,
    mailbox: &str,
) -> Result<()> {
    for _ in 0..APPEND_UNTAGGED_LIMIT {
        let Some(res) = session
            .read_response()
            .await
            .map_err(|e| Error::Imap(format!("APPEND {mailbox}: {e}")))?
        else {
            return Err(Error::Imap(format!("APPEND {mailbox}: connection lost")));
        };
        match res.parsed() {
            Response::Continue { .. } => return Ok(()),
            Response::Done {
                tag,
                status,
                information,
                ..
            } if tag == id => {
                return Err(append_done_error(mailbox, status, information.as_deref()));
            }
            _ => {}
        }
    }
    Err(Error::Imap(format!(
        "APPEND {mailbox}: no continuation from server"
    )))
}

async fn wait_for_append_done(
    session: &mut Session<ImapTransport>,
    id: &RequestId,
    mailbox: &str,
) -> Result<AppendedMail> {
    for _ in 0..APPEND_UNTAGGED_LIMIT {
        let Some(res) = session
            .read_response()
            .await
            .map_err(|e| Error::Imap(format!("APPEND {mailbox}: {e}")))?
        else {
            return Err(Error::Imap(format!("APPEND {mailbox}: connection lost")));
        };
        match res.parsed() {
            Response::Done {
                tag,
                status,
                code,
                information,
            } if tag == id => {
                return match status {
                    Status::Ok => Ok(appended_mail_from_code(code.as_ref())),
                    _ => Err(append_done_error(mailbox, status, information.as_deref())),
                };
            }
            _ => {}
        }
    }
    Err(Error::Imap(format!(
        "APPEND {mailbox}: no tagged completion from server"
    )))
}

fn appended_mail_from_code(code: Option<&imap_proto::ResponseCode<'_>>) -> AppendedMail {
    match code {
        Some(imap_proto::ResponseCode::AppendUid(uidvalidity, members)) => AppendedMail {
            uidvalidity: Some(*uidvalidity),
            uid: first_appended_uid(members),
        },
        _ => AppendedMail::default(),
    }
}

fn first_appended_uid(members: &[imap_proto::UidSetMember]) -> Option<u32> {
    let uid = match members.first()? {
        imap_proto::UidSetMember::Uid(uid) => *uid,
        imap_proto::UidSetMember::UidRange(range) => *range.start(),
    };
    (uid != 0).then_some(uid)
}

fn append_done_error(mailbox: &str, status: &Status, information: Option<&str>) -> Error {
    let kind = match status {
        Status::Bad => "BAD",
        Status::No => "NO",
        Status::Bye => "BYE",
        _ => "error",
    };
    let info = information.unwrap_or("").trim();
    if info.is_empty() {
        Error::Imap(format!("APPEND {mailbox}: {kind}"))
    } else {
        Error::Imap(format!("APPEND {mailbox}: {kind} {info}"))
    }
}

fn append_rejected_draft_flag(err: &Error) -> bool {
    let lower = err.to_string().to_ascii_lowercase();
    lower.contains("\\draft")
        || lower.contains("flag")
        || lower.contains("noperm")
        || lower.contains("not allowed")
}

/// Prefix `CHARSET UTF-8` when the criteria carry non-ASCII text.
///
/// Without a charset the server must read search strings as US-ASCII, so a
/// name or address with accents is rejected or never matches.
fn search_criteria_with_charset(criteria: &str) -> String {
    if criteria.is_ascii() {
        criteria.to_string()
    } else {
        format!("CHARSET UTF-8 {criteria}")
    }
}

/// Quote a value for IMAP `astring` / quoted-string use in SEARCH criteria.
///
/// CR, LF and NUL cannot appear in a quoted string and would end the command
/// line, letting a crafted `Message-ID` or address inject another command, so
/// they are dropped. (async-imap's `uid_search` does not validate its input.)
pub fn imap_quoted_astring(value: &str) -> String {
    let mut out = String::from('"');
    for ch in value.chars() {
        match ch {
            '\r' | '\n' | '\0' => {}
            '\\' | '"' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// `UID SEARCH` criteria for a `Message-ID` header value.
pub fn imap_header_message_id_search(hdr: &str) -> String {
    format!("HEADER Message-ID {}", imap_quoted_astring(hdr.trim()))
}

/// `UID SEARCH` criteria for `\Deleted` messages excluding a UID set.
pub fn imap_deleted_bystanders_search(uid_set: &str) -> String {
    format!("DELETED NOT UID {uid_set}")
}

/// `UID STORE` commands for IMAP junk classification keywords.
pub fn junk_keyword_store_commands(junk: bool) -> Vec<String> {
    if junk {
        vec![
            "+FLAGS.SILENT ($Junk Junk)".to_string(),
            "-FLAGS.SILENT ($NotJunk NonJunk)".to_string(),
        ]
    } else {
        vec![
            "+FLAGS.SILENT ($NotJunk NonJunk)".to_string(),
            "-FLAGS.SILENT ($Junk Junk)".to_string(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_poll_items_exact_strings() {
        assert_eq!(
            status_poll_items(true),
            "(MESSAGES UIDNEXT UIDVALIDITY UNSEEN HIGHESTMODSEQ)"
        );
        assert_eq!(
            status_poll_items(false),
            "(MESSAGES UIDNEXT UIDVALIDITY UNSEEN)"
        );
    }

    #[test]
    fn appended_mail_from_code_reads_appenduid() {
        use async_imap::imap_proto::{ResponseCode, UidSetMember};

        let code = ResponseCode::AppendUid(9, vec![UidSetMember::Uid(42)]);
        assert_eq!(
            appended_mail_from_code(Some(&code)),
            AppendedMail {
                uidvalidity: Some(9),
                uid: Some(42),
            }
        );
        let range = ResponseCode::AppendUid(3, vec![(40..=41).into()]);
        assert_eq!(appended_mail_from_code(Some(&range)).uid, Some(40));
        assert_eq!(
            appended_mail_from_code(Some(&ResponseCode::UidNext(7))),
            AppendedMail::default()
        );
        assert_eq!(appended_mail_from_code(None), AppendedMail::default());
    }

    #[test]
    fn imap_header_message_id_search_quotes_angle_addr() {
        assert_eq!(
            imap_header_message_id_search("<a@b.com>"),
            "HEADER Message-ID \"<a@b.com>\""
        );
    }

    #[test]
    fn imap_deleted_bystanders_search_excludes_uid_set() {
        assert_eq!(
            imap_deleted_bystanders_search("1:3,5"),
            "DELETED NOT UID 1:3,5"
        );
    }

    #[test]
    fn junk_keyword_store_commands_spam_and_not_spam() {
        assert_eq!(
            junk_keyword_store_commands(true),
            vec![
                "+FLAGS.SILENT ($Junk Junk)".to_string(),
                "-FLAGS.SILENT ($NotJunk NonJunk)".to_string(),
            ]
        );
        assert_eq!(
            junk_keyword_store_commands(false),
            vec![
                "+FLAGS.SILENT ($NotJunk NonJunk)".to_string(),
                "-FLAGS.SILENT ($Junk Junk)".to_string(),
            ]
        );
    }

    #[test]
    fn append_rejected_draft_flag_detects_server_no() {
        assert!(append_rejected_draft_flag(&Error::Imap(
            "APPEND Drafts: NO [NOPERM] The \\Draft flag is not supported".into()
        )));
        assert!(append_rejected_draft_flag(&Error::Imap(
            "APPEND Drafts: NO Invalid system flag".into()
        )));
        assert!(!append_rejected_draft_flag(&Error::Imap(
            "APPEND Drafts: NO Quota exceeded".into()
        )));
        assert!(!append_rejected_draft_flag(&Error::Imap(
            "APPEND Drafts: could not append mail to mailbox".into()
        )));
    }

    #[test]
    fn imap_quoted_astring_escapes_backslash_and_quote() {
        assert_eq!(imap_quoted_astring("a\"b"), "\"a\\\"b\"");
        assert_eq!(imap_quoted_astring("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn non_ascii_search_criteria_get_a_utf8_charset() {
        assert_eq!(search_criteria_with_charset("UNDELETED"), "UNDELETED");
        assert_eq!(
            search_criteria_with_charset("FROM \"José\""),
            "CHARSET UTF-8 FROM \"José\""
        );
    }

    #[test]
    fn special_use_snoozed_follows_the_server_delimiter() {
        assert!(matches!(
            map_special_use(&[], "INBOX.Snoozed", "."),
            SpecialUse::Snoozed
        ));
        assert!(matches!(
            map_special_use(&[], "INBOX.Snoozed", "/"),
            SpecialUse::Other
        ));
    }

    #[test]
    fn imap_quoted_astring_cannot_end_the_command_line() {
        let quoted = imap_quoted_astring("<a@b>\"\r\nA1 DELETE INBOX\r\n\0");
        assert!(!quoted.contains(['\r', '\n', '\0']));
        assert_eq!(quoted, "\"<a@b>\\\"A1 DELETE INBOX\"");
    }

    #[test]
    fn maps_special_use_attributes() {
        let attrs = [NameAttribute::Sent];
        assert!(matches!(
            map_special_use(&attrs, "Sent Items", "/"),
            SpecialUse::Sent
        ));
        assert!(matches!(
            map_special_use(&[], "INBOX", "/"),
            SpecialUse::Inbox
        ));
        assert!(matches!(
            map_special_use(&[], "Snoozed", "/"),
            SpecialUse::Snoozed
        ));
        assert!(matches!(
            map_special_use(&[], "[Gmail]/Snoozed", "/"),
            SpecialUse::Snoozed
        ));
    }

    fn listed(name: &str, special: SpecialUse) -> ListedFolder {
        ListedFolder {
            imap_name: name.to_string(),
            delimiter: "/".to_string(),
            special,
            selectable: true,
        }
    }

    #[test]
    fn trash_fallback_marks_common_names_when_no_flag_present() {
        for name in ["Trash", "Deleted Items", "deleted messages", "INBOX/Trash"] {
            let mut folders = vec![
                listed("INBOX", SpecialUse::Inbox),
                listed(name, SpecialUse::Other),
            ];
            apply_trash_name_fallback(&mut folders);
            assert_eq!(folders[1].special, SpecialUse::Trash, "{name}");
        }
    }

    #[test]
    fn trash_fallback_marks_gmail_trash_by_leaf_name() {
        let mut folders = vec![listed("[Gmail]/Trash", SpecialUse::Other)];
        apply_trash_name_fallback(&mut folders);
        assert_eq!(folders[0].special, SpecialUse::Trash);
    }

    #[test]
    fn trash_fallback_defers_to_server_flagged_trash() {
        let mut folders = vec![
            listed("Bin", SpecialUse::Trash),
            listed("Trash", SpecialUse::Other),
        ];
        apply_trash_name_fallback(&mut folders);
        assert_eq!(folders[1].special, SpecialUse::Other);
    }

    #[test]
    fn trash_fallback_prefers_top_level_and_skips_unselectable() {
        let mut folders = vec![
            listed("Archive/Trash", SpecialUse::Other),
            listed("Deleted Items", SpecialUse::Other),
        ];
        apply_trash_name_fallback(&mut folders);
        assert_eq!(folders[0].special, SpecialUse::Other);
        assert_eq!(folders[1].special, SpecialUse::Trash);

        let mut hidden = vec![listed("Trash", SpecialUse::Other)];
        hidden[0].selectable = false;
        apply_trash_name_fallback(&mut hidden);
        assert_eq!(hidden[0].special, SpecialUse::Other);
    }

    #[test]
    fn trash_fallback_leaves_accounts_without_trash_alone() {
        let mut folders = vec![
            listed("INBOX", SpecialUse::Inbox),
            listed("Notes", SpecialUse::Other),
        ];
        apply_trash_name_fallback(&mut folders);
        assert_eq!(folders[1].special, SpecialUse::Other);
    }

    #[test]
    fn imap_server_caps_unselect_defaults_false() {
        let caps = ImapServerCaps::default();
        assert!(!caps.unselect);
        assert!(!caps.gmail_ext);
    }

    #[test]
    fn condstore_query_appends_changedsince() {
        let caps = ImapServerCaps {
            condstore: true,
            ..ImapServerCaps::default()
        };
        let q = uid_fetch_query_with_changedsince("(FLAGS UID)", Some(42), &caps);
        assert_eq!(q, "(FLAGS UID) (CHANGEDSINCE 42)");
    }

    #[test]
    fn qresync_without_condstore_skips_changedsince() {
        let caps = ImapServerCaps {
            qresync: true,
            ..ImapServerCaps::default()
        };
        let q = uid_fetch_query_with_changedsince("(FLAGS UID)", Some(42), &caps);
        assert_eq!(q, "(FLAGS UID)");
    }

    #[test]
    fn imap_auth_debug_redacts_secrets() {
        let auth = ImapAuth::Password("secret-pass".to_string());
        let debug = format!("{auth:?}");
        assert!(debug.contains("***"));
        assert!(!debug.contains("secret-pass"));

        let opts = ImapConnectOptions {
            host: "imap.example.com".to_string(),
            port: 993,
            email: "user@example.com".to_string(),
            username: "user@example.com".to_string(),
            auth: ImapAuth::XOAuth2("ya29.token".to_string()),
        };
        let debug = format!("{opts:?}");
        assert!(debug.contains("***"));
        assert!(!debug.contains("ya29.token"));
    }

    #[test]
    fn move_no_unavailable_can_fall_back() {
        let refused = async_imap::error::Error::No(
            "code: None, info: Some(\"[UNAVAILABLE] Service temporarily unavailable\")".into(),
        );
        assert!(imap_move_can_fall_back(&refused));
        assert!(imap_move_can_fall_back(&async_imap::error::Error::Bad(
            "unknown command".into()
        )));
        assert!(!imap_move_can_fall_back(
            &async_imap::error::Error::ConnectionLost
        ));
    }

    #[test]
    fn imap_empty_set_error_is_case_insensitive() {
        assert!(imap_empty_set_error(
            "NO UID STORE failed: No matching messages"
        ));
        assert!(imap_empty_set_error("No messages in mailbox"));
        assert!(!imap_empty_set_error("NO [ALERT] Server busy"));
    }

    #[test]
    fn imap_date_text_formats_rfc3501_date() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 24).unwrap();
        assert_eq!(imap_date_text(date), "24-Aug-2026");
    }

    #[test]
    fn imap_search_before_inclusive_is_exclusive_of_next_day() {
        let through = NaiveDate::from_ymd_opt(2026, 8, 24).unwrap();
        assert_eq!(imap_search_before_inclusive(through), "BEFORE 25-Aug-2026");
    }

    #[test]
    fn imap_search_before_inclusive_handles_month_end() {
        let through = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
        assert_eq!(imap_search_before_inclusive(through), "BEFORE 1-Feb-2026");
    }

    #[test]
    fn format_uid_set_chunks_compresses_ranges() {
        assert_eq!(
            format_uid_set_chunks(&[1, 2, 3, 5, 10], 250),
            vec!["1:3,5,10".to_string()]
        );
    }

    #[test]
    fn format_uid_set_chunks_splits_by_uid_count() {
        assert_eq!(
            format_uid_set_chunks(&[1, 2, 3, 4], 2),
            vec!["1:2".to_string(), "3:4".to_string()]
        );
    }
}
