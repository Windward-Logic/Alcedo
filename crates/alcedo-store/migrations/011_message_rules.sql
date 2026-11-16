CREATE TABLE message_rules (
  id TEXT PRIMARY KEY,
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  position INTEGER NOT NULL,
  match_mode TEXT NOT NULL,
  stop_processing INTEGER NOT NULL DEFAULT 1,
  conditions_json TEXT NOT NULL,
  actions_json TEXT NOT NULL,
  execution TEXT NOT NULL DEFAULT 'client',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX idx_message_rules_account_pos ON message_rules(account_id, position);

CREATE TABLE message_rule_runs (
  message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
  PRIMARY KEY (message_id)
);
