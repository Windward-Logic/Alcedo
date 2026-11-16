CREATE TABLE contacts_new (
  id INTEGER PRIMARY KEY,
  email TEXT NOT NULL COLLATE NOCASE,
  display_name TEXT NOT NULL DEFAULT '',
  nickname TEXT NOT NULL DEFAULT '',
  organization TEXT NOT NULL DEFAULT '',
  notes TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL CHECK (status IN ('approved', 'suggested', 'blocked')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  last_used_at TEXT
);

INSERT INTO contacts_new
SELECT id, email, display_name, nickname, organization, notes, status,
       created_at, updated_at, last_used_at
FROM contacts;

DROP TABLE contacts;
ALTER TABLE contacts_new RENAME TO contacts;

CREATE UNIQUE INDEX contacts_email_nocase ON contacts(email COLLATE NOCASE);
