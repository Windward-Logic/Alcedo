CREATE TABLE account_signature_inlines (
  account_id TEXT NOT NULL REFERENCES account_signatures(account_id) ON DELETE CASCADE,
  content_id TEXT NOT NULL,
  filename TEXT NOT NULL,
  mime TEXT NOT NULL,
  bytes BLOB NOT NULL,
  position INTEGER NOT NULL,
  PRIMARY KEY (account_id, content_id)
);

INSERT INTO account_signature_inlines (
  account_id, content_id, filename, mime, bytes, position
)
SELECT
  account_id,
  'signature-logo-' || account_id || '@alcedo.mail',
  'signature-logo.png',
  'image/png',
  logo_png,
  0
FROM account_signatures
WHERE length(logo_png) > 0
  AND instr(
    lower(body),
    'cid:signature-logo-' || lower(account_id) || '@alcedo.mail'
  ) > 0;

ALTER TABLE account_signatures DROP COLUMN logo_png;
