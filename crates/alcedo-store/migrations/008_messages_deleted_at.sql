ALTER TABLE messages ADD COLUMN deleted_at TEXT;
CREATE INDEX idx_messages_folder_deleted_at ON messages(folder_id, deleted_at DESC);
