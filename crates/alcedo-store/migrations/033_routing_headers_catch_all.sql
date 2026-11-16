-- Reply routing headers (Mail-Reply-To, Mail-Followup-To, List-Post, List-Id,
-- Delivered-To, X-Original-To) as one JSON object. NULL = not fetched yet.
ALTER TABLE messages ADD COLUMN routing_json TEXT;

-- Domains whose every address belongs to the account (comma separated).
ALTER TABLE accounts ADD COLUMN catch_all_domains TEXT NOT NULL DEFAULT '';

DROP VIEW IF EXISTS unified_inbox;
CREATE VIEW unified_inbox AS
SELECT m.*, a.accent_color AS account_accent, a.avatar_text AS account_avatar, a.email AS account_email
FROM messages m
JOIN folders f ON f.id = m.folder_id
JOIN accounts a ON a.id = m.account_id
WHERE f.special = 'inbox' AND m.deleted = 0;
