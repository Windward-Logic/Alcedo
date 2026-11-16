CREATE TABLE account_signatures (
  account_id TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
  format TEXT NOT NULL,
  body TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE account_identities (
  id TEXT PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  display_name TEXT NOT NULL,
  email TEXT NOT NULL COLLATE NOCASE,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_identities_account_email ON account_identities(account_id, email);
CREATE INDEX idx_identities_account ON account_identities(account_id);

CREATE TABLE snippets (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL COLLATE NOCASE,
  format TEXT NOT NULL,
  body TEXT NOT NULL,
  position INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_snippets_name ON snippets(name COLLATE NOCASE);
