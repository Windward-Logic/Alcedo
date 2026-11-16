ALTER TABLE messages ADD COLUMN rfc822_size INTEGER;
CREATE INDEX idx_messages_rfc822_size ON messages(rfc822_size) WHERE deleted = 0;
