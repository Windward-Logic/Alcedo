CREATE TABLE contact_groups (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL COLLATE NOCASE,
  is_system INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);

CREATE UNIQUE INDEX contact_groups_name_nocase ON contact_groups(name COLLATE NOCASE);

CREATE TABLE contacts (
  id INTEGER PRIMARY KEY,
  email TEXT NOT NULL COLLATE NOCASE,
  display_name TEXT NOT NULL DEFAULT '',
  nickname TEXT NOT NULL DEFAULT '',
  organization TEXT NOT NULL DEFAULT '',
  notes TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL CHECK (status IN ('approved', 'suggested')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  last_used_at TEXT
);

CREATE UNIQUE INDEX contacts_email_nocase ON contacts(email COLLATE NOCASE);

CREATE TABLE contact_group_members (
  group_id INTEGER NOT NULL REFERENCES contact_groups(id) ON DELETE CASCADE,
  contact_id INTEGER NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
  PRIMARY KEY (group_id, contact_id)
);

INSERT INTO contact_groups (id, name, is_system, created_at)
VALUES (1, 'Unapproved', 1, '1970-01-01T00:00:00Z');
