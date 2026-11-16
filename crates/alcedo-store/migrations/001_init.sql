CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);

CREATE TABLE accounts (
  id TEXT PRIMARY KEY,
  email TEXT NOT NULL,
  display_name TEXT NOT NULL,
  imap_host TEXT NOT NULL,
  imap_port INTEGER NOT NULL,
  smtp_host TEXT NOT NULL,
  smtp_port INTEGER NOT NULL,
  smtp_starttls INTEGER NOT NULL DEFAULT 0,
  auth_method TEXT NOT NULL,
  accent_color TEXT NOT NULL,
  avatar_text TEXT NOT NULL
);

CREATE TABLE folders (
  id INTEGER PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  imap_name TEXT NOT NULL,
  delimiter TEXT NOT NULL DEFAULT '/',
  special TEXT NOT NULL,
  selectable INTEGER NOT NULL DEFAULT 1,
  uidvalidity INTEGER,
  uidnext INTEGER,
  highest_modseq INTEGER,
  unread INTEGER NOT NULL DEFAULT 0,
  total INTEGER NOT NULL DEFAULT 0,
  UNIQUE(account_id, imap_name)
);

CREATE TABLE messages (
  id INTEGER PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
  uid INTEGER NOT NULL,
  thread_id TEXT NOT NULL,
  date_utc TEXT NOT NULL,
  subject TEXT NOT NULL,
  from_json TEXT NOT NULL,
  to_json TEXT NOT NULL,
  cc_json TEXT NOT NULL,
  snippet TEXT NOT NULL,
  body_text TEXT,
  body_html TEXT,
  message_id_hdr TEXT,
  in_reply_to TEXT,
  references_hdr TEXT,
  seen INTEGER NOT NULL DEFAULT 0,
  flagged INTEGER NOT NULL DEFAULT 0,
  answered INTEGER NOT NULL DEFAULT 0,
  draft INTEGER NOT NULL DEFAULT 0,
  deleted INTEGER NOT NULL DEFAULT 0,
  has_attachments INTEGER NOT NULL DEFAULT 0,
  UNIQUE(folder_id, uid)
);
CREATE INDEX idx_messages_date ON messages(date_utc DESC);
CREATE INDEX idx_messages_account_date ON messages(account_id, date_utc DESC);

CREATE VIRTUAL TABLE messages_fts USING fts5(
  subject,
  from_addr,
  to_addrs,
  body_text,
  content='messages',
  content_rowid='id',
  tokenize='unicode61'
);

CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
  INSERT INTO messages_fts(rowid, subject, from_addr, to_addrs, body_text)
  VALUES (
    new.id,
    new.subject,
    (SELECT group_concat(json_extract(value, '$.email'), ' ')
     FROM json_each(new.from_json)),
    (SELECT group_concat(json_extract(value, '$.email'), ' ')
     FROM json_each(new.to_json)),
    coalesce(new.body_text, '')
  );
END;

CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, subject, from_addr, to_addrs, body_text)
  VALUES (
    'delete',
    old.id,
    old.subject,
    (SELECT group_concat(json_extract(value, '$.email'), ' ')
     FROM json_each(old.from_json)),
    (SELECT group_concat(json_extract(value, '$.email'), ' ')
     FROM json_each(old.to_json)),
    coalesce(old.body_text, '')
  );
END;

CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, subject, from_addr, to_addrs, body_text)
  VALUES (
    'delete',
    old.id,
    old.subject,
    (SELECT group_concat(json_extract(value, '$.email'), ' ')
     FROM json_each(old.from_json)),
    (SELECT group_concat(json_extract(value, '$.email'), ' ')
     FROM json_each(old.to_json)),
    coalesce(old.body_text, '')
  );
  INSERT INTO messages_fts(rowid, subject, from_addr, to_addrs, body_text)
  VALUES (
    new.id,
    new.subject,
    (SELECT group_concat(json_extract(value, '$.email'), ' ')
     FROM json_each(new.from_json)),
    (SELECT group_concat(json_extract(value, '$.email'), ' ')
     FROM json_each(new.to_json)),
    coalesce(new.body_text, '')
  );
END;

CREATE TABLE attachments (
  id INTEGER PRIMARY KEY,
  message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
  filename TEXT NOT NULL,
  mime TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  size INTEGER NOT NULL,
  cid TEXT,
  UNIQUE(message_id, sha256)
);

CREATE VIEW unified_inbox AS
SELECT m.*, a.accent_color AS account_accent, a.avatar_text AS account_avatar, a.email AS account_email
FROM messages m
JOIN folders f ON f.id = m.folder_id
JOIN accounts a ON a.id = m.account_id
WHERE f.special = 'inbox' AND m.deleted = 0;
