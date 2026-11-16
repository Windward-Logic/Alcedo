CREATE TABLE attachment_repair_v2 (
  account_id TEXT PRIMARY KEY,
  sent_structure_scan INTEGER NOT NULL DEFAULT 0
);
DROP TABLE attachment_repair;
ALTER TABLE attachment_repair_v2 RENAME TO attachment_repair;
