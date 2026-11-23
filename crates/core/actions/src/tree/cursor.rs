//! Where a listing goes on from: the cursor handed out with one page of a
//! level, which reads the next from the directory page it starts in.

use super::PageMark;

/// A cursor a listing issues: how many entries the listing is past, and,
/// for a resource's listing, the directory page the next one starts in and
/// how many resources that page holds before it, so the next page reads
/// from there instead of every page before it. Written
/// `<offset>` or `<offset>.<skip>.<revision hex>.<cursor hex>[.<after hex>]`.
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
        let cursor = hex::decode(parts.next()?).ok()?;
        let after = match parts.next() {
            Some(after) => Some(String::from_utf8(hex::decode(after).ok()?).ok()?),
            None => None,
        };
        if parts.next().is_some() || cursor.is_empty() {
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
                let mut written = format!(
                    "{}.{skip}.{}.{}",
                    self.offset,
                    hex::encode(mark.revision.as_bytes()),
                    hex::encode(&mark.cursor)
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
