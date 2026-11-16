CREATE TABLE queued_sends (
  id TEXT PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  draft_json TEXT NOT NULL,
  queued_at TEXT NOT NULL
);
