//! How far a directory listing is read: the reads that page a level in, up to
//! the pages a first read is bound to or on to the resource a listing wants.

use super::walk::{
    LEVEL_PAGES, LEVEL_WAIT, Levels, PAGE_ROWS, ROOT_PEEK, Tree, Unresolved, as_the_service,
    cannot_be_read_now, is_chain, restart_pause,
};
use super::{Level, LevelPages, Node, PageMark, distinct_children, level_of};
use crate::ActionDescriptor;
use crate::remote::{ActionProvider, CatalogueLimits, CatalogueQuery, CatalogueRow};

/// How often [`Tree::level_from`] reads on from where it stood at the revision a
/// directory moved on to, while writes beside it keep moving it on.
const RESUMES: u32 = 16;

/// Whether `level` was read on past the pages its first read is bound to
/// ([`LEVEL_PAGES`]): each page after the first leaves its mark.
pub fn read_on(level: &Level) -> bool {
    level.pages.marks.len() >= LEVEL_PAGES
}

/// What to ask a provider for so a page reaches `reach` resources of the
/// level under `parent`, from what `rows` already read: as many rows as a page
/// taken a resource at a time holds ([`CatalogueQuery::by_resource`]), and the
/// resources still wanted, one past them to say whether the level goes on
/// ([`CatalogueQuery::max_resources`]). The provider stops there, building
/// the resources the page shows and not as many as a guess at the rows each
/// offers. A read of the whole level keeps to the pages it is bounded to.
fn page_bounds(rows: &[CatalogueRow], parent: &str, reach: usize) -> (u32, Option<u32>) {
    if reach == usize::MAX {
        return (PAGE_ROWS, None);
    }
    let wanted = reach
        .saturating_sub(distinct_children(rows, parent))
        .saturating_add(1);
    (
        crate::remote::MAX_BY_RESOURCE_PAGE,
        Some(u32::try_from(wanted).unwrap_or(u32::MAX)),
    )
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

impl<P: ActionProvider + Sync + Clone + Send + 'static> Tree<P> {
    /// `level`, read on until it holds more than `children` resources or
    /// the directory ends: what a listing paging past the first bounded
    /// read needs, so every resource a directory holds is reachable.
    pub async fn level_reaching(
        &self,
        provider: &str,
        parent: &str,
        mut level: Level,
        children: usize,
    ) -> Result<Level, Unresolved> {
        while level.children.len() <= children && level.unread() {
            let had = level.children.len();
            level = self
                .read_level(provider, parent, Some(level), children.saturating_add(1))
                .await?
                .ok_or(Unresolved::KeptChanging)?;
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
    pub async fn level_holding(
        &self,
        provider: &str,
        parent: &str,
        levels: &mut Levels,
        find: impl Fn(&Level) -> Result<Option<Node>, Unresolved> + Send,
    ) -> Result<(Option<Node>, Level), Unresolved> {
        let mut level = self.level(provider, parent, levels).await?;
        let mut found = find(&level)?;
        while found.is_none() && level.unread() {
            level = self
                .read_level(provider, parent, Some(level), usize::MAX)
                .await?
                .ok_or(Unresolved::KeptChanging)?;
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
    pub async fn level_from(
        &self,
        provider: &str,
        parent: &str,
        mark: PageMark,
        children: usize,
    ) -> Result<Option<Level>, Unresolved> {
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
    pub async fn read_level(
        &self,
        provider: &str,
        parent: &str,
        from: Option<Level>,
        reach: usize,
    ) -> Result<Option<Level>, Unresolved> {
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
        let deadline = self.pause().now().checked_add(LEVEL_WAIT);
        let mut from = from.unwrap_or_default();
        let mut reads: u32 = 0;
        loop {
            let pause = restart_pause(reads);
            if !pause.is_zero()
                && let Some(deadline) = deadline
            {
                let now = self.pause().now();
                let until = now
                    .checked_add(pause)
                    .map_or(deadline, |until| until.min(deadline));
                self.pause().pause(until.saturating_sub(now)).await;
            }
            reads = reads.saturating_add(1);
            let unread = match self.read_pages(provider, parent, from, reach).await? {
                Ok(read) => return Ok(Some(read)),
                Err(unread) => unread,
            };
            if deadline.is_none_or(|deadline| self.pause().now() >= deadline) {
                return match unread {
                    Unread::MovedOn(_) => Ok(None),
                    Unread::CannotNow => Err(Unresolved::CannotListNow),
                    Unread::Unavailable(reason) => Err(Unresolved::Unavailable(reason)),
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
    ) -> Result<Result<Level, Unread>, Unresolved> {
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
            let (max_results, max_resources) = page_bounds(&rows, parent, reach);
            let asked = self
                .ask(CatalogueQuery {
                    provider: Some(provider.to_owned()),
                    parent_resource: (!parent.is_empty()).then(|| parent.to_owned()),
                    cursor: std::mem::take(&mut cursor),
                    revision: revision.take(),
                    max_results,
                    max_resources,
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
    /// own), with the containers' own actions (`notes#new-page`). A root
    /// resource's actions are the app's too, each under the root it is
    /// published on (`notes/n1#new-note`), so a caller listing the app calls
    /// one without naming the root first. A chain among them holds the app's
    /// items: it is listed first, with the items it holds, and its actions
    /// are the app's before any other's (`notes#new-note`).
    pub async fn app_children(
        &self,
        provider: &str,
        levels: &mut Levels,
    ) -> Result<Level, Unresolved> {
        self.app_children_reading(provider, levels, None).await
    }

    /// [`Self::app_children`] for a listing: what each container and chain
    /// holds is read as far as one page of the listing reaches, `reaching`
    /// resources and one past them so that the page and whether more follow
    /// are whole ([`Self::listed_level`]), not the whole of it. A tracker of
    /// thousands of issues read whole took several pages, each waiting behind
    /// whatever the workspace was writing, and while an import wrote between
    /// every two of them the read never held one revision and the listing
    /// failed.
    pub async fn app_children_listed(
        &self,
        provider: &str,
        levels: &mut Levels,
        reaching: usize,
    ) -> Result<Level, Unresolved> {
        self.app_children_reading(provider, levels, Some(reaching))
            .await
    }

    /// The level under `parent` a listing shows: read near first, so a
    /// level it does not page past costs a page ([`Self::level_near`]), then
    /// on until it holds more than `reaching` resources ([`Self::level_reaching`]).
    /// A listing of an app's own page merges what its containers hold and
    /// pages it by offset: the rows it merges are the ones up to the page's
    /// end, one past it saying more follow, and no resource later is built.
    async fn listed_level(
        &self,
        provider: &str,
        parent: &str,
        levels: &mut Levels,
        reaching: usize,
    ) -> Result<Level, Unresolved> {
        let near = self.level_near(provider, parent, levels).await?;
        self.level_reaching(provider, parent, near, reaching).await
    }

    async fn app_children_reading(
        &self,
        provider: &str,
        levels: &mut Levels,
        listing: Option<usize>,
    ) -> Result<Level, Unresolved> {
        let mut root = self.level(provider, "", levels).await?;
        // The app's own resources, before a container's items are listed beside them: a root
        // resource is one of these, and what it offers is the app's to name.
        let roots: Vec<Node> = root.children.clone();
        let containers: Vec<Node> = root
            .children
            .iter()
            .filter(|node| node.container)
            .take(ROOT_PEEK)
            .cloned()
            .collect();
        for node in containers {
            let inside = match listing {
                Some(reaching) => {
                    self.listed_level(provider, &node.id, levels, reaching)
                        .await?
                }
                None => self.level(provider, &node.id, levels).await?,
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
            .filter(|node| is_chain(&node.id))
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
                let inside = match listing {
                    Some(reaching) => {
                        self.listed_level(provider, &chain.id, levels, reaching)
                            .await?
                    }
                    None => self.level(provider, &chain.id, levels).await?,
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
        // A host service's one resource is the service itself, before its actions are taken
        // for an app's root's.
        as_the_service(provider, &mut root);
        // A root resource's actions are the app's own, each under the root it is published on:
        // a listing of the app offers them one call away (`notes/n1#new-note`), and a call
        // naming the app finds them before it would read what the root holds. A control the
        // window draws, or a host's operation, is no root: it is addressed, never listed.
        for node in roots
            .iter()
            .filter(|node| !crate::kind_mirrors_a_control(&node.resource_type))
        {
            for action in node.listed_actions() {
                if !root
                    .own
                    .iter()
                    .any(|seen| seen.address.action == action.address.action)
                {
                    root.own.push(action.clone());
                }
            }
        }
        let all = root.children.clone();
        root.children
            .retain(|node| !all.iter().any(|owner| owner.beside(node)));
        Ok(root)
    }
}
