//! Sort comparators and sort-key change detection for message rows.

use super::*;

pub(crate) fn cmp_from(a: &MessageRow, b: &MessageRow) -> Ordering {
    cmp_keep_thread(
        a,
        b,
        from_thread_sort_key(a).cmp(&from_thread_sort_key(b)),
        false,
    )
}

pub(super) fn from_thread_sort_key(row: &MessageRow) -> String {
    if row.message_count > 1 || row.depth == 1 {
        row.participants
            .first()
            .map(|addr| addr.display_name())
            .unwrap_or_else(|| "Unknown".to_string())
            .to_lowercase()
    } else {
        sender_sort_key(&row.envelope)
    }
}

pub(super) fn sender_sort_key(envelope: &Envelope) -> String {
    envelope
        .from
        .first()
        .map(|addr| addr.display_name())
        .unwrap_or_else(|| "Unknown".to_string())
        .to_lowercase()
}

pub(crate) fn cmp_subject(a: &MessageRow, b: &MessageRow) -> Ordering {
    cmp_keep_thread(
        a,
        b,
        a.sort_subject
            .to_lowercase()
            .cmp(&b.sort_subject.to_lowercase()),
        false,
    )
}

pub(super) fn subject_sort_key(envelope: &Envelope) -> String {
    envelope.display_subject().to_lowercase()
}

pub(super) fn cmp_keep_thread(
    a: &MessageRow,
    b: &MessageRow,
    primary: Ordering,
    invert_tree: bool,
) -> Ordering {
    primary.then_with(|| {
        if a.thread_id != b.thread_id {
            return Ordering::Equal;
        }
        if a.message_count <= 1 && a.depth == 0 && b.message_count <= 1 && b.depth == 0 {
            return Ordering::Equal;
        }
        let tree = a
            .depth
            .cmp(&b.depth)
            .then_with(|| a.envelope.date.cmp(&b.envelope.date))
            .then_with(|| a.envelope.id.0.cmp(&b.envelope.id.0));
        if invert_tree { tree.reverse() } else { tree }
    })
}

pub(crate) fn cmp_date(a: &MessageRow, b: &MessageRow) -> Ordering {
    cmp_keep_thread(a, b, a.sort_date.cmp(&b.sort_date), true)
}

pub(crate) fn cmp_size(a: &MessageRow, b: &MessageRow) -> Ordering {
    cmp_keep_thread(a, b, a.sort_size.cmp(&b.sort_size), true)
}

pub(super) fn envelope_sort_keys_changed(old: &Envelope, new: &Envelope) -> bool {
    old.date != new.date
        || old.deleted_at != new.deleted_at
        || old.folder_id != new.folder_id
        || old.size_bytes != new.size_bytes
        || sender_sort_key(old) != sender_sort_key(new)
        || subject_sort_key(old) != subject_sort_key(new)
}

pub(super) fn row_sort_keys_changed(old: &MessageRow, new: &MessageRow) -> bool {
    envelope_sort_keys_changed(&old.envelope, &new.envelope)
        || old.sort_date != new.sort_date
        || old.sort_size != new.sort_size
        || old.sort_subject != new.sort_subject
}
