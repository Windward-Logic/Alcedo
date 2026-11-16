UPDATE messages
SET has_attachments = 1
WHERE has_attachments = 0
  AND EXISTS (
    SELECT 1 FROM attachments att
    WHERE att.message_id = messages.id
      AND (
        att.cid IS NULL OR trim(att.cid) = ''
        OR lower(trim(att.mime)) NOT LIKE 'image/%'
        OR lower(trim(att.mime)) IN (
          'text/html',
          'application/javascript',
          'text/javascript',
          'image/svg+xml',
          'application/wasm',
          'application/x-msdownload',
          'application/vnd.microsoft.portable-executable',
          'application/vnd.ms-msi',
          'application/x-msi',
          'application/x-sharedlib',
          'application/x-executable',
          'application/x-dosexec'
        )
      )
  );

CREATE TABLE attachment_repair (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  sent_structure_scan INTEGER NOT NULL DEFAULT 0
);
INSERT INTO attachment_repair (id, sent_structure_scan) VALUES (1, 0);
