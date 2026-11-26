//! Where a listing goes on from: the cursor handed out with one page of a
//! level, which reads the next from the directory page it starts in.

use super::PageMark;

/// How a resume mark with no source cursor is written: the page a level started in when it
/// was read from the directory's start, which is asked for with no cursor.
const NO_CURSOR: &str = "~";

/// A cursor a listing issues: how many entries the listing is past, and,
/// for a resource's listing, the directory page the next one starts in and
/// how many resources that page holds before it, so the next page reads
/// from there instead of every page before it. Written
/// `<offset>` or `<offset>.<skip>.<revision hex>.<cursor hex>[.<after hex>]`,
/// the cursor hex being `~` when the page is the directory's start.
#[derive(Debug, PartialEq, Eq)]
pub struct LsCursor {
    /// Entries the listing is past.
    pub offset: usize,
    /// The directory page the next entry starts in, and how many resources
    /// that page holds before it.
    pub resume: Option<(PageMark, usize)>,
    /// The resource the page before ended at, where the next page resumes in a directory:
    /// read on from the same row once the directory moved on, a page that finds it again
    /// starts after it, so what writes put before it never lists a resource twice.
    pub after: Option<String>,
}

impl LsCursor {
    /// The cursor `text` writes ([`Self::write`]); `None` for one it does not.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let offset = parts.next()?.parse().ok()?;
        let Some(skip) = parts.next() else {
            return Some(Self {
                offset,
                resume: None,
                after: None,
            });
        };
        let skip = skip.parse().ok()?;
        let revision = String::from_utf8(hex::decode(parts.next()?).ok()?).ok()?;
        let cursor = match parts.next()? {
            NO_CURSOR => Vec::new(),
            written => {
                let cursor = hex::decode(written).ok()?;
                if cursor.is_empty() {
                    return None;
                }
                cursor
            }
        };
        let after = match parts.next() {
            Some(after) => Some(String::from_utf8(hex::decode(after).ok()?).ok()?),
            None => None,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            offset,
            resume: Some((
                PageMark {
                    row: 0,
                    cursor,
                    revision,
                },
                skip,
            )),
            after,
        })
    }

    /// The cursor as it is handed out.
    #[must_use]
    pub fn write(&self) -> String {
        match &self.resume {
            None => self.offset.to_string(),
            Some((mark, skip)) => {
                let cursor = if mark.cursor.is_empty() {
                    NO_CURSOR.to_owned()
                } else {
                    hex::encode(&mark.cursor)
                };
                let mut written = format!(
                    "{}.{skip}.{}.{cursor}",
                    self.offset,
                    hex::encode(mark.revision.as_bytes()),
                );
                if let Some(after) = &self.after {
                    written.push('.');
                    written.push_str(&hex::encode(after.as_bytes()));
                }
                written
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LsCursor, PageMark};

    /// A page of the directory's start carries no source cursor, and is written and read back
    /// as that page: its revision names it, and the skip how far into it the next entry is.
    #[test]
    fn a_cursor_naming_the_directorys_start_round_trips() {
        let cursor = LsCursor {
            offset: 7,
            resume: Some((
                PageMark {
                    row: 0,
                    cursor: Vec::new(),
                    revision: "rev-1".to_owned(),
                },
                7,
            )),
            after: Some("message 6".to_owned()),
        };
        assert_eq!(LsCursor::parse(&cursor.write()), Some(cursor));
    }

    /// The one resume mark with no cursor is the one this writes `~`: an empty cursor field
    /// names no page, and is refused with the other cursors this host did not write.
    #[test]
    fn an_empty_cursor_field_is_not_a_continuation() {
        assert_eq!(LsCursor::parse("7.7.7265762d31."), None);
    }
}
