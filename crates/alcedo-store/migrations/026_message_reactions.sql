ALTER TABLE messages ADD COLUMN reaction_emoji TEXT;
ALTER TABLE messages ADD COLUMN reaction_target_hdr TEXT;
CREATE INDEX idx_messages_reaction_target
  ON messages(account_id, reaction_target_hdr)
  WHERE reaction_emoji IS NOT NULL;

DROP VIEW IF EXISTS unified_inbox;
CREATE VIEW unified_inbox AS
SELECT m.*, a.accent_color AS account_accent, a.avatar_text AS account_avatar, a.email AS account_email
FROM messages m
JOIN folders f ON f.id = m.folder_id
JOIN accounts a ON a.id = m.account_id
WHERE f.special = 'inbox' AND m.deleted = 0
  AND NOT (
    m.reaction_emoji IS NOT NULL
    AND m.reaction_target_hdr IS NOT NULL
    AND EXISTS (
      SELECT 1 FROM messages parent
      WHERE parent.account_id = m.account_id
        AND parent.message_id_hdr = m.reaction_target_hdr
        AND parent.deleted = 0
        AND parent.id != m.id
    )
  );
