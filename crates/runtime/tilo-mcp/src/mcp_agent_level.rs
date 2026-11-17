//! How far a directory listing was read, and where it goes on: the pages a
//! level was read from, the cursor `tilo_ls` hands out to read on from the
//! page its next resource starts in, and the reads that page a level in.

use std::collections::HashSet;

use super::{
    ActionDescriptor, ActionProvider, CatalogueLimits, CatalogueQuery, CatalogueRow,
    DEFAULT_WAIT_MS, LEVEL_PAGES, Level, Levels, McpServer, Node, PAGE_ROWS, ROOT_PEEK,
    cannot_be_read_now, level_of, restart_pause,
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

    /// Whether the level was read on past the pages its first read is bound to
    /// ([`LEVEL_PAGES`]): each page after the first leaves its mark.
    pub(super) fn read_on(&self) -> bool {
        self.pages.marks.len() >= LEVEL_PAGES
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

/// Why a read of a level's pages gave no level.
enum Unread {
    /// The app's directory moved on between pages.
    MovedOn,
    /// The app cannot read its directory right now ([`cannot_be_read_now`]).
    CannotNow,
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
            level = self
                .read_level(provider, parent, Some(level))
                .await?
                .ok_or("the app's directory kept changing while it was read; try again")?;
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
                .read_level(provider, parent, Some(level))
                .await?
                .ok_or("the app's directory kept changing while it was read; try again")?;
            levels.insert(parent.to_owned(), level.clone());
            found = find(&level)?;
        }
        Ok((found, level))
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
    /// first page while the directory moves on between pages or the app
    /// cannot read it.
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
            let unread = match self.read_pages(provider, parent, from).await? {
                Ok(read) => return Ok(Some(read)),
                Err(unread) => unread,
            };
            if deadline.is_none_or(|deadline| tokio::time::Instant::now() >= deadline) {
                return match unread {
                    Unread::MovedOn => Ok(None),
                    Unread::CannotNow => {
                        Err("the app cannot list what it has right now; try again".into())
                    }
                };
            }
            from = Level::default();
        }
    }

    /// [`Self::read_level`] without starting again: `None` when the
    /// directory moved on between pages, or cannot be read right now.
    pub(super) async fn read_level_once(
        &self,
        provider: &str,
        parent: &str,
        from: Level,
    ) -> Result<Option<Level>, String> {
        Ok(self.read_pages(provider, parent, from).await?.ok())
    }

    /// The pages [`Self::read_level_once`] reads, saying why they gave no
    /// level when they gave none.
    async fn read_pages(
        &self,
        provider: &str,
        parent: &str,
        from: Level,
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
                return Ok(Err(Unread::MovedOn));
            }
            if cannot_be_read_now(&asked) {
                return Ok(Err(Unread::CannotNow));
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
        let mut root = self.level(provider, "", levels).await?;
        let containers: Vec<Node> = root
            .children
            .iter()
            .filter(|node| node.container)
            .take(ROOT_PEEK)
            .cloned()
            .collect();
        for node in containers {
            let inside = self.level(provider, &node.id, levels).await?;
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
                let inside = self.level(provider, &chain.id, levels).await?;
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
