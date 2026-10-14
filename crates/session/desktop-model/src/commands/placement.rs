//! Where a newly opened window lands. A strip has unlimited space to the
//! right, so "wherever the tree walk reached first" isn't an answer:
//!
//! 1. **Right of the window you're in** — next to what opened it, not at the
//!    end of the strip.
//! 2. **Into the group you're in, when it's tabbed** — a tab of several means
//!    the person is working *inside* that group.
//! 3. **At a seam, when nothing is selected** — the visible border between
//!    two groups, else the next one rightwards.
//!
//! An app chosen from search skips rules 1–2 and takes the rightmost
//! insertion point in view. A view opened for an actor that is not the
//! person at the keyboard (an agent, a Shortcut, another app) opens as a new
//! group right after the view that actor works in, on that actor's strip,
//! which need not be the one the person is looking at: everyone keeps their
//! place. Rules 1–2 read the tree alone; rule 3 and search
//! need a [`StripViewport`], which only a host that lays the strip out has.
//! A host without a layout or a focus (a headless owner placing an agent's
//! open) gets a new group at the end of its last strip.

use super::TabPlacement;
use crate::ids::{GroupId, StripId};
use crate::tree::DesktopTree;
use crate::workspace::Workspace;

/// One group's horizontal extent, in the strip's **unscrolled canvas
/// space**, so a seam's position doesn't move when the strip is panned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupSpan {
    pub id: GroupId,
    pub x0: f64,
    pub x1: f64,
}

/// A border between windows. The seam *after* the last group counts — the
/// strip is unbounded to the right, so the trailing edge is a border against
/// open space, which is where "rightwards" terminates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seam {
    /// The group whose right edge this seam is.
    pub after: GroupId,
    pub x: f64,
}

/// What the person can currently see of one strip, published by the host
/// that lays it out so an open can consult it without reaching into widgets.
#[derive(Clone, Debug, PartialEq)]
pub struct StripViewport {
    pub strip: StripId,
    /// Every group, in strip order.
    pub groups: Vec<GroupSpan>,
    /// Canvas-x of the visible edges.
    pub visible_x0: f64,
    pub visible_x1: f64,
}

impl StripViewport {
    /// Left to right, including the one after the last group.
    pub fn seams(&self) -> impl DoubleEndedIterator<Item = Seam> + '_ {
        self.groups.iter().map(|g| Seam {
            after: g.id,
            x: g.x1,
        })
    }

    fn on_screen(&self, seam: &Seam) -> bool {
        seam.x > self.visible_x0 && seam.x < self.visible_x1
    }

    /// The leftmost border **between two windows** actually visible.
    /// Excludes a seam sitting exactly on the screen edge (falls through to
    /// [`Self::next_seam_rightwards`]) and the one after the *last* window, a
    /// border against open space.
    pub fn visible_seam(&self) -> Option<Seam> {
        let between_windows = self.groups.len().saturating_sub(1);
        self.seams()
            .take(between_windows)
            .find(|s| self.on_screen(s))
    }

    /// The rightmost place on the screen where a new group can be inserted.
    /// Unlike [`Self::visible_seam`], this includes the trailing edge of the
    /// last window: visible canvas after it is the most natural place for a
    /// searched app.
    pub fn rightmost_visible_seam(&self) -> Option<Seam> {
        self.seams().rev().find(|s| self.on_screen(s))
    }

    /// Falls back to the rightmost seam when every seam is behind the
    /// viewport — the end of the strip is still a seam.
    pub fn next_seam_rightwards(&self) -> Option<Seam> {
        self.seams()
            .find(|s| s.x >= self.visible_x1)
            .or_else(|| self.seams().last())
    }
}

/// Why a window is being opened, which decides the rule that places it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpenIntent {
    /// A launch beside what the person is working in (rules 1–3).
    #[default]
    Launch,
    /// An app explicitly chosen from search: spatially predictable rather
    /// than focus-relative, so selecting a result never silently becomes a
    /// tab just because the focused group holds several.
    Search,
    /// A view an actor's action needs, opened beside the group that actor
    /// works in ([`OpenContext::beside`]): a new group right after it, never
    /// a tab inside it and never at the person's focus.
    Beside,
}

/// What an open is placed against: the person's focus and, when the host
/// lays the strip out, what of it is on screen. The default — no focus, no
/// layout — is a headless owner's turn.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenContext<'a> {
    pub intent: OpenIntent,
    /// The strip the person is in.
    pub strip: Option<StripId>,
    /// The group selected in that strip.
    pub selected: Option<GroupId>,
    pub viewport: Option<&'a StripViewport>,
}

impl<'a> OpenContext<'a> {
    /// The active workspace's focus in `tree`: its active strip (else its
    /// first) and that strip's selected group.
    pub fn focused(
        tree: &DesktopTree,
        intent: OpenIntent,
        viewport: Option<&'a StripViewport>,
    ) -> Self {
        let strip = tree.active_workspace().and_then(|ws| {
            tree.active_strip
                .get(&ws.id)
                .filter(|id| ws.strips.iter().any(|s| s.id == **id))
                .or_else(|| ws.strips.first().map(|s| &s.id))
                .copied()
        });
        Self {
            intent,
            strip,
            selected: strip.and_then(|strip| tree.active_group_for(strip)),
            viewport,
        }
    }
}

impl OpenContext<'_> {
    /// Opening beside `group`, on whichever strip of `workspace` holds it.
    /// `None` when no strip holds it any more.
    #[must_use]
    pub fn beside(workspace: &Workspace, group: GroupId) -> Option<Self> {
        let strip = workspace
            .strips
            .iter()
            .find(|strip| strip.window_groups.iter().any(|g| g.id == group))?;
        Some(Self {
            intent: OpenIntent::Beside,
            strip: Some(strip.id),
            selected: Some(group),
            viewport: None,
        })
    }
}

/// Where an opened app goes in `workspace`, inside the strips and groups
/// `owns` accepts so an owner keeps placement in its own topology namespace.
///
/// # Errors
///
/// Returns a description when no strip `owns` accepts can receive the tab.
pub fn open_app_placement(
    workspace: &Workspace,
    at: &OpenContext<'_>,
    owns: impl Fn(u64) -> bool,
) -> Result<TabPlacement, String> {
    if workspace.strips.is_empty() {
        return Ok(TabPlacement::EmptyWorkspace);
    }
    let strip = at
        .strip
        .and_then(|id| workspace.strips.iter().find(|s| s.id == id))
        .filter(|s| owns(s.id.0))
        .or_else(|| workspace.strips.iter().rev().find(|s| owns(s.id.0)))
        .ok_or("no desktop strip can receive the opened application")?;
    let new_group = |after: Option<GroupId>| TabPlacement::NewGroup {
        strip: strip.id.0,
        after,
    };
    let seam = |pick: fn(&StripViewport) -> Option<Seam>| {
        at.viewport
            .filter(|view| view.strip == strip.id)
            .and_then(|view| pick(view).or_else(|| view.next_seam_rightwards()))
            .map(|seam| seam.after)
            .filter(|group| owns(*group))
    };
    Ok(match at.intent {
        OpenIntent::Launch => {
            let selected = at
                .selected
                .and_then(|id| strip.window_groups.iter().find(|g| g.id == id))
                .filter(|g| owns(g.id));
            match selected {
                Some(group) if group.tabs.len() > 1 => TabPlacement::InGroup {
                    strip: strip.id.0,
                    group: group.id,
                    after: None,
                },
                Some(group) => new_group(Some(group.id)),
                None => new_group(seam(StripViewport::visible_seam)),
            }
        }
        OpenIntent::Beside => new_group(
            at.selected
                .filter(|id| strip.window_groups.iter().any(|g| g.id == *id))
                .or_else(|| strip.window_groups.last().map(|g| g.id))
                .filter(|group| owns(*group)),
        ),
        OpenIntent::Search => {
            new_group(seam(StripViewport::rightmost_visible_seam).or_else(|| {
                strip
                    .window_groups
                    .last()
                    .map(|g| g.id)
                    .filter(|group| owns(*group))
            }))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppKind;
    use crate::ids::WorkspaceId;
    use crate::layout::{Strip, Tab, TileContent, WindowGroup};
    use crate::width::WindowWidth;

    const STRIP: StripId = StripId(7);

    /// One strip whose i-th group (id `100 + i`) holds `tab_counts[i]` tabs.
    fn workspace(tab_counts: &[usize]) -> (Workspace, Vec<GroupId>) {
        let mut workspace = Workspace::new(WorkspaceId(1), "W");
        let mut ids = Vec::new();
        let mut next = 1001_u64..;
        let window_groups = tab_counts
            .iter()
            .zip(100..)
            .map(|(count, id)| {
                ids.push(id);
                let tabs: Vec<Tab> = (0..*count)
                    .zip(&mut next)
                    .map(|(_, tab)| Tab {
                        id: tab,
                        title: "t".into(),
                        tile: TileContent::Stub {
                            kind: AppKind::Terminal,
                            label: "t".into(),
                        },
                    })
                    .collect();
                WindowGroup {
                    id,
                    active_tab: tabs[0].id,
                    tabs,
                    width: WindowWidth::Normal,
                    maximized: false,
                }
            })
            .collect();
        workspace.strips = vec![Strip {
            id: STRIP,
            name: "S".into(),
            window_groups,
        }];
        (workspace, ids)
    }

    /// Even spans, 100 wide each.
    fn viewport(ids: &[GroupId], visible: (f64, f64)) -> StripViewport {
        StripViewport {
            strip: STRIP,
            groups: ids
                .iter()
                .zip(0u32..)
                .map(|(id, i)| GroupSpan {
                    id: *id,
                    x0: f64::from(i) * 100.0,
                    x1: f64::from(i) * 100.0 + 100.0,
                })
                .collect(),
            visible_x0: visible.0,
            visible_x1: visible.1,
        }
    }

    fn place(
        workspace: &Workspace,
        intent: OpenIntent,
        selected: Option<GroupId>,
        viewport: Option<&StripViewport>,
    ) -> TabPlacement {
        let at = OpenContext {
            intent,
            strip: Some(STRIP),
            selected,
            viewport,
        };
        open_app_placement(workspace, &at, |_| true).unwrap()
    }

    fn right_of(group: GroupId) -> TabPlacement {
        TabPlacement::NewGroup {
            strip: STRIP.0,
            after: Some(group),
        }
    }

    #[test]
    fn a_launch_follows_the_selection_or_the_visible_seam() {
        use OpenIntent::Launch;
        let (w, ids) = workspace(&[1, 1, 1]);
        assert_eq!(place(&w, Launch, Some(ids[1]), None), right_of(ids[1]));

        let (tabbed, tabbed_ids) = workspace(&[1, 3, 1]);
        assert_eq!(
            place(&tabbed, Launch, Some(tabbed_ids[1]), None),
            TabPlacement::InGroup {
                strip: STRIP.0,
                group: tabbed_ids[1],
                after: None
            },
        );

        // Canvas 150..280: the seam at 200 is on screen, 100 is off it.
        let view = viewport(&ids, (150.0, 280.0));
        assert_eq!(place(&w, Launch, None, Some(&view)), right_of(ids[1]));
        // Inside group 1: no border on screen, so the next one rightwards.
        let view = viewport(&ids, (120.0, 190.0));
        assert_eq!(place(&w, Launch, None, Some(&view)), right_of(ids[1]));
        // A border exactly on the screen edge is not a space you can see.
        let view = viewport(&ids, (0.0, 100.0));
        assert_eq!(view.visible_seam(), None);
        assert_eq!(place(&w, Launch, None, Some(&view)), right_of(ids[0]));

        let (lone, lone_ids) = workspace(&[1]);
        let view = viewport(&lone_ids, (0.5, 99.5));
        assert_eq!(
            place(&lone, Launch, None, Some(&view)),
            right_of(lone_ids[0])
        );

        let (pair, pair_ids) = workspace(&[1, 1]);
        let view = viewport(&pair_ids, (400.0, 800.0));
        assert_eq!(
            place(&pair, Launch, None, Some(&view)),
            right_of(pair_ids[1]),
            "panning past the last window still lands at the end",
        );

        let mut other = viewport(&ids, (150.0, 280.0));
        other.strip = StripId(99);
        assert_eq!(
            place(&w, Launch, None, Some(&other)),
            TabPlacement::NewGroup {
                strip: STRIP.0,
                after: None
            },
            "a snapshot of a different strip must not decide this one",
        );
    }

    #[test]
    fn search_takes_the_rightmost_visible_seam() {
        use OpenIntent::Search;
        let (w, ids) = workspace(&[3, 1, 1, 1]);
        let view = viewport(&ids, (0.0, 350.0));
        assert_eq!(
            place(&w, Search, Some(ids[0]), Some(&view)),
            right_of(ids[2])
        );

        let (pair, pair_ids) = workspace(&[1, 1]);
        let view = viewport(&pair_ids, (0.0, 350.0));
        assert_eq!(
            place(&pair, Search, None, Some(&view)),
            right_of(pair_ids[1])
        );

        let (three, three_ids) = workspace(&[1, 1, 1]);
        let view = viewport(&three_ids, (20.0, 90.0));
        assert_eq!(
            place(&three, Search, None, Some(&view)),
            right_of(three_ids[0])
        );
        assert_eq!(place(&three, Search, None, None), right_of(three_ids[2]));
    }

    #[test]
    fn an_actors_view_opens_beside_its_group_on_its_own_strip() {
        let (mut w, ids) = workspace(&[1, 3, 1]);
        let mut other = Strip::new(StripId(8), "Agent");
        other.window_groups.push(WindowGroup {
            id: 900,
            active_tab: 901,
            tabs: vec![Tab {
                id: 901,
                title: "a".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Terminal,
                    label: "a".into(),
                },
            }],
            width: WindowWidth::Normal,
            maximized: false,
        });
        w.strips.push(other);

        let tabbed = OpenContext::beside(&w, ids[1]).expect("group 1 is on a strip");
        assert_eq!(
            open_app_placement(&w, &tabbed, |_| true).unwrap(),
            right_of(ids[1]),
            "beside a tabbed group, never a tab inside it",
        );
        let elsewhere = OpenContext::beside(&w, 900).expect("group 900 is on a strip");
        assert_eq!(
            open_app_placement(&w, &elsewhere, |_| true).unwrap(),
            TabPlacement::NewGroup {
                strip: 8,
                after: Some(900)
            },
            "the actor's strip, not the person's",
        );
        assert!(OpenContext::beside(&w, 4242).is_none());
    }

    #[test]
    fn a_headless_open_lands_at_the_end_of_the_last_owned_strip() {
        let (mut w, _) = workspace(&[1]);
        w.strips.push(Strip::new(StripId(8), "Other"));
        let headless = OpenContext::default();
        assert_eq!(
            open_app_placement(&w, &headless, |id| id != 8).unwrap(),
            TabPlacement::NewGroup {
                strip: STRIP.0,
                after: None
            },
        );
        assert!(open_app_placement(&w, &headless, |_| false).is_err());
        w.strips.clear();
        assert_eq!(
            open_app_placement(&w, &headless, |_| false).unwrap(),
            TabPlacement::EmptyWorkspace
        );
    }
}
