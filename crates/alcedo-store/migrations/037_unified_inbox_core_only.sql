-- The unified inbox lists live inbox mail only. Earlier versions of this view
-- also excluded rows through a table the core app no longer owns.
DROP VIEW IF EXISTS unified_inbox;
CREATE VIEW unified_inbox AS
SELECT m.*, a.accent_color AS account_accent, a.avatar_text AS account_avatar, a.email AS account_email
FROM messages m
JOIN folders f ON f.id = m.folder_id
JOIN accounts a ON a.id = m.account_id
WHERE f.special = 'inbox' AND m.deleted = 0;
