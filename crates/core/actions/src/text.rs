//! The smallest change between two texts.

/// How long a run `a` and `b` share at their start, and how long a run they
/// share at their end after it (the two never overlap).
#[must_use]
pub fn common_ends<T: PartialEq>(a: &[T], b: &[T]) -> (usize, usize) {
    let prefix = a
        .iter()
        .zip(b)
        .take_while(|(left, right)| left == right)
        .count();
    let room = a.len().min(b.len()).saturating_sub(prefix);
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(room)
        .take_while(|(left, right)| left == right)
        .count();
    (prefix, suffix)
}

/// [`common_ends`] of two texts, in bytes, on character boundaries: the
/// texts are compared byte by byte, never decoded into characters.
#[must_use]
pub fn common_text_ends(a: &str, b: &str) -> (usize, usize) {
    let mut prefix = a
        .bytes()
        .zip(b.bytes())
        .take_while(|(left, right)| left == right)
        .count();
    while !(a.is_char_boundary(prefix) && b.is_char_boundary(prefix)) {
        prefix = prefix.saturating_sub(1);
    }
    let ((_, a), (_, b)) = (a.split_at(prefix), b.split_at(prefix));
    let mut suffix = a
        .bytes()
        .rev()
        .zip(b.bytes().rev())
        .take_while(|(left, right)| left == right)
        .count();
    while !(a.is_char_boundary(a.len().saturating_sub(suffix))
        && b.is_char_boundary(b.len().saturating_sub(suffix)))
    {
        suffix = suffix.saturating_sub(1);
    }
    (prefix, suffix)
}

/// A splice in characters: at `at`, `delete` characters give way to `insert`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Splice {
    pub at: usize,
    pub delete: usize,
    pub insert: String,
}

/// The smallest splice, in characters, that turns `before` into `after`:
/// what the two share at their start and end is kept.
#[must_use]
pub fn splice_between(before: &str, after: &str) -> Option<Splice> {
    if before == after {
        return None;
    }
    let (start, end) = common_text_ends(before, after);
    let (kept, deleted) = before.split_at(start);
    let (deleted, _) = deleted.split_at(deleted.len().saturating_sub(end));
    let (_, inserted) = after.split_at(start);
    let (inserted, _) = inserted.split_at(inserted.len().saturating_sub(end));
    Some(Splice {
        at: kept.chars().count(),
        delete: deleted.chars().count(),
        insert: inserted.to_owned(),
    })
}
