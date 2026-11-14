//! How far a directory listing was read, and where it goes on: the pages a
//! level was read from, the cursor `tilo_ls` hands out to read on from the
//! page its next resource starts in, and the reads that page a level in.

use std::collections::HashSet;

use super::{
    ActionHost, CatalogueLimits, CatalogueQuery, CatalogueRow, DEFAULT_WAIT_MS, LEVEL_PAGES, Level,
    McpServer, PAGE_ROWS, level_of, restart_pause,
};

/// The rows a [`Level`] was read from, where each of its pages after the
/// first started, and, when the read stopped at its bound before the last
/// page, where the directory goes on: a listing paging past what was read
/// reads on from there ([`McpServer::level_reaching`]), and the next page of
/// a listing starts at the page its first resource was read in
/// ([`LsCursor`]).
#[derive(Clone, Default)]
pub(super) struct LevelPages {
    pub(super) rows: Vec<CatalogueRow>,
    pub(super) marks: Vec<PageMark>,
    pub(super) rest: Option<PageMark>,
}

/// Where one page of a directory starts: the row it starts at among those
/// read, and the cursor and revision that ask for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PageMark {
    pub(super) row: usize,
    pub(super) cursor: Vec<u8>,
    pub(super) revision: String,
}

impl Level {
    /// Whether the directory goes on past what was read.
    pub(super) fn unread(&self) -> bool {
        self.pages.rest.is_some()
    }

    /// Where a read starting at the page holding `child`'s first row picks
    /// up: that page, and how many resources that read meets before
    /// `child`. `None` when that page is the directory's first.
    pub(super) fn resume_at(&self, parent: &str, child: usize) -> Option<(PageMark, usize)> {
        let rows = &self.pages.rows;
        let mut seen: HashSet<&str> = HashSet::new();
        let mut first = None;
        for (at, row) in rows.iter().enumerate() {
            let id = row.resource.id.as_str();
            if (!parent.is_empty() && id == parent) || !seen.insert(id) {
                continue;
            }
            if seen.len() == child.saturating_add(1) {
                first = Some(at);
                break;
            }
        }
        let first = first?;
        let mark = self
            .pages
            .marks
            .iter()
            .rev()
            .find(|mark| mark.row <= first)?
            .clone();
        let mut met: HashSet<&str> = HashSet::new();
        for row in rows.get(mark.row..first).unwrap_or_default() {
            let id = row.resource.id.as_str();
            if parent.is_empty() || id != parent {
                met.insert(id);
            }
        }
        Some((mark, met.len()))
    }
}

/// A cursor `tilo_ls` issues: how many entries the listing is past, and,
/// for a resource's listing, the directory page the next one starts in and
/// how many resources that page holds before it, so the next page reads
/// from there instead of every page before it. Written
/// `<offset>` or `<offset>.<skip>.<revision hex>.<cursor hex>`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct LsCursor {
    pub(super) offset: usize,
    pub(super) resume: Option<(PageMark, usize)>,
}

impl LsCursor {
    pub(super) fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let offset = parts.next()?.parse().ok()?;
        let Some(skip) = parts.next() else {
            return Some(Self {
                offset,
                resume: None,
            });
        };
        let skip = skip.parse().ok()?;
        let revision = String::from_utf8(hex::decode(parts.next()?).ok()?).ok()?;
        let cursor = hex::decode(parts.next()?).ok()?;
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
        })
    }

    pub(super) fn write(&self) -> String {
        match &self.resume {
            None => self.offset.to_string(),
            Some((mark, skip)) => format!(
                "{}.{skip}.{}.{}",
                self.offset,
                hex::encode(mark.revision.as_bytes()),
                hex::encode(&mark.cursor)
            ),
        }
    }
}

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// `level`, read on until it holds more than `children` resources or
    /// the directory ends: what a listing paging past the first bounded
    /// read needs, so every resource a directory holds is reachable.
    pub(super) async fn level_reaching(
        &self,
        provider: &str,
        parent: &str,
        mut level: Level,
        children: usize,
    ) -> Result<Level, String> {
        while level.children.len() <= children && level.unread() {
            level = self
                .read_level(provider, parent, Some(level))
                .await?
                .ok_or("the app's directory kept changing while it was read; try again")?;
        }
        Ok(level)
    }

    /// The directory under `parent` from the page `mark` names on, until it
    /// holds more than `children` resources or ends; `None` when the
    /// directory moved on since `mark` was read.
    pub(super) async fn level_from(
        &self,
        provider: &str,
        parent: &str,
        mark: PageMark,
        children: usize,
    ) -> Result<Option<Level>, String> {
        let mut level = Level {
            pages: std::sync::Arc::new(LevelPages {
                rest: Some(mark),
                ..LevelPages::default()
            }),
            ..Level::default()
        };
        while level.children.len() <= children && level.unread() {
            let Some(read) = self.read_level_once(provider, parent, level).await? else {
                return Ok(None);
            };
            level = read;
        }
        Ok(Some(level))
    }

    /// Up to [`LEVEL_PAGES`] pages of the directory under `parent`, from
    /// the start or on from where `from` stopped, starting again from the
    /// first page while the directory moves on between pages.
    pub(super) async fn read_level(
        &self,
        provider: &str,
        parent: &str,
        from: Option<Level>,
    ) -> Result<Option<Level>, String> {
        // A level is one snapshot: every page of it at one revision. When the
        // app's directory moves on between pages (its document changed, or an
        // invocation refreshed it), what was read belongs to the old revision
        // and the read starts again from the first page at the new one, for
        // as long as an action is waited for, pausing longer each time a
        // directory that keeps changing is read again.
        let deadline = tokio::time::Instant::now()
            .checked_add(std::time::Duration::from_millis(DEFAULT_WAIT_MS));
        let mut from = from.unwrap_or_default();
        let mut reads: u32 = 0;
        loop {
            let pause = restart_pause(reads);
            if !pause.is_zero()
                && let Some(deadline) = deadline
            {
                let until = tokio::time::Instant::now()
                    .checked_add(pause)
                    .map_or(deadline, |until| until.min(deadline));
                tokio::time::sleep_until(until).await;
            }
            reads = reads.saturating_add(1);
            if let Some(read) = self.read_level_once(provider, parent, from).await? {
                return Ok(Some(read));
            }
            if deadline.is_none_or(|deadline| tokio::time::Instant::now() >= deadline) {
                return Ok(None);
            }
            from = Level::default();
        }
    }

    /// [`Self::read_level`] without starting again: `None` when the
    /// directory moved on between pages.
    pub(super) async fn read_level_once(
        &self,
        provider: &str,
        parent: &str,
        from: Level,
    ) -> Result<Option<Level>, String> {
        let LevelPages {
            mut rows,
            mut marks,
            rest,
        } = std::sync::Arc::unwrap_or_clone(from.pages);
        let (mut cursor, mut revision) = rest.map_or((Vec::new(), None), |mark| {
            (mark.cursor, Some(mark.revision))
        });
        let mut read_at = from.revision;
        let mut rest = None;
        for page in 0..LEVEL_PAGES {
            if !cursor.is_empty() {
                marks.push(PageMark {
                    row: rows.len(),
                    cursor: cursor.clone(),
                    revision: revision.clone().unwrap_or_default(),
                });
            }
            let asked = self
                .ask(CatalogueQuery {
                    provider: Some(provider.to_owned()),
                    parent_resource: (!parent.is_empty()).then(|| parent.to_owned()),
                    cursor: std::mem::take(&mut cursor),
                    revision: revision.take(),
                    max_results: PAGE_ROWS,
                    limits: Some(CatalogueLimits {
                        max_bytes: Some(65536),
                        max_work: Some(512),
                        max_depth: Some(16),
                    }),
                    ..CatalogueQuery::default()
                })
                .await?;
            if asked.stale_cursor {
                return Ok(None);
            }
            rows.extend(asked.rows);
            read_at.clone_from(&asked.revision);
            if asked.exhausted || asked.next_cursor.is_empty() {
                break;
            }
            if page.saturating_add(1) == LEVEL_PAGES {
                rest = Some(PageMark {
                    row: rows.len(),
                    cursor: asked.next_cursor,
                    revision: asked.revision,
                });
                break;
            }
            revision = Some(asked.revision);
            cursor = asked.next_cursor;
        }
        let mut read = level_of(&rows, parent);
        read.revision = read_at;
        read.pages = std::sync::Arc::new(LevelPages { rows, marks, rest });
        Ok(Some(read))
    }
}
