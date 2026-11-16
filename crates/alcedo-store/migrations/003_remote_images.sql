ALTER TABLE messages ADD COLUMN allow_remote_images INTEGER NOT NULL DEFAULT 0;
CREATE TABLE remote_image_senders (
  email TEXT PRIMARY KEY COLLATE NOCASE
);
