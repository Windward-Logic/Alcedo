-- Denormalized list columns (maintained in Rust on every writer).
ALTER TABLE messages ADD COLUMN list_has_attachments INTEGER NOT NULL DEFAULT 0;
ALTER TABLE messages ADD COLUMN list_size_bytes INTEGER NOT NULL DEFAULT 0;
