//! How far a directory listing was read, and where it goes on: the pages a
//! level was read from, the cursor `tilo_ls` hands out to read on from the
//! page its next resource starts in, and the reads that page a level in.

use std::collections::HashSet;

use super::{
    ActionDescriptor, ActionProvider, CatalogueLimits, CatalogueQuery, CatalogueRow,
    DEFAULT_WAIT_MS, LEVEL_PAGES, Level, Levels, McpServer, Node, PAGE_ROWS, ROOT_PEEK,
    cannot_be_read_now, level_of, restart_pause,
};

/// How often [`McpServer::level_from`] reads on from where it stood at the revision a
/// directory moved on to, while writes beside it keep moving it on.
const RESUMES: u32 = 16;

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

    /// The resources of the level every row of which was read. A read that stopped at its
    /// bound may have stopped inside its last resource, whose other rows (the actions it
    /// offers, the one that reads it among them) are on the page after: that one is no
    /// resource to name or act on until the level is read on.
    pub(super) fn whole_children(&self) -> &[Node] {
        match self.children.split_last() {
            Some((_, whole)) if self.unread() => whole,
            _ => &self.children,
        }
    }

    /// Whether the provider's cursors the level was read by are row offsets ([`row_offset`]),
    /// as far as a cursor it handed out says.
    fn pages_by_offset(&self) -> bool {
        self.pages
            .rest
            .iter()
            .chain(&self.pages.marks)
            .next()
            .is_some_and(|mark| mark.cursor.len() == size_of::<u64>())
    }

    /// Whether the level was read on past the pages its first read is bound to
    /// ([`LEVEL_PAGES`]): each page after the first leaves its mark.
    pub(super) fn read_on(&self) -> bool {
        self.pages.marks.len() >= LEVEL_PAGES
    }

    /// The provider's cursor for the row the resource at `child` among the
    /// level's rows starts at, and the revision it was read at: where a
    /// listing's next page asks for its first resource, so that page reads
    /// from there and not from the level's first one. A resource past the
    /// rows read starts at the page the level goes on from; `None` only when
    /// the rows read cover the directory whole, so there is nowhere to go on
    /// from.
    pub(super) fn row_mark(&self, parent: &str, child: usize) -> Option<(Vec<u8>, String)> {
        let rows = &self.pages.rows;
        let mut seen: HashSet<&str> = HashSet::new();
        let mut at = None;
        for (index, row) in rows.iter().enumerate() {
            let id = row.resource.id.as_str();
            if (!parent.is_empty() && id == parent) || !seen.insert(id) {
                continue;
            }
            if seen.len() == child.saturating_add(1) {
                at = Some(index);
                break;
            }
        }
        let Some(at) = at else {
            // The page asks for a resource past those read: it stands on the
            // page the directory goes on from, if it does, and else after
            // every row read, where the directory holds nothing more.
            if let Some(rest) = self.pages.rest.as_ref() {
                return Some((rest.cursor.clone(), rest.revision.clone()));
            }
            let mark = self.pages.marks.last()?;
            let offset = row_offset(&mark.cursor)
                .saturating_add(spanned(rows.get(mark.row..).unwrap_or_default()));
            return Some((
                u64::try_from(offset)
                    .unwrap_or(u64::MAX)
                    .to_le_bytes()
                    .to_vec(),
                mark.revision.clone(),
            ));
        };
        // The marks a level leaves are pages of it, each with the provider's
        // row cursor it started at: the rows since move that on by as many as each stands
        // for. A row of the first page, which leaves no mark, is that many rows from the
        // directory's start, at the revision the level was read at, where the provider's
        // cursors are row offsets.
        let (offset, revision) = match self.pages.marks.iter().rev().find(|mark| mark.row <= at) {
            Some(mark) => (
                row_offset(&mark.cursor)
                    .saturating_add(spanned(rows.get(mark.row..at).unwrap_or_default())),
                &mark.revision,
            ),
            None if !self.revision.is_empty() && self.pages_by_offset() => {
                (spanned(rows.get(..at).unwrap_or_default()), &self.revision)
            }
            None => return None,
        };
        Some((
            u64::try_from(offset)
                .unwrap_or(u64::MAX)
                .to_le_bytes()
                .to_vec(),
            revision.clone(),
        ))
    }
}

/// How many rows to ask a provider for so a page reaches `reach` resources of
/// the level under `parent`, from what `rows` already read: each resource
/// offers its actions as rows, a few dozen on an item, which a listing taken a
/// resource at a time sends as one ([`CatalogueQuery::by_resource`]). Asking
/// a row per resource read three items a call, and a tracker of thousands a
/// call per three.
fn page_rows(rows: &[CatalogueRow], parent: &str, reach: usize) -> u32 {
    /// The rows a resource is taken to offer before any was read.
    const ROWS_EACH: usize = 48;
    // A read of the whole level keeps to the pages it is bounded to.
    if reach == usize::MAX {
        return PAGE_ROWS;
    }
    let read = distinct_children(rows, parent);
    let each = if read == 0 {
        ROWS_EACH
    } else {
        spanned(rows).div_ceil(read).max(1)
    };
    let wanted = reach
        .saturating_sub(read)
        .saturating_add(1)
        .saturating_mul(each);
    u32::try_from(wanted)
        .unwrap_or(u32::MAX)
        .clamp(PAGE_ROWS, tilo_actions::remote::MAX_BY_RESOURCE_PAGE)
}

/// The row a provider's cursor names: how many rows of the directory it
/// stands at.
fn row_offset(cursor: &[u8]) -> usize {
    <[u8; 8]>::try_from(cursor)
        .map(u64::from_le_bytes)
        .ok()
        .and_then(|offset| usize::try_from(offset).ok())
        .unwrap_or(0)
}

/// Why a read of a level's pages gave no level.
enum Unread {
    /// The app's directory moved on between pages, to the revision named.
    MovedOn(String),
    /// The app cannot read its directory right now ([`cannot_be_read_now`]).
    CannotNow,
    /// The app the query named could not be listed, and why: what it keeps did
    /// not mount yet, or the caller may not read it now.
    Unavailable(String),
}

/// A cursor `tilo_ls` issues: how many entries the listing is past, and,
/// for a resource's listing, the directory page the next one starts in and
/// how many resources that page holds before it, so the next page reads
/// from there instead of every page before it. Written
/// `<offset>` or `<offset>.<skip>.<revision hex>.<cursor hex>[.<after hex>]`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct LsCursor {
    pub(super) offset: usize,
    pub(super) resume: Option<(PageMark, usize)>,
    /// The resource the page before ended at, where the next page resumes in a directory:
    /// read on from the same row once the directory moved on, a page that finds it again
    /// starts after it, so what writes put before it never lists a resource twice.
    pub(super) after: Option<String>,
}

impl LsCursor {
    pub(super) fn parse(text: &str) -> Option<Self> {
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

    pub(super) fn write(&self) -> String {
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

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
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
            let had = level.children.len();
            level = self
                .read_level(provider, parent, Some(level), children.saturating_add(1))
                .await?
                .ok_or("the app's directory kept changing while it was read; try again")?;
            // A directory that moved on between every two pages gave its
            // first pages again: what they hold is what can be listed now.
            if level.children.len() <= had {
                break;
            }
        }
        Ok(level)
    }

    /// The directory level under `parent`, and the resource `find` picks out of it: the
    /// level is read on past its bound until `find` picks one or the directory ends. A read
    /// that names one resource so finds it wherever it falls in the directory, where a level
    /// read only to its bound would say a resource past it is not there. `find` picks among
    /// [`Level::whole_children`], so the resource it finds has all its actions. The level,
    /// as far as it was read, is kept in `levels`.
    pub(super) async fn level_holding(
        &self,
        provider: &str,
        parent: &str,
        levels: &mut Levels,
        find: impl Fn(&Level) -> Result<Option<Node>, String> + Send,
    ) -> Result<(Option<Node>, Level), String> {
        let mut level = self.level(provider, parent, levels).await?;
        let mut found = find(&level)?;
        while found.is_none() && level.unread() {
            level = self
                .read_level(provider, parent, Some(level), usize::MAX)
                .await?
                .ok_or("the app's directory kept changing while it was read; try again")?;
            levels.insert(parent.to_owned(), level.clone());
            found = find(&level)?;
        }
        Ok((found, level))
    }

    /// The directory under `parent` from the page `mark` names on, until it
    /// holds more than `children` resources or ends.
    ///
    /// A directory that moved on since `mark` was read is read on from the same row at the
    /// revision it stands at now, as often as it moves on meanwhile ([`RESUMES`]): writes
    /// beside a listing (an import's) move a directory on between every two pages of it, and
    /// read again from its first page, a listing of thousands never got back to where it
    /// stood before the next write, and answered pages of nothing while its cursor went on.
    /// A provider whose cursor does not carry over to another revision refuses it again, and
    /// the read is `None` as the directory moved on.
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
        let mut resumed = 0_u32;
        while level.children.len() <= children && level.unread() {
            let before = level.clone();
            match self
                .read_pages(provider, parent, level, children.saturating_add(1))
                .await?
            {
                Ok(read) => level = read,
                Err(Unread::MovedOn(now)) if resumed < RESUMES && !now.is_empty() => {
                    resumed = resumed.saturating_add(1);
                    let mut pages = std::sync::Arc::unwrap_or_clone(before.pages);
                    let Some(rest) = pages.rest.as_mut().filter(|rest| rest.revision != now) else {
                        return Ok(None);
                    };
                    rest.revision = now;
                    level = Level {
                        pages: std::sync::Arc::new(pages),
                        ..before
                    };
                }
                Err(_) => return Ok(None),
            }
        }
        Ok(Some(level))
    }

    /// Up to [`LEVEL_PAGES`] pages of the directory under `parent`, from
    /// the start or on from where `from` stopped, and no page past the
    /// `reach`th resource while fewer would not do: a listing asks for the
    /// resources its page holds, not a fixed stretch of the directory. It
    /// starts again from the first page while the directory moves on between
    /// pages or the app cannot read it.
    pub(super) async fn read_level(
        &self,
        provider: &str,
        parent: &str,
        from: Option<Level>,
        reach: usize,
    ) -> Result<Option<Level>, String> {
        // A level is one snapshot: every page of it at one revision. When the
        // app's directory moves on between pages (its document changed, or an
        // invocation refreshed it), what was read belongs to the old revision
        // and the read starts again from the first page at the new one, for
        // as long as an action is waited for, pausing longer each time a
        // directory that keeps changing is read again. An app that cannot
        // read its directory right now, as while another actor's action is
        // being applied, is read again the same way: that is not a level
        // with nothing in it, and once the wait is over the caller is told
        // the app cannot list, never that it lacks what was named.
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
            let unread = match self.read_pages(provider, parent, from, reach).await? {
                Ok(read) => return Ok(Some(read)),
                Err(unread) => unread,
            };
            if deadline.is_none_or(|deadline| tokio::time::Instant::now() >= deadline) {
                return match unread {
                    Unread::MovedOn(_) => Ok(None),
                    Unread::CannotNow => {
                        Err("the app cannot list what it has right now; try again".into())
                    }
                    Unread::Unavailable(reason) => Err(reason),
                };
            }
            from = Level::default();
        }
    }

    /// Up to [`LEVEL_PAGES`] pages of the directory under `parent` on from where `from` stopped,
    /// without starting again, saying why they gave no level when they gave none, and stopping
    /// once the directory holds more than `reach` resources.
    async fn read_pages(
        &self,
        provider: &str,
        parent: &str,
        from: Level,
        reach: usize,
    ) -> Result<Result<Level, Unread>, String> {
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
        let held = rows.len();
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
                    max_results: page_rows(&rows, parent, reach),
                    limits: Some(CatalogueLimits {
                        max_bytes: Some(65536),
                        max_work: Some(512),
                        max_depth: Some(16),
                    }),
                    by_resource: true,
                    ..CatalogueQuery::default()
                })
                .await?;
            if asked.stale_cursor {
                // A read bounded to what a listing shows keeps the pages it
                // holds, all of one revision, and leaves the rest to whoever
                // pages past them: while an import writes between every two
                // pages, starting again never read past the first, and the
                // listing failed after its whole wait.
                if reach != usize::MAX
                    && rows.len() > held
                    && marks.last().is_some_and(|mark| mark.row == rows.len())
                {
                    rest = marks.pop();
                    break;
                }
                return Ok(Err(Unread::MovedOn(asked.revision)));
            }
            // What the caller named could not be listed: said in place of a
            // level with nothing in it, and looked for again while the wait
            // lasts, as a directory still being read is.
            if let Some(reason) = &asked.unavailable {
                return Ok(Err(Unread::Unavailable(reason.clone())));
            }
            if cannot_be_read_now(&asked) {
                return Ok(Err(Unread::CannotNow));
            }
            rows.extend(asked.rows);
            read_at.clone_from(&asked.revision);
            if asked.exhausted || asked.next_cursor.is_empty() {
                break;
            }
            // Enough resources for the caller: where the directory goes on
            // is kept, so reading the next page starts there.
            if reach != usize::MAX && distinct_children(&rows, parent) > reach {
                rest = Some(PageMark {
                    row: rows.len(),
                    cursor: asked.next_cursor,
                    revision: asked.revision,
                });
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
        Ok(Ok(read))
    }

    /// The resources an app's path starts from: its top-level resources and
    /// the resources inside its containers (a notebook's pages are the app's
    /// own), with the containers' own actions (`notes#new-page`). A chain
    /// among them holds the app's items: it is listed first, with the items
    /// it holds, and its actions are the app's before any other's
    /// (`notes#new-note`).
    pub(super) async fn app_children(
        &self,
        provider: &str,
        levels: &mut Levels,
    ) -> Result<Level, String> {
        self.app_children_reading(provider, levels, false).await
    }

    /// [`Self::app_children`] for a listing: what each container and chain holds is read as
    /// far as a listing shows ([`Self::level_near`]), not the whole of it. A
    /// tracker of thousands of issues read whole took several pages, each waiting behind
    /// whatever the workspace was writing, and while an import wrote between every two of
    /// them the read never held one revision and the listing failed.
    pub(super) async fn app_children_listed(
        &self,
        provider: &str,
        levels: &mut Levels,
    ) -> Result<Level, String> {
        self.app_children_reading(provider, levels, true).await
    }

    async fn app_children_reading(
        &self,
        provider: &str,
        levels: &mut Levels,
        listed: bool,
    ) -> Result<Level, String> {
        let mut root = self.level(provider, "", levels).await?;
        let containers: Vec<Node> = root
            .children
            .iter()
            .filter(|node| node.container)
            .take(ROOT_PEEK)
            .cloned()
            .collect();
        for node in containers {
            let inside = if listed {
                self.level_near(provider, &node.id, levels).await?
            } else {
                self.level(provider, &node.id, levels).await?
            };
            for child in inside.whole_children() {
                if !root.children.iter().any(|seen| seen.id == child.id) {
                    root.children.push(child.clone());
                }
            }
            root.own.extend(inside.own);
        }
        let chains: Vec<Node> = root
            .children
            .iter()
            .filter(|node| collab_types::document::chain_of_resource_id(&node.id).is_some())
            .take(ROOT_PEEK)
            .cloned()
            .collect();
        let mut own: Vec<ActionDescriptor> = Vec::new();
        for chain in chains.iter().rev() {
            root.children.retain(|node| node.id != chain.id);
            root.children.insert(0, chain.clone());
        }
        for chain in &chains {
            for action in &chain.actions {
                if !own
                    .iter()
                    .any(|seen| seen.address.action == action.address.action)
                {
                    own.push(action.clone());
                }
            }
            if chain.container {
                let inside = if listed {
                    self.level_near(provider, &chain.id, levels).await?
                } else {
                    self.level(provider, &chain.id, levels).await?
                };
                for child in inside.whole_children() {
                    if !root.children.iter().any(|seen| seen.id == child.id) {
                        root.children.push(child.clone());
                    }
                }
            }
        }
        // An action of the same id elsewhere is reached through its resource.
        root.own.retain(|action| {
            !own.iter()
                .any(|chain| chain.address.action == action.address.action)
        });
        own.append(&mut root.own);
        root.own = own;
        let all = root.children.clone();
        root.children
            .retain(|node| !all.iter().any(|owner| owner.beside(node)));
        Ok(root)
    }
}

/// How many resources of the level under `parent` `rows` name: each once,
/// and never `parent` itself, whose own actions are the level's own.
/// How many of the provider's rows `rows` stand for: a resource's actions sent as one row
/// ([`CatalogueQuery::by_resource`]) are a row each where the provider counts its cursor.
fn spanned(rows: &[CatalogueRow]) -> usize {
    rows.iter()
        .map(|row| {
            row.offered
                .as_ref()
                .map_or(1, |offered| offered.actions.len().max(1))
        })
        .sum()
}

fn distinct_children(rows: &[CatalogueRow], parent: &str) -> usize {
    let mut seen = HashSet::new();
    for row in rows {
        if row.resource.id != parent {
            seen.insert(row.resource.id.as_str());
        }
    }
    seen.len()
}
