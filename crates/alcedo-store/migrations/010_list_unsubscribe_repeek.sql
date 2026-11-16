-- Empty JSON was a "already peeked, no action" sentinel. A parser bug stored
-- that for mail that did have List-Unsubscribe (mail-parser Address vs Text).
-- NULL again so the next open re-peeks with the raw-header parser.
UPDATE messages
SET list_unsubscribe_json = NULL
WHERE list_unsubscribe_json IS NOT NULL
  AND json_extract(list_unsubscribe_json, '$.https') IS NULL
  AND json_extract(list_unsubscribe_json, '$.mailto') IS NULL
  AND COALESCE(json_extract(list_unsubscribe_json, '$.one_click'), 0) = 0;
