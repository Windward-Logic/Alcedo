ALTER TABLE accounts ADD COLUMN smtp_envelope_policy TEXT NOT NULL DEFAULT 'mailbox';
UPDATE accounts SET smtp_envelope_policy = 'same_domain_identity' WHERE auth_method = 'password';
