# Translation glossary

Rules for `po/*.po` (domain `alcedo`). Keep a term the same everywhere it appears. Alcedo Pro's catalogue
(`alcedo-pro`) follows the same glossary, so shared words read the same in both.

The four translations were drafted from the English text and need review by native speakers before a release.

## Register

Formal address in every language: French *vous*, Spanish *usted*, German *Sie*, Italian *Lei*. Buttons and menu
items use the infinitive (fr, es, it) or the form common in each desktop (de).

## Keep as written

Alcedo, OpenPGP, Autocrypt, IMAP, SMTP, SSL/TLS, STARTTLS, OAuth, Google, Microsoft, Outlook, Gmail, Yahoo,
HTML, Markdown, MIME, URL, search operators (`from:`, `to:`, `subject:`, `folder:`, `in:`, `is:`, `has:`).

## Terms

| English | fr | es | de | it |
|---|---|---|---|---|
| Inbox | Boîte de réception | Bandeja de entrada | Posteingang | Posta in arrivo |
| Sent | Envoyés | Enviados | Gesendet | Inviati |
| Drafts / draft | Brouillons / brouillon | Borradores / borrador | Entwürfe / Entwurf | Bozze / bozza |
| Trash | Corbeille | Papelera | Papierkorb | Cestino |
| Junk / spam | Indésirables / indésirable | Correo no deseado / no deseado | Spam | Posta indesiderata / indesiderato |
| Archive | Archives | Archivo | Archiv | Archivio |
| Snoozed | Reportés | Pospuestos | Zurückgestellt | Posticipati |
| Outbox | Boîte d’envoi | Bandeja de salida | Postausgang | Posta in uscita |
| Everything (unified inbox) | Tout | Todo | Alles | Tutto |
| star / starred | suivre / suivi | destacar / destacado | markieren / markiert | speciali |
| keyring | trousseau de clés | llavero | Schlüsselbund | portachiavi |
| account chip (initials badge) | pastille | insignia | Kürzel | sigla |
| snippet | extrait | fragmento | Textbaustein | snippet |
| Unapproved (contacts group) | Non approuvés | No aprobados | Nicht zugelassen | Non approvati |
| activity log | journal d’activité | registro de actividad | Aktivitätsprotokoll | registro attività |
| From address | adresse d’expédition | dirección de remitente | Absenderadresse | indirizzo mittente |

## Mechanics

- Plural forms follow gettext: French uses one form for 0 and 1; es, de and it use one form for 1 only. A plural
  form may add `{n}` but must keep every other placeholder; `scripts/i18n.py check` enforces this.
- Date and clock patterns (`strftime pattern: …`) are translated like text; day and month names come from the
  desktop locale, not from the catalogue. Only conversions GLib can format are accepted.
- A menu item with a `_` has one underscore marking its keyboard mnemonic; keep exactly one, on a letter that is
  not used by another item in the same menu.
- Placeholders such as `{name}` are copied unchanged. Text after `{reason}` comes from another program and stays in
  its own language.
