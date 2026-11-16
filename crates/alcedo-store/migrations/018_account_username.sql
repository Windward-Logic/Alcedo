ALTER TABLE accounts ADD COLUMN username TEXT NOT NULL DEFAULT '';
UPDATE accounts SET username = email WHERE username = '';
