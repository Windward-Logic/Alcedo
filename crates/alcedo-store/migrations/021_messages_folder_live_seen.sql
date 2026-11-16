CREATE INDEX IF NOT EXISTS idx_messages_folder_live_seen ON messages(folder_id, deleted, seen);
