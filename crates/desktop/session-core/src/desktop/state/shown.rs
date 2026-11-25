//! Where the windows the shell shows stand, as the last layout left them: the whole of each window,
//! and the bounds of the tab it shows. Every display shows a strip of its own, so that is every row
//! the shell draws, not only the one the person works in.
use tilo_desktop_model::{LayoutKind, StripId, TileContent};
use ui::kurbo::Rect;
use ui::widget::{
    FLOAT_CARD_MARGIN, FLOAT_CARD_SIZE, HANDLE_SLOT_PX, STRIP_BOTTOM_GUTTER_PX,
    STRIP_EDGE_GUTTER_PX, STRIP_TOP_GAP_PX,
};

use super::{DesktopHandle, GroupId, Shared, TabId, displays, floats};

impl DesktopHandle {
    /// Where each window the shell draws draws its active tab, in the surface's coordinates, as of
    /// the last layout. Hidden tabs are absent, and so is a window whose display shows another
    /// strip than its own.
    pub fn shown_tabs(&self) -> Vec<(TabId, Rect)> {
        self.shared.borrow().shown_tabs.content()
    }

    /// Where each drawn window's active tab stands, in the surface's coordinates, as of the last
    /// layout: the tab's content where it has laid out, and the whole window where it has not (a
    /// window this device shows but has not opened, whose position an offset on it still means
    /// something across). Hidden tabs are absent.
    pub fn shown_tiles(&self) -> Vec<(TabId, Rect)> {
        self.shared.borrow().shown_tabs.tiles()
    }

    /// Whether a window the shell draws shows `item` of `app` in the tab in front, as of the last
    /// layout: the window it opened on ([`DesktopHandle::set_window_items`]) stands on screen, so
    /// what lands in the item is before the person.
    #[must_use]
    pub fn shows_item(&self, app: &str, item: &str) -> bool {
        let shown = self.shown_tabs();
        let items = self.shared.borrow().items.clone();
        self.read(|tree| {
            tree.tabs().any(|tab| {
                items.get(&tab.id).is_some_and(|opened| opened == item)
                    && shown.iter().any(|(id, _)| *id == tab.id)
                    && tilo_desktop_model::apps::canonical_app_id(&tab.tile)
                        .is_some_and(|id| tilo_desktop_model::apps::app_ids_match(&id, app))
            })
        })
    }

    /// The top of the drawn windows' content, in the surface's coordinates,
    /// as of the last layout: where the transients pinned into the display's
    /// top-right corner start, below the windows' bars. Zero with no window.
    pub fn content_top(&self) -> f64 {
        self.shared.borrow().shown_tabs.content_top()
    }

    /// The window covering its display whose own bar stands over its content, as the person
    /// reached for the bar over the display, and where that bar stands, in the surface's
    /// coordinates, as of the last layout. The window is drawn around it, and a press there is the
    /// bar's.
    pub fn covering_bar(&self) -> Option<(TabId, Rect)> {
        self.shared.borrow().shown_tabs.bar_over_content()
    }

    /// Each window floating over the shell, and the rect the shell draws over the windows under it
    /// for that window — its bar, its body and the shadow that lifts it, whole and opaque — in the
    /// surface's coordinates, as of the last draw. A window that has not finished fading in is
    /// absent: what the shell draws of it yet lets the windows it covers show through.
    pub fn float_covers(&self) -> Vec<(TabId, Rect)> {
        self.shared.borrow().float_covers.clone()
    }

    /// The application the window of `tab` runs, or `None` for a window that is not an
    /// application's.
    #[must_use]
    pub fn app_of(&self, tab: TabId) -> Option<String> {
        crate::desktop::host::records::app_of(&self.shared.borrow().tree, tab).map(str::to_owned)
    }

    /// Where the whole window of `tab` stands — its bar, and the tab's content in it — in the
    /// surface's coordinates, as of the last layout: where a menu for it hangs from.
    #[must_use]
    pub fn window_rect(&self, tab: TabId) -> Option<Rect> {
        self.shared.borrow().shown_tabs.window_rect(tab)
    }

    /// Each window group's size on the display it is laid out on, as of the last layout: the tab's
    /// content — the pixels the window draws — where the window has laid it out, and the whole
    /// window where it has not. A group whose row is not drawn (its strip is not on screen, or its
    /// display shows another strip) is not here; a host pictures it at the size its share of the
    /// strip would give it. A picture of the window fills its card by this size: a card drawn from
    /// it stands at the window's own aspect, so what the window shows fills the card rather than
    /// leaving bars of ground round a smaller picture.
    #[must_use]
    pub fn group_sizes(&self) -> Vec<(GroupId, ui::kurbo::Size)> {
        self.shared.borrow().shown_tabs.sizes()
    }
}

/// Where a window the shell draws stands, and the bounds of the tab it shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShownTab {
    /// The tab the window shows: the active one of its bar.
    pub tab: TabId,
    /// The window group's own id, which a picture of it is named by.
    pub group: GroupId,
    /// The window's whole frame: its bar, and the tab's content in it.
    pub window: Rect,
    /// The bounds of the tab's content, once its window laid it out.
    pub content: Option<Rect>,
    /// The window's bar where it stands over that content: a window covering its display shows it
    /// there while the person reaches for the bar over the display.
    pub bar_over_content: Option<Rect>,
    /// Whether the window floats over the shell rather than laid out in its strip.
    pub floated: bool,
}

/// What one strip holds, window by window, as the last layout left it.
#[derive(Debug, Default, PartialEq)]
pub struct ShownTabs(pub Vec<ShownTab>);

impl ShownTabs {
    /// These windows and `other` 's together: every row the shell draws. A window in both (a group
    /// on its way from one strip to another) keeps `other` 's place, the row drawn over it.
    #[must_use]
    pub fn and(mut self, other: Self) -> Self {
        for shown in other.0 {
            match self.0.iter_mut().find(|have| have.tab == shown.tab) {
                Some(have) => *have = shown,
                None => self.0.push(shown),
            }
        }
        self
    }

    /// Where each window draws its active tab: absent while its app's window is not laid out.
    pub fn content(&self) -> Vec<(TabId, Rect)> {
        self.0
            .iter()
            .filter_map(|shown| Some((shown.tab, shown.content?)))
            .collect()
    }

    /// Where each window of the shown strip's active tab stands: the tab's content where its window
    /// laid one out, and the whole window where it has not. A window shown without its content laid
    /// out (one this device has open bit does not draw here) still has a place on the shell, and
    /// what is drawn on its tile — another person's pointer — keeps meaning there.
    pub fn tiles(&self) -> Vec<(TabId, Rect)> {
        self.0
            .iter()
            .map(|shown| (shown.tab, shown.content.unwrap_or(shown.window)))
            .collect()
    }

    /// The window whose bar stands over its own content, and where: the shell draws there over
    /// what the window shows.
    pub fn bar_over_content(&self) -> Option<(TabId, Rect)> {
        self.0
            .iter()
            .find_map(|shown| Some((shown.tab, shown.bar_over_content?)))
    }

    /// Where the whole window of `tab` stands, its bar and content together.
    pub fn window_rect(&self, tab: TabId) -> Option<Rect> {
        let shown = self.0.iter().find(|shown| shown.tab == tab)?;
        Some(shown.window)
    }

    /// Each window group's size on its display: the tab's content where the window laid it out —
    /// the pixels the window draws, which a picture of it shows — and the whole window where it has
    /// not.
    pub fn sizes(&self) -> Vec<(GroupId, ui::kurbo::Size)> {
        self.0
            .iter()
            .map(|shown| (shown.group, shown.content.unwrap_or(shown.window).size()))
            .collect()
    }

    /// The top of the strips' windows' content: where banners start, below the windows' bars, which
    /// pinned to the strip's top would cover their own controls. A floating window stands lower,
    /// and banners keep clear of it whole ([`DesktopHandle::float_covers`]) rather than start
    /// under its bar, over what it shows.
    pub fn content_top(&self) -> f64 {
        self.0
            .iter()
            .filter(|shown| !shown.floated)
            .filter_map(|shown| shown.content)
            .map(|content| content.y0)
            .fold(0.0, f64::max)
    }
}

impl DesktopHandle {
    /// The app of `tab` left a sideways wheel to the shell: its content did not scroll with it, so
    /// the strip its window is on moves by `travel` logical pixels, positive toward later windows,
    /// as the wheel over the strip's own canvas does. What the app scrolled with itself never
    /// arrives here, so a gesture is never split between its content and the strip: the app decided
    /// at its first event ([`ui::widget::sideways_travel`], and the app's own server of an event it
    /// left).
    pub fn sideways_wheel(&self, tab: TabId, travel: f64) {
        let mut shared = self.shared.borrow_mut();
        shared.sideways.push((tab, travel));
        // Where the windows are is nothing the workspace publishes: the shell draws the moved strip
        // in the frame this asks for, and nothing else.
        shared.wake.wake();
    }
}

/// The height of the area a host lays the shell out on before it names a display: a 16:10 display
/// as wide as the viewport ([`DesktopHandle::set_viewport_width`]).
const UNNAMED_DISPLAY_ASPECT: f64 = 10.0 / 16.0;

/// How far one strip shown on one display is panned, and the window it last brought into view
/// there: a strip pans to the window the person moves to, and stays where they panned it until
/// they move to another.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StripPan {
    /// How far the strip's windows are moved left, in logical pixels.
    pub scroll: f64,
    /// The window last brought into view.
    pub revealed: Option<GroupId>,
    /// How much of the strip the display shows, and how wide its windows stand: what a pan
    /// moves within, and what the navigation snapshot presents.
    pub viewport: f64,
    pub content: f64,
    /// The furthest the strip scrolls: `content` less `viewport`, never below zero.
    pub max: f64,
}

/// One strip as one display shows it: the windows it lays out there, in order, over `area`.
struct Section {
    strip: StripId,
    display: Option<u32>,
    area: Rect,
    groups: Vec<GroupId>,
}

/// The area the shell is laid out on for `display`: that display, or with none named the display
/// the person works on, else the one the host last named first, else the viewport.
#[must_use]
pub fn work_area(shared: &Shared, display: Option<u32>) -> Rect {
    let named = display.and_then(|id| shared.displays.iter().find(|shown| shown.id == id));
    let focused = shared.tree.outputs.focused_display();
    named
        .or_else(|| {
            shared
                .displays
                .iter()
                .find(|shown| Some(shown.id) == focused)
        })
        .or_else(|| shared.displays.iter().find(|shown| shown.primary))
        .or_else(|| shared.displays.first())
        .map_or_else(
            || {
                let width = shared.viewport_width.max(1.0);
                Rect::new(0.0, 0.0, width, (width * UNNAMED_DISPLAY_ASPECT).round())
            },
            crate::desktop::display_rect,
        )
}

/// Every strip the shell shows, one section per display it is shown on.
fn sections(shared: &Shared) -> Vec<Section> {
    let tree = &shared.tree;
    if tree.is_partitioned() {
        return displays::display_strips(tree)
            .into_iter()
            .flat_map(|plan| {
                plan.displays
                    .iter()
                    .map(|display| Section {
                        strip: plan.strip,
                        display: Some(*display),
                        area: work_area(shared, Some(*display)),
                        groups: plan
                            .groups
                            .iter()
                            .copied()
                            .filter(|group| plan.assignment.get(group) == Some(display))
                            .collect(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
    }
    let Some(strip) = tree.active_strip_id() else {
        return Vec::new();
    };
    let groups = floats::workspace_with(tree, strip)
        .and_then(|workspace| workspace.strips.iter().find(|held| held.id == strip))
        .map(|held| held.window_groups.iter().map(|group| group.id).collect())
        .unwrap_or_default();
    vec![Section {
        strip,
        display: None,
        area: work_area(shared, None),
        groups,
    }]
}

/// The strip the window `group` is laid out in and the work area it is laid out on: what a float of
/// it is placed against.
pub fn section_holding(shared: &Shared, group: GroupId) -> Option<(StripId, Rect)> {
    sections(shared)
        .into_iter()
        .find(|section| section.groups.contains(&group))
        .map(|section| (section.strip, section.area))
}

/// The offset that brings the window spanning `x0..x1` of a strip's canvas into a view `width`
/// wide panned to `scroll`, on a strip whose windows end at `end`: clear of the view's edges by
/// the gap between two windows, its start first when it is wider than the view, and flush with
/// either end of the strip when it is within a gutter of it.
fn revealing(scroll: f64, x0: f64, x1: f64, width: f64, end: f64) -> f64 {
    let wide = x1 - x0;
    let margin = HANDLE_SLOT_PX.min((width - wide).max(0.0) / 2.0);
    let (left, right) = (scroll + margin, scroll + width - margin);
    let mut to = scroll;
    if wide > right - left {
        to += x0 - left;
    } else if x1 > right {
        to += x1 - right;
    } else if x0 < left {
        to -= left - x0;
    }
    let max = (end + STRIP_EDGE_GUTTER_PX - width).max(0.0);
    if to > 0.0 && to <= STRIP_EDGE_GUTTER_PX + 0.5 && x1 <= width - margin + 0.5 {
        to = 0.0;
    } else if to < max && to >= max - STRIP_EDGE_GUTTER_PX - 0.5 && x0 - max >= margin - 0.5 {
        to = max;
    }
    to.clamp(0.0, max)
}

/// Lay out every strip the shell shows from the tree: each window where its strip puts it on its
/// display, panned to the window the person moved to, and each window floating over the shell
/// where the workspace holds it. The windows a host places and the ones it tiers by what is in
/// view read this ([`DesktopHandle::shown_tabs`], [`DesktopHandle::float_covers`]). Whether
/// anything moved.
pub fn lay_out(shared: &mut Shared) -> bool {
    let reveal_asked = std::mem::take(&mut shared.reveal_asked);
    let mut laid = Laid::default();
    for section in sections(shared) {
        lay_out_section(shared, &section, &reveal_asked, &mut laid);
    }
    let shown = ShownTabs(laid.shown);
    let moved = shared.shown_tabs != shown
        || shared.float_covers != laid.covers
        || shared.pans != laid.pans;
    shared.shown_tabs = shown;
    shared.float_covers = laid.covers;
    shared.pans = laid.pans;
    moved
}

/// What one layout gathers across the strips the shell shows.
#[derive(Default)]
struct Laid {
    shown: Vec<ShownTab>,
    covers: Vec<(TabId, Rect)>,
    pans: std::collections::HashMap<(StripId, Option<u32>), StripPan>,
}

/// Lay out the windows of one strip on one display into `laid`.
fn lay_out_section(
    shared: &Shared,
    section: &Section,
    reveal_asked: &[(StripId, GroupId)],
    laid: &mut Laid,
) {
    let Some(workspace) = floats::workspace_with(&shared.tree, section.strip) else {
        return;
    };
    let Some(strip) = workspace
        .strips
        .iter()
        .find(|held| held.id == section.strip)
    else {
        return;
    };
    let area = section.area;
    let groups: Vec<&tilo_desktop_model::WindowGroup> = section
        .groups
        .iter()
        .filter_map(|id| strip.window_groups.iter().find(|group| group.id == *id))
        .collect();
    let floating = floats::floats_of(shared, section.strip, floats::area_of(area));
    let top = area.y0 + shared.top_overlay + STRIP_TOP_GAP_PX;
    let bottom = (area.y1 - STRIP_BOTTOM_GUTTER_PX).max(top + 1.0);
    let bar = ui::widget::tabs::TAB_BAR_HEIGHT.min(bottom - top);
    let framed = |group: &tilo_desktop_model::WindowGroup, window: Rect, floated: bool| ShownTab {
        tab: group.active_tab,
        group: group.id,
        window,
        content: Some(Rect::new(window.x0, window.y0 + bar, window.x1, window.y1)),
        bar_over_content: None,
        floated,
    };
    // A window covering its display, or one the person maximised, is the one its display shows:
    // the others on it are out of sight until it gives the display back.
    let fullscreen = shared.tree.fullscreen_group();
    if let Some(group) = groups.iter().find(|group| Some(group.id) == fullscreen) {
        laid.shown.push(ShownTab {
            content: Some(area),
            ..framed(group, area, false)
        });
        return;
    }
    if let Some(group) = groups.iter().find(|group| group.maximized) {
        let window = Rect::new(
            area.x0 + STRIP_EDGE_GUTTER_PX,
            top,
            area.x1 - STRIP_EDGE_GUTTER_PX,
            bottom,
        );
        laid.shown.push(framed(group, window, false));
        return;
    }
    let in_strip: Vec<&tilo_desktop_model::WindowGroup> = groups
        .iter()
        .copied()
        .filter(|group| !floating.contains_key(&group.id))
        .collect();
    if workspace.layout.kind == LayoutKind::Canvas {
        for group in in_strip {
            if let Some(place) = workspace.layout.place_of(group.id) {
                laid.shown
                    .push(framed(group, on_area(floats::rect_of(place), area), false));
            }
        }
    } else {
        // Each window of the strip in its run of the canvas, from the display's left edge.
        let usable = (area.width() - 2.0 * STRIP_EDGE_GUTTER_PX).max(1.0);
        let alone = in_strip.len() == 1;
        let mut x = STRIP_EDGE_GUTTER_PX;
        let spans: Vec<(GroupId, f64, f64)> = in_strip
            .iter()
            .map(|group| {
                let width = group.width.resolve_in_strip(usable, alone);
                let span = (group.id, x, x + width);
                x += width + HANDLE_SLOT_PX;
                span
            })
            .collect();
        let pan = pan_section(shared, section, &spans, reveal_asked);
        laid.pans.insert((section.strip, section.display), pan);
        for (group, (_, x0, x1)) in in_strip.iter().zip(spans) {
            let window = Rect::new(
                area.x0 + x0 - pan.scroll,
                top,
                area.x0 + x1 - pan.scroll,
                bottom,
            );
            laid.shown.push(framed(group, window, false));
        }
    }
    for group in groups {
        let Some(at) = floating.get(&group.id) else {
            continue;
        };
        laid.shown.push(framed(group, *at, true));
        // What the shell draws whole over the windows under a floating window: its card, once a
        // window fills it. A place still waiting for its window draws nothing over them.
        let filled = group.tabs.iter().any(|tab| {
            tab.id == group.active_tab && matches!(tab.tile, TileContent::Foreign { .. })
        });
        if filled {
            laid.covers.push((group.active_tab, *at));
        }
    }
}

/// Present the pan of the strip the person works on beside the tree: how far it is scrolled, the
/// view its windows are seen through and how far it may scroll. The navigation snapshot and the
/// `pan` verb read it, and a host that pans a strip by a swipe presents it here.
pub fn present_pan(shared: &mut Shared) {
    // A canvas has no strip to pan: its places stand where the person put them.
    let pans = shared
        .tree
        .active_workspace()
        .is_some_and(|workspace| workspace.layout.kind != LayoutKind::Canvas);
    let Some(strip) = shared.tree.active_strip_id().filter(|_| pans) else {
        let mut next = shared.presentation.clone();
        next.pan = None;
        shared.present(next);
        return;
    };
    let focused = shared.tree.outputs.focused_display();
    let pan = focused
        .and_then(|display| shared.pans.get(&(strip, Some(display))).copied())
        .or_else(|| shared.pans.get(&(strip, None)).copied())
        .or_else(|| {
            shared
                .pans
                .iter()
                .find_map(|((shown, _), pan)| (*shown == strip).then_some(*pan))
        });
    let mut next = shared.presentation.clone();
    next.pan = pan.map(|pan| crate::desktop::navigation::DesktopPan {
        workspace: shared.tree.active.to_string(),
        strip: strip.0.to_string(),
        offset: pan.scroll,
        viewport: pan.viewport,
        content: pan.content,
        max: pan.max,
        // The strip's motion is the shell's own drawing now: the desktop has no frame to coast it
        // into, so it presents where the pan stands, never a motion in flight.
        moving: false,
    });
    shared.present(next);
}

/// `at`, a place on the canvas of the work area `area`, in the surface's coordinates.
fn on_area(at: Rect, area: Rect) -> Rect {
    Rect::new(
        area.x0 + at.x0,
        area.y0 + at.y0,
        area.x0 + at.x1,
        area.y0 + at.y1,
    )
}

/// How far the strip of `section`, its windows at `spans` of its canvas, is panned now: to the
/// window asked into view since the last layout, else to the window the person moved to, else
/// where it was.
fn pan_section(
    shared: &Shared,
    section: &Section,
    spans: &[(GroupId, f64, f64)],
    reveal_asked: &[(StripId, GroupId)],
) -> StripPan {
    let width = section.area.width();
    let end = spans.last().map_or(0.0, |(_, _, x1)| *x1);
    let mut pan = shared
        .pans
        .get(&(section.strip, section.display))
        .copied()
        .unwrap_or_default();
    let laid_here = |group: GroupId| spans.iter().any(|span| span.0 == group);
    let active = shared
        .tree
        .active_group_for(section.strip)
        .filter(|group| laid_here(*group));
    let asked = reveal_asked
        .iter()
        .rev()
        .find(|(strip, group)| *strip == section.strip && laid_here(*group))
        .map(|(_, group)| *group);
    let moved_to = active.filter(|group| pan.revealed != Some(*group));
    if let Some((_, x0, x1)) = asked
        .or(moved_to)
        .and_then(|target| spans.iter().find(|span| span.0 == target))
    {
        pan.scroll = revealing(pan.scroll, *x0, *x1, width, end);
    }
    if active.is_some() {
        pan.revealed = active;
    }
    pan.max = (end + STRIP_EDGE_GUTTER_PX - width).max(0.0);
    pan.viewport = width;
    pan.content = width + pan.max;
    pan.scroll = pan.scroll.clamp(0.0, pan.max);
    pan
}

/// The room the card of a window opened over the shell takes beside the window it opened next to:
/// the card, and the margin it keeps on either side.
#[must_use]
pub fn float_card_room() -> f64 {
    FLOAT_CARD_SIZE.0 + 2.0 * FLOAT_CARD_MARGIN
}
