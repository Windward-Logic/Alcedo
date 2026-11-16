ALTER TABLE accounts ADD COLUMN pgp_sign_outgoing INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN pgp_status_json TEXT;
CREATE TABLE pgp_keys (
  fingerprint TEXT PRIMARY KEY,
  emails_json TEXT NOT NULL,
  armored TEXT NOT NULL,
  source TEXT NOT NULL,
  account_id TEXT,
  created_utc TEXT NOT NULL,
  last_seen_utc TEXT NOT NULL
);
CREATE INDEX pgp_keys_account ON pgp_keys(account_id);
