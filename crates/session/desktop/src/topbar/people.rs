//! Who is here, in the bar: the others' faces (a "+N" badge when crowded),
//! the people button, a quiet count of pages reading along, and the invite
//! button.
//!
//! One who's-here bar. Each face is the person as everyone looking sees
//! them (one initials rule, one colour source) and opens their card. The
//! people button beside the faces opens everyone here (the panel, which
//! can be kept on screen); so do the badge and the watching count. The
//! invite button stays beside them however many people are present, and
//! invites in one step: the host's own invite flow, not the list.
use super::{
    Affine, BAR_CONTROL_PX, ChromeIntent, Point, Rect, Scene, TextRenderer, Theme, TopBarChrome, t,
    t_args,
};

/// The people button's glyph: the cluster of who is here.
static USERS_GLYPH: &[ui::icons::Icon] = &[ui::icons::Icon::Users];
/// The invite seat's glyph: a person-plus in the empty seat.
static INVITE_GLYPH: &[ui::icons::Icon] = &[ui::icons::Icon::UserPlus];

/// The width a bar chip carrying one glyph takes, measured as the people and invite buttons are
/// drawn, so a bar that lays controls out by their width keeps one rule for both.
fn glyph_chip_width(
    name: &str,
    glyph: &[ui::icons::Icon],
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) -> f64 {
    ui::widget::BarChipFace::new(name, &[])
        .icons(glyph)
        .at_height(BAR_CONTROL_PX)
        .width(text_renderer, theme)
}

impl TopBarChrome {
    pub(super) const CONTROL_INVITE: &'static str = "topbar:invite";

    /// Whether the host lets this person invite others here.
    pub(super) fn can_invite(&self) -> bool {
        !self.is_compact()
            && self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.snapshot().invite == crate::chrome::Invite::Offered)
    }

    /// Where the invite button was last drawn; zero when it is not shown.
    #[must_use]
    pub fn invite_rect(&self) -> Rect {
        self.invite_rect
    }

    /// Where the face of the person with roster id `id` was last drawn.
    #[must_use]
    pub fn face_rect(&self, id: &str) -> Option<Rect> {
        let index = self.collaborator_ids.iter().position(|shown| shown == id)?;
        let drawn = self.shown_faces.iter().position(|shown| *shown == index)?;
        self.face_rects.get(drawn).copied()
    }

    /// Whether this bar is a phone's: its middle lane is the time alone (or the folded banner of
    /// following someone), and it has room beside the people button for one face.
    pub(super) fn phone_wide(&self) -> bool {
        !self.is_compact() && crate::follow_banner::folds(self.rect.width())
    }

    /// Whether anyone but this person is here or reading along: the people button then stands.
    fn anyone_else(&self) -> bool {
        !self.collaborator_initials.is_empty() || self.watching() > 0
    }

    /// The person whose face is under `point` , by roster id.
    pub(super) fn face_at(&self, point: Point) -> Option<String> {
        // Faces overlap left over right: the later one is on top.
        self.face_rects
            .iter()
            .zip(&self.shown_faces)
            .rev()
            .find(|(rect, _)| rect.contains(point))
            .and_then(|(_, index)| self.collaborator_ids.get(*index).cloned())
    }

    /// What a press at `point` on the people cluster asks for: invite, open one person, or open
    /// everyone.
    pub(super) fn people_intent_at(&self, point: Point) -> Option<ChromeIntent> {
        if self.invite_rect.contains(point) {
            return Some(ChromeIntent::Invite);
        }
        if let Some(id) = self.face_at(point) {
            return Some(ChromeIntent::ShowPerson(id));
        }
        (self.people_button_rect.contains(point)
            || self.people_count_rect.contains(point)
            || self.avatar_stack_rect.contains(point)
            || self.sharing_indicator_rect.contains(point))
        .then_some(ChromeIntent::ToggleCollaborators)
    }

    /// Where the people button was last drawn; zero when it is not shown.
    #[must_use]
    pub fn people_button_rect(&self) -> Rect {
        self.people_button_rect
    }

    /// The people button's chip: the people glyph in a round seat, plain at rest and lifting
    /// under the pointer like the bar's other controls.
    pub(super) fn people_chip<'a>(
        &self,
        name: &'a str,
        hovered: bool,
    ) -> ui::widget::BarChipFace<'a> {
        ui::widget::BarChipFace::new(name, &[])
            .icons(USERS_GLYPH)
            .at_height(BAR_CONTROL_PX)
            .hovered(hovered)
            .focused(
                self.focus_ring_visible()
                    && self.focused_control.as_deref() == Some(Self::CONTROL_COLLABORATORS),
            )
    }

    /// Where the people button stands, ending at `right_x`. It opens everyone here; the faces
    /// beside it each open one person's card. Returns where the controls before it end.
    fn place_people_button(
        &mut self,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        right_x: f64,
    ) -> f64 {
        use ui::widget::BarChipFace;
        let seat = f64::from(ui::tokens::Metrics::BASE.min_pointer_target);
        let hovered = self.hovered_control().as_deref() == Some(Self::CONTROL_COLLABORATORS)
            && self.last_cursor.is_some_and(|point| {
                Rect::new(
                    right_x - seat,
                    center_y - seat / 2.0,
                    right_x,
                    center_y + seat / 2.0,
                )
                .contains(point)
            });
        let name = t("topbar-show-collaborators");
        let width = self.people_chip(&name, hovered).width(text_renderer, theme);
        let pill = BarChipFace::place_at(right_x - width, center_y, width, BAR_CONTROL_PX);
        self.people_button_rect = pill;
        self.placed.people_button = Some(hovered);
        pill.x0 - f64::from(theme.tokens.spacing.s1)
    }

    /// Whether the invite button has room without pressing into the banner's lane. While this
    /// person follows someone, the bar's middle lane belongs to the banner that says whose view
    /// this is, and the controls beside it keep the bar's own control gap clear of it. A bar too
    /// narrow for the invite, the people button and the banner keeps the banner and who is here:
    /// the invite waits in the panel the people button opens, and comes back when following ends.
    fn invite_clears_the_lane(
        &self,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        right_x: f64,
    ) -> bool {
        if !self.following || self.center_lane.is_zero_area() {
            return true;
        }
        let gap = f64::from(theme.tokens.spacing.s2);
        let seat = f64::from(theme.tokens.spacing.s1);
        let watching = self.watching();
        let watching_w = if watching == 0 {
            0.0
        } else {
            let label = t_args("topbar-watching-count", &[("count", watching.into())]);
            ui::widget::BarChipFace::new(&label, &[])
                .label(&label)
                .at_height(BAR_CONTROL_PX)
                .width(text_renderer, theme)
                + gap
        };
        let invite_w = glyph_chip_width(&t("topbar-invite"), INVITE_GLYPH, text_renderer, theme);
        let people_w = glyph_chip_width(
            &t("topbar-show-collaborators"),
            USERS_GLYPH,
            text_renderer,
            theme,
        );
        // The people button ends where the invite leaves it, and its own gap on either side is
        // the bar's: what stands left of this cluster must still clear the lane by one gap.
        right_x - invite_w - gap - watching_w - people_w - seat >= self.center_lane.x1 + gap
    }

    /// The invite button's chip: a person-plus glyph in the seat the next person takes, lifting
    /// under the pointer like every other bar control. Its words are its accessible name.
    pub(super) fn invite_chip<'a>(&self, name: &'a str) -> ui::widget::BarChipFace<'a> {
        let hovered = self.hovered_control().as_deref() == Some(Self::CONTROL_INVITE);
        ui::widget::BarChipFace::new(name, &[])
            .icons(INVITE_GLYPH)
            .at_height(BAR_CONTROL_PX)
            .hovered(hovered)
            .focused(
                self.focus_ring_visible()
                    && self.focused_control.as_deref() == Some(Self::CONTROL_INVITE),
            )
    }

    /// Where the invite button stands, ending at `right_x`. Returns where the controls before it
    /// end.
    pub(super) fn place_invite_button(
        &mut self,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        right_x: f64,
    ) -> f64 {
        use ui::widget::BarChipFace;
        // A phone-wide bar with someone else here keeps its room for who is here: the face of the
        // one who did something last beside the people button, and the clock. The invite waits in
        // the panel that button opens ("Invite people"), and comes back once nobody else is here.
        if !self.can_invite()
            || !self.invite_clears_the_lane(text_renderer, theme, right_x)
            || (self.phone_wide() && self.anyone_else())
        {
            self.invite_rect = Rect::ZERO;
            return right_x;
        }
        let name = t("topbar-invite");
        let width = self.invite_chip(&name).width(text_renderer, theme);
        let pill = BarChipFace::place_at(right_x - width, center_y, width, BAR_CONTROL_PX);
        self.invite_rect = pill;
        pill.x0 - f64::from(theme.tokens.spacing.s2)
    }

    /// Where the others' faces stand in reading order, then the "+N" badge last, the whole stack
    /// ending at `stack_right_x`. Returns where the stack stands (zero when nobody else is here).
    /// Every face is the shared [`ui::presence::FaceStack`]: an agent wears its client's hue as a
    /// rounded square and a person a circle, at every scale the desktop shows people.
    pub(super) fn place_avatar_chips(
        &mut self,
        theme: &Theme,
        center_y: f64,
        stack_right_x: f64,
    ) -> Rect {
        if self.phone_wide() {
            return self.place_phone_face(theme, center_y, stack_right_x);
        }
        let stack = ui::presence::FaceStack::BAR;
        let gap = f64::from(theme.tokens.spacing.s2);
        let pill_right = self.ws_pill_rect.x1.max(self.rect.x0);
        // The faces keep off the clock's lane, standing between it and the people cluster where
        // that is room enough. What does not fit joins the "+N" badge, and with no room for even
        // the badge the people button beside them is who is here. A phone-wide bar has no room
        // right of the clock and the gap before it where they fit: they stand there rather than
        // folding everyone out of the bar, so the agent this person follows is still on it.
        // What the middle of the bar keeps for itself: the banner's lane while this view follows
        // someone, else the clock's own drawing. The lane is the clock's seat with the room its
        // hands may sweep through; a phone's bar is a couple of pixels short of a face beside
        // that seat, and the clock's ink is not, so the faces stand clear of the drawing rather
        // than of the seat.
        let middle = if self.center_rect.is_zero_area() {
            self.center_lane
        } else {
            Rect::new(
                self.center_rect.x0,
                self.center_lane.y0,
                self.center_rect.x1,
                self.center_lane.y1,
            )
        };
        let (anchor, room) = if middle.is_zero_area() {
            (stack_right_x, stack_right_x - pill_right)
        } else {
            let right_of_middle = stack_right_x - middle.x1 - gap;
            if right_of_middle >= stack.width_of(1, 0) {
                (stack_right_x, right_of_middle)
            } else {
                let before_middle = (middle.x0 - gap).max(pill_right);
                (before_middle, (before_middle - pill_right).max(0.0))
            }
        };
        let (most, past) = stack.split(self.collaborator_names.len());
        let mut shown: Vec<usize> = (0..most).collect();
        let mut more = past;
        while !shown.is_empty() && stack.width_of(shown.len(), more) > room {
            shown.pop();
            more = more.saturating_add(1);
        }
        if stack.width_of(0, more) > room {
            more = 0;
        }
        let faces: Vec<ui::presence::Face<'_>> =
            shown.iter().map(|&i| self.face_of(i, theme)).collect();
        let (rects, badge) = stack.layout((anchor, center_y), (&faces, more));
        drop(faces);
        let count = shown.len();
        self.faces_shown = count;
        self.shown_faces.clone_from(&shown);
        self.collaborator_overflow = more;
        self.face_rects = Self::faces_drawn(rects);
        self.placed.faces = Some(super::placed::FacesPlaced {
            shown,
            more,
            anchor: (anchor, center_y),
        });
        if count == 0 && badge.is_none() {
            return Rect::ZERO;
        }
        let width = stack.width_of(count, more);
        Rect::new(
            anchor - width,
            center_y - BAR_CONTROL_PX / 2.0,
            anchor,
            center_y + BAR_CONTROL_PX / 2.0,
        )
    }

    /// Paint the faces where [`Self::place_avatar_chips`] stood them, over the bar's own ground
    /// (`draw_overlay_surface`), so the gap between faces and their rings is the bar, not a grey
    /// halo.
    pub(super) fn paint_avatar_chips(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        let Some(placed) = self.placed.faces.as_ref() else {
            return;
        };
        let faces: Vec<ui::presence::Face<'_>> = placed
            .shown
            .iter()
            .map(|&i| self.face_of(i, theme))
            .collect();
        let _ = ui::presence::FaceStack::BAR.paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            placed.anchor,
            (&faces, placed.more),
            theme.tokens.colors.surfaces.base,
        );
    }

    /// The face of the person at roster index `i` , as everyone looking sees them: an agent wears
    /// its client's hue as a rounded square and a person a circle.
    fn face_of(&self, i: usize, theme: &Theme) -> ui::presence::Face<'_> {
        let look = self.face_looks.get(i).copied();
        ui::presence::Face {
            name: self.collaborator_names.get(i).map_or("", String::as_str),
            identity: self.collaborator_ids.get(i).map_or("", String::as_str),
            colour: self
                .collaborator_colors
                .get(i)
                .copied()
                .unwrap_or(theme.tokens.colors.intent.you),
            // Software is never drawn as a person: the shared agent mark in the same ring.
            is_agent: self.collaborator_agents.get(i).copied().unwrap_or(false),
            dimmed: look.is_some_and(|look| look.dimmed),
            in_call: look.and_then(|look| look.call),
            scale: look.map_or(1.0, |look| look.scale),
        }
    }

    /// What pressing a face stands for, by the roster id it opens: the bar's own target, so a
    /// reader presses a face where the pointer does.
    pub(super) const FACE_TARGET: &'static str = "topbar:face:";

    /// The faces drawn, each where a press reaches it: the disc the stack painted, with the ring
    /// and the ground drawn around it. Where a face stands is decided once, here, for the pointer
    /// and for what a reader is told alike.
    pub(super) fn faces_drawn(rects: Vec<Rect>) -> Vec<Rect> {
        rects
            .into_iter()
            .map(|rect| rect.inflate(ui::presence::Face::REACH, ui::presence::Face::REACH))
            .collect()
    }

    /// Where a phone-wide bar's one face stands, ending at `right_x`: the person who did
    /// something last, agents included, so someone who arrives, comes back or starts working is on
    /// the bar the moment they do. Everyone else here is the stack's own "+N" badge beside that
    /// face. The one this view follows is not drawn twice: the banner's face is theirs. The stack
    /// keeps the bar's gap from the banner's lane and from the workspace's name; the clock yields
    /// to it (`center_yield`). With no room for the face it keeps the badge alone, counting
    /// everyone, and with no room for that the people button stands for who is here.
    fn place_phone_face(&mut self, theme: &Theme, center_y: f64, right_x: f64) -> Rect {
        let stack = ui::presence::FaceStack::BAR;
        let gap = f64::from(theme.tokens.spacing.s2);
        let mut left = self.ws_pill_rect.x1.max(self.rect.x0) + gap;
        if self.following && !self.center_lane.is_zero_area() {
            left = left.max(self.center_lane.x1 + gap);
        }
        let others = self
            .collaborator_ids
            .len()
            .saturating_sub(usize::from(self.following));
        let room = right_x - left;
        let latest: Vec<usize> = self.latest_active.into_iter().collect();
        let past_latest = others.saturating_sub(latest.len());
        let (shown, more) = if stack.width_of(latest.len(), past_latest) <= room {
            (latest, past_latest)
        } else if others > 0 && stack.width_of(0, others) <= room {
            (Vec::new(), others)
        } else {
            (Vec::new(), 0)
        };
        self.faces_shown = shown.len();
        self.collaborator_overflow = if shown.is_empty() && more == 0 {
            others
        } else {
            more
        };
        let faces: Vec<ui::presence::Face<'_>> =
            shown.iter().map(|&i| self.face_of(i, theme)).collect();
        let (rects, _) = stack.layout((right_x, center_y), (&faces, more));
        drop(faces);
        self.face_rects = Self::faces_drawn(rects);
        let width = stack.width_of(shown.len(), more);
        self.shown_faces.clone_from(&shown);
        self.placed.faces = Some(super::placed::FacesPlaced {
            shown,
            more,
            anchor: (right_x, center_y),
        });
        if width <= 0.0 {
            return Rect::ZERO;
        }
        Rect::new(
            right_x - width,
            center_y - BAR_CONTROL_PX / 2.0,
            right_x,
            center_y + BAR_CONTROL_PX / 2.0,
        )
    }

    /// Where the "+N" the people button wears stands, where the button stands for who is here (no
    /// room for a face or the stack's own badge): a small neutral count on its corner, which takes
    /// none of the bar's width, so nobody here goes uncounted however narrow the bar. Its words
    /// are the badge's own node.
    fn place_people_count(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        self.people_count_rect = Rect::ZERO;
        self.placed.people_count = None;
        let button = self.people_button_rect;
        if self.collaborator_overflow == 0 || button.is_zero_area() {
            return;
        }
        let label = format!("+{}", self.collaborator_overflow.min(99));
        let height = ui::widget::BadgeFace::height(theme);
        let width =
            ui::widget::BadgeFace::intrinsic_width(&label, text_renderer, theme).max(height);
        // On the button's top right corner, inside the bar's band and clear of the control after it.
        let centre = Point::new(
            button.x1 - 2.0,
            (button.y0 + 2.0).max(self.rect.y0 + height / 2.0 + 1.0),
        );
        self.people_count_rect = Rect::from_center_size(centre, (width, height));
        self.placed.people_count = Some(label);
    }

    /// The watching count's chip showing `label`: a bar chip with its words and no glyph, so it
    /// stands and lifts like every other control while it opens who is here too.
    pub(super) fn watching_chip<'a>(
        &self,
        label: &'a str,
        right_x: f64,
    ) -> ui::widget::BarChipFace<'a> {
        let hovered = self.hovered_control().as_deref() == Some(Self::CONTROL_COLLABORATORS)
            && self.last_cursor.is_some_and(|point| point.x <= right_x);
        ui::widget::BarChipFace::new(label, &[])
            .label(label)
            .at_height(BAR_CONTROL_PX)
            .hovered(hovered)
            .focused(
                self.focus_ring_visible()
                    && self.focused_control.as_deref() == Some(Self::CONTROL_COLLABORATORS),
            )
    }

    /// Where a quiet "+N watching" label for pages reading this workspace stands, left of the
    /// invite button, ending at `right_x`. Returns its pointer target (zero when nobody is
    /// watching): the bar's control height, like the faces'. Pages are a count, not faces.
    pub(super) fn place_watching_chip(
        &mut self,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        right_x: f64,
    ) -> Rect {
        self.placed.watching = None;
        let watching = self.watching();
        if watching == 0 {
            return Rect::ZERO;
        }
        let label = t_args("topbar-watching-count", &[("count", watching.into())]);
        let width = self
            .watching_chip(&label, right_x)
            .width(text_renderer, theme);
        let pill =
            ui::widget::BarChipFace::place_at(right_x - width, center_y, width, BAR_CONTROL_PX);
        self.placed.watching = Some((label, pill));
        pill
    }

    /// Where the people cluster stands, right to left from `right_x`: the invite button, the
    /// watching count, then the others' faces. The faces open who is here themselves, so the
    /// people button stands only where someone is here and not even the "+N" badge has room.
    /// Returns its left edge, where the controls before it end.
    pub(super) fn place_people_cluster(
        &mut self,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        (center_y, right_x): (f64, f64),
    ) -> f64 {
        let gap = f64::from(theme.tokens.spacing.s2);
        let right_x = self.place_invite_button(theme, text_renderer, center_y, right_x);
        let watching = self.place_watching_chip(theme, text_renderer, center_y, right_x);
        let right_x = if watching.is_zero_area() {
            right_x
        } else {
            watching.x0 - gap
        };
        self.people_button_rect = Rect::ZERO;
        self.placed.people_button = None;
        self.placed.faces = None;
        self.avatar_stack_rect = self.place_avatar_chips(theme, center_y, right_x);
        // Someone is here but the bar had no room for their face or the count: the people button
        // is who is here then.
        if self.avatar_stack_rect.is_zero_area() && !self.collaborator_ids.is_empty() {
            self.collaborator_overflow = self
                .collaborator_ids
                .len()
                .saturating_sub(usize::from(self.following && self.phone_wide()));
            self.place_people_button(theme, text_renderer, center_y, right_x);
        }
        self.place_people_count(theme, text_renderer);
        // The watching count opens who is here too.
        if !watching.is_zero_area() {
            self.people_button_rect = if self.people_button_rect.is_zero_area() {
                watching
            } else {
                self.people_button_rect.union(watching)
            };
        }
        [
            self.avatar_stack_rect,
            self.people_button_rect,
            self.invite_rect,
        ]
        .into_iter()
        .filter(|rect| !rect.is_zero_area())
        .map(|rect| rect.x0)
        .fold(right_x, f64::min)
    }

    /// Paint the people cluster where [`Self::place_people_cluster`] stood it.
    pub(super) fn paint_people_cluster(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
    ) {
        let ground = theme.tokens.colors.surfaces.base;
        if !self.invite_rect.is_zero_area() {
            let name = t("topbar-invite");
            let _ = self.invite_chip(&name).paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                self.invite_rect,
                ground,
            );
        }
        if let Some((label, pill)) = self.placed.watching.as_ref() {
            let _ = self.watching_chip(label, pill.x1).paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                *pill,
                ground,
            );
        }
        self.paint_avatar_chips(scene, transform, theme, text_renderer);
        if let Some(hovered) = self.placed.people_button {
            // The button's own rect, before the watching count joined its target.
            let width = self.people_button_rect.width();
            let name = t("topbar-show-collaborators");
            let chip = self.people_chip(&name, hovered);
            let own = chip.width(text_renderer, theme);
            let rect = if self.placed.watching.is_some() && own < width {
                Rect::new(
                    self.people_button_rect.x0,
                    self.people_button_rect.y0,
                    self.people_button_rect.x0 + own,
                    self.people_button_rect.y1,
                )
            } else {
                self.people_button_rect
            };
            let _ = chip.paint(
                &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
                ground,
            );
        }
        if let Some(label) = self.placed.people_count.as_ref() {
            ui::widget::BadgeFace::new(self.people_count_rect, label)
                .style(ui::widget::ChipStyle::Neutral)
                .paint(scene, transform, text_renderer, theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixture::FixtureChrome;
    use super::*;
    use tilo_desktop_model::{Collaborator, DesktopTree, Workspace, WorkspaceSlot};

    fn desk(people: &[(&str, &str)]) -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceSlot(1), "Hacked");
        workspace.collaborators = people
            .iter()
            .map(|(id, name)| Collaborator::new(*id, *name))
            .collect();
        DesktopTree {
            active: workspace.slot,
            workspaces: vec![workspace],
            ..DesktopTree::default()
        }
    }

    fn bar_over(
        tree: &DesktopTree,
        can_invite: bool,
    ) -> (TopBarChrome, std::rc::Rc<FixtureChrome>) {
        let chrome = FixtureChrome::new(tree.clone());
        chrome.borrow_mut().can_invite = can_invite;
        let mut bar = TopBarChrome::from_tree(tree).with_chrome(chrome.clone());
        bar.sync_from_tree(tree);
        super::super::tests::lay_out_and_draw(&mut bar, 1600.0);
        (bar, chrome)
    }

    /// How far apart two rects stand: zero when they touch or overlap, else the straight
    /// distance between their nearest edges.
    fn apart(a: Rect, b: Rect) -> f64 {
        let dx = (a.x0 - b.x1).max(b.x0 - a.x1).max(0.0);
        let dy = (a.y0 - b.y1).max(b.y0 - a.y1).max(0.0);
        dx.hypot(dy)
    }

    fn press(bar: &mut TopBarChrome, at: Point) -> bool {
        bar.last_cursor = Some(at);
        let mut handled = false;
        bar.handle_left_click(&mut handled);
        handled
    }

    /// The invite button stays beside the faces once people arrive, and a press on it invites: one
    /// step, never the list first.
    #[test]
    fn invite_stays_in_the_bar_and_invites_in_one_step() {
        for people in [&[][..], &[("ada", "Ada Lovelace")][..]] {
            let tree = desk(people);
            let (mut bar, chrome) = bar_over(&tree, true);
            let invite = bar.invite_rect();
            assert!(!invite.is_zero_area(), "with {} here", people.len());
            if !people.is_empty() {
                assert!(
                    bar.avatar_stack_rect.x1 <= invite.x0,
                    "the faces, then the seat the next person takes"
                );
            }
            assert!(press(&mut bar, invite.center()));
            assert_eq!(chrome.borrow().intents.last(), Some(&ChromeIntent::Invite));
            assert!(
                !chrome.borrow().overlays.collaborators,
                "inviting does not open the list"
            );
        }
        let (bar, _) = bar_over(&desk(&[]), false);
        assert!(
            bar.invite_rect().is_zero_area(),
            "nobody is offered an invite the host would refuse"
        );
    }

    /// Pages reading along are counted beside the invite button, never instead of it.
    #[test]
    fn watching_never_replaces_the_invite() {
        let tree = desk(&[]);
        let chrome = FixtureChrome::new(tree.clone());
        chrome.borrow_mut().can_invite = true;
        chrome.borrow_mut().watching = 1;
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
        super::super::tests::lay_out_and_draw(&mut bar, 1600.0);
        assert!(!bar.invite_rect().is_zero_area());
        assert!(
            !bar.people_button_rect().is_zero_area(),
            "the count opens who is here"
        );
        assert!(bar.people_button_rect().x1 <= bar.invite_rect().x0);
    }

    /// The faces are who is here: each opens the panel at its person, so no second people
    /// button stands beside them, and the panel opens from the faces.
    #[test]
    fn the_faces_open_who_is_here_without_a_second_button() {
        let tree = desk(&[("ada", "Ada Lovelace")]);
        let (mut bar, chrome) = bar_over(&tree, true);
        assert!(
            bar.people_button_rect().is_zero_area(),
            "no people button beside a face"
        );
        let face = bar.face_rect("ada").expect("Ada's face");
        assert!(
            face.x1 <= bar.invite_rect().x0,
            "the faces, then the seat the next person takes"
        );
        assert!(press(&mut bar, face.center()));
        assert_eq!(
            chrome.borrow().intents.last(),
            Some(&ChromeIntent::ShowPerson("ada".into()))
        );
        assert_eq!(
            bar.collaborators_rect(),
            bar.avatar_stack_rect,
            "the panel opens from the faces"
        );

        let (bar, _) = bar_over(&desk(&[]), true);
        assert!(
            bar.people_button_rect().is_zero_area() && bar.avatar_stack_rect.is_zero_area(),
            "nobody else here: only the invite"
        );
    }

    /// This person appears once on their bar: their avatar at the far right, never also among
    /// the faces, however the host lists them; and it is drawn as the faces are, at their size.
    #[test]
    fn this_person_appears_once_drawn_as_everyone_is() {
        let tree = desk(&[("ada", "Ada Lovelace"), ("me-root", "Jo Doe")]);
        let chrome = FixtureChrome::new(tree.clone());
        chrome.borrow_mut().own = Some(crate::chrome::OwnChip {
            name: "Jo Doe".into(),
            rgb: (247, 133, 125),
            identity: "me-root".into(),
        });
        let mut bar = TopBarChrome::from_tree(&tree).with_chrome(chrome);
        bar.sync_from_tree(&tree);
        super::super::tests::lay_out_and_draw(&mut bar, 1600.0);
        assert!(bar.face_rect("me-root").is_none(), "not among the others");
        assert_eq!(bar.collaborator_ids, vec!["ada".to_owned()]);
        let own = bar.own_avatar_rect();
        assert!(!own.is_zero_area(), "their own avatar stands");
        let ada = bar.face_rect("ada").expect("Ada's face");
        assert!(
            (own.width() - ada.width()).abs() < 0.01 && (own.height() - ada.height()).abs() < 0.01,
            "one size for every face: {own:?} and {ada:?}"
        );
        assert!(
            bar.right_side_x() < own.x0 && own.x1 <= bar.tray_pill_rect().x0,
            "their avatar stands right of who is here, before the notifications"
        );
    }

    /// Each face is one place: the rect a reader is told is the rect a press reaches, the middle
    /// of which opens that person, and it is a control of its own — so a reader presses the face
    /// where a person does, never the bar behind it.
    #[test]
    fn a_face_is_published_where_a_press_reaches_it() {
        let tree = desk(&[("ada", "Ada Lovelace"), ("bo", "Bo Brave")]);
        let (bar, _chrome) = bar_over(&tree, true);
        let faces = bar.face_accessibility();
        assert_eq!(faces.len(), 2, "one node per face");
        for (face, id) in faces.iter().zip(["ada", "bo"]) {
            let bounds = face.bounds.expect("placed");
            let rect = Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1);
            assert_eq!(
                bar.face_rect(id),
                Some(rect),
                "the bar publishes a face where it draws and hit-tests it"
            );
            assert_eq!(
                bar.face_at(rect.center()).as_deref(),
                Some(id),
                "a press at {id}'s middle opens {id}"
            );
            assert_eq!(
                face.action_target.as_deref(),
                Some(format!("{}{id}", TopBarChrome::FACE_TARGET).as_str()),
                "the face carries its own press"
            );
        }
    }

    /// A press on someone's face opens who is here at them; the badge opens everyone.
    #[test]
    fn a_face_opens_that_person() {
        let tree = desk(&[("ada", "Ada Lovelace"), ("bo", "Bo Brave")]);
        let (mut bar, chrome) = bar_over(&tree, true);
        let bo = bar.face_rects[1];
        assert!(press(&mut bar, bo.center()));
        assert_eq!(
            chrome.borrow().intents.last(),
            Some(&ChromeIntent::ShowPerson("bo".into()))
        );

        let crowd: Vec<(String, String)> = (0..6)
            .map(|i| (format!("p{i}"), format!("Person {i}")))
            .collect();
        let crowd: Vec<(&str, &str)> = crowd
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let (mut bar, chrome) = bar_over(&desk(&crowd), true);
        assert!(bar.collaborator_overflow() > 0, "crowded: a +N badge");
        let badge_x = bar.face_rects.last().unwrap().x1 + 4.0;
        let stack_y = bar.avatar_stack_rect.center().y;
        assert!(press(&mut bar, Point::new(badge_x + 6.0, stack_y)));
        assert_eq!(
            chrome.borrow().intents.last(),
            Some(&ChromeIntent::ToggleCollaborators)
        );
    }

    /// On a laptop-wide bar a wave still says who waved, in words, beside the faces: the notice
    /// yields to the clock, not to the whole lane kept for an activity.
    #[test]
    fn a_wave_is_worded_beside_the_faces_on_a_laptop_wide_bar() {
        let tree = desk(&[("gd", "Gentle Dolphin")]);
        let (mut bar, chrome) = bar_over(&tree, true);
        super::super::tests::lay_out_and_draw(&mut bar, 1024.0);
        let quiet = bar.people_left_x;
        chrome.borrow_mut().waved = Some(crate::chrome::WaveNotice {
            from: "Gentle Dolphin".into(),
            at: ui::time::now(),
        });
        super::super::tests::lay_out_and_draw(&mut bar, 1024.0);
        let theme = Theme::default();
        let mut text = TextRenderer::new();
        let least = ui::widget::ChipFace::intrinsic_width(
            "Gentle D\u{2026} waved at you",
            ui::widget::ChipLead::Glyph(ui::icons::Icon::Hand),
            false,
            &mut text,
            &theme,
        );
        assert!(
            quiet - bar.people_left_x >= least,
            "the notice takes room for its words: {quiet} -> {}",
            bar.people_left_x
        );
        assert!(
            bar.people_left_x > bar.center_rect.x1,
            "and keeps off the clock"
        );
    }

    /// A bar as the hosted page stands it on a phone: the Tilo mark, the workspace's name, the
    /// person's own avatar and the bell, so the room left for who is here is the page's.
    fn phone_bar(tree: &DesktopTree, width: f32) -> (TopBarChrome, std::rc::Rc<FixtureChrome>) {
        let chrome = FixtureChrome::new(tree.clone());
        chrome.borrow_mut().can_invite = true;
        chrome.borrow_mut().own = Some(crate::chrome::OwnChip {
            name: "Calm Fox".into(),
            rgb: (110, 140, 230),
            identity: String::new(),
        });
        let mut bar = TopBarChrome::from_tree(tree)
            .with_chrome(chrome.clone())
            .with_tilo_mark();
        bar.sync_from_tree(tree);
        super::super::tests::lay_out_and_draw(&mut bar, width);
        (bar, chrome)
    }

    fn agent_and_person() -> DesktopTree {
        let mut tree = desk(&[("ada", "Ada Lovelace"), ("cc", "Claude Code")]);
        tree.workspaces[0].collaborators[1].role_id = Some("agent".into());
        tree
    }

    /// What the bar tells a reader its faces are, in the order drawn. A face is a control of its
    /// own (`FACE_TARGET`), so the "+N" counted beside them is not one of them.
    fn face_names(bar: &TopBarChrome) -> Vec<String> {
        bar.face_accessibility()
            .into_iter()
            .filter(|node| {
                node.action_target
                    .as_deref()
                    .is_some_and(|target| target.starts_with(TopBarChrome::FACE_TARGET))
            })
            .filter_map(|node| node.name)
            .collect()
    }

    /// The phone's bar has room for one face, and it is whoever did something last: an agent
    /// that starts working (or comes back) is on the bar at once, as its client's square face,
    /// and everyone else here is the stack's "+N" beside it. The invite waits in the panel, and
    /// nothing stands on the clock or the workspace's name.
    ///
    /// The bar reads the workspace at one monotonic instant throughout: who did something last is
    /// the order the changes reached the bar, so no amount of clock the host does or does not
    /// advance can decide it.
    #[test]
    fn a_phone_bar_shows_whoever_did_something_last() {
        ui::time::testing::with_monotonic_time(ui::time::now(), phone_bar_at_one_instant);
    }

    fn phone_bar_at_one_instant() {
        let theme = Theme::default();
        let gap = f64::from(theme.tokens.spacing.s2);
        for width in [390.0f32, 360.0] {
            let mut tree = agent_and_person();
            let (mut bar, _chrome) = phone_bar(&tree, width);
            assert_eq!(bar.faces_shown, 1, "one face at {width}");
            assert_eq!(
                bar.collaborator_overflow(),
                1,
                "and the other counted at {width}"
            );
            // The agent starts writing: its presence says so, and its face takes the bar.
            tree.workspaces[0].collaborators[1].status = Some(tilo_desktop_model::Status {
                text: "Editing Launch notes in Notes".into(),
                automatic: true,
            });
            bar.sync_from_tree(&tree);
            super::super::tests::lay_out_and_draw(&mut bar, width);
            assert_eq!(
                face_names(&bar),
                vec![t_args(
                    "topbar-face-agent",
                    &[("name", "Claude Code".into())]
                )],
                "the agent that just did something is the face at {width}"
            );
            let face = bar.face_rect("cc").expect("the agent's face");
            assert!(
                bar.people_button_rect().is_zero_area(),
                "no people button beside the face at {width}"
            );
            assert_eq!(bar.collaborator_overflow(), 1, "Ada is counted at {width}");
            let stack = bar.avatar_stack_rect;
            assert!(
                stack.x1 > face.x1 + 1.0,
                "the +1 stands in the stack beside the face at {width}: {stack:?} {face:?}"
            );
            assert!(
                stack.x1 < bar.own_avatar_rect.x0,
                "and clear of the avatar after it at {width}: {stack:?}"
            );
            assert!(
                bar.invite_rect().is_zero_area(),
                "the invite waits in the panel at {width}"
            );
            assert!(
                face.x0 >= bar.ws_pill_rect.x1 + gap,
                "the face keeps off the workspace's name at {width}: {face:?} {:?}",
                bar.ws_pill_rect
            );
            if !bar.center_rect.is_zero_area() {
                assert!(
                    bar.center_rect.x1 + gap <= face.x0,
                    "the clock yields to the face at {width}: {:?} {face:?}",
                    bar.center_rect
                );
            }
            // A press on it opens the agent; on the count, everyone.
            assert_eq!(
                bar.people_intent_at(face.center()),
                Some(ChromeIntent::ShowPerson("cc".into()))
            );
            assert_eq!(
                bar.people_intent_at(Point::new(stack.x1 - 4.0, stack.center().y)),
                Some(ChromeIntent::ToggleCollaborators)
            );

            // Alone, an agent that comes back is the one face, with no count.
            let mut alone = desk(&[("cc", "Claude Code")]);
            alone.workspaces[0].collaborators[0].role_id = Some("agent".into());
            let (bar, _chrome) = phone_bar(&alone, width);
            assert!(
                bar.face_rect("cc").is_some(),
                "the agent is on the bar at {width}"
            );
            assert_eq!(bar.collaborator_overflow(), 0);
            assert!(bar.people_count_rect.is_zero_area(), "no count at {width}");
        }
    }

    /// Nobody else here, the phone's bar keeps the invite: it yields only to people.
    #[test]
    fn an_empty_phone_bar_keeps_the_invite() {
        let (bar, _chrome) = phone_bar(&desk(&[]), 390.0);
        assert!(!bar.invite_rect().is_zero_area());
        assert!(bar.people_button_rect().is_zero_area());
    }

    /// Following someone on a phone, the banner's face is theirs: the bar never draws it again,
    /// and everyone else here is a face or counted.
    #[test]
    fn a_phone_bar_never_draws_the_followed_face_twice() {
        let mut tree = agent_and_person();
        tree.workspaces[0].collaborators[1].following = true;
        for width in [390.0, 360.0] {
            let (bar, _chrome) = phone_bar(&tree, width);
            assert!(
                bar.face_rect("cc").is_none(),
                "the banner stands for the agent at {width}"
            );
            assert_eq!(
                bar.faces_shown + bar.collaborator_overflow(),
                1,
                "Ada is a face or counted at {width}"
            );
        }
    }

    /// Following someone never costs the bar the people beside them: while the banner that says
    /// whose view this is stands in the middle lane, the people button, the workspace's name and
    /// the faces keep their room either side of it, and Stop stands in the banner at every width.
    #[test]
    fn following_keeps_the_people_beside_it_on_a_phone() {
        let mut tree = desk(&[("ada", "Ada Lovelace"), ("cc", "Claude Code")]);
        tree.workspaces[0].collaborators[1].role_id = Some("agent".into());
        tree.workspaces[0].collaborators[1].following = true;
        let theme = Theme::default();
        for width in [390.0f32, 360.0] {
            let (bar, _chrome) = phone_bar(&tree, width);
            assert!(bar.following, "the bar knows this view is not its own");
            let lane = crate::follow_banner::lane_width(f64::from(width));
            assert!(
                (bar.center_lane.width() - lane).abs() < 0.5,
                "the middle lane is the banner's, where the clock stood: {:?} at {width}",
                bar.center_lane
            );
            assert!(
                bar.center_rect.is_zero_area(),
                "and the clock stands aside at {width}"
            );
            // The banner as it stands on this bar: their face and Stop in the lane.
            let bar_rect = bar.bar_rect();
            let mut text = TextRenderer::new();
            let card = crate::follow_banner::rects(
                &mut text,
                &theme,
                (
                    &t_args("follow-label", &[("person", "Claude Code".into())]),
                    &t("follow-stop"),
                ),
                crate::follow_banner::BannerFrame {
                    work_area: Rect::new(0.0, bar_rect.y1, f64::from(width), bar_rect.y1 + 760.0),
                    bar: bar_rect,
                    cursor: None,
                    motion: theme.motion(),
                },
                true,
            );
            let (chip, stop) = (card.chip, card.stop);
            assert!(!chip.is_zero_area(), "the banner stands at {width}");
            assert!(
                chip.width() >= f64::from(theme.tokens.metrics.avatar_inline),
                "with their face at {width}: {chip:?}"
            );
            assert!(!stop.is_zero_area(), "and Stop at {width}: {chip:?}");
            let people = bar.control_rect(TopBarChrome::CONTROL_COLLABORATORS);
            assert!(!people.is_zero_area(), "who is here stands at {width}");
            let mut beside: Vec<(&str, Rect)> = vec![
                ("the workspace's name", bar.ws_pill_rect),
                ("who is here", people),
                ("the count on the people button", bar.people_count_rect),
                ("the invite button", bar.invite_rect),
                ("the notifications", bar.tray_hit_rect),
            ];
            beside.extend(bar.face_rects.iter().map(|rect| ("a face", *rect)));
            // The gap the bar keeps between its own controls: a neighbour the banner merely
            // misses reads as one control with it, so the banner keeps this, not zero.
            let gap = f64::from(theme.tokens.spacing.s2);
            for (what, rect) in beside.into_iter().filter(|(_, rect)| !rect.is_zero_area()) {
                assert!(
                    chip.intersect(rect).is_zero_area(),
                    "the banner covers no {what} at {width}: {chip:?} over {rect:?}"
                );
                assert!(
                    apart(chip, rect) >= gap,
                    "the banner keeps the bar's gap from {what} at {width}: {chip:?} beside {rect:?}"
                );
                assert!(
                    stop.intersect(rect).is_zero_area(),
                    "nor Stop over {what} at {width}: {stop:?} over {rect:?}"
                );
                assert!(
                    apart(stop, rect) >= gap,
                    "and Stop keeps it from {what} at {width}: {stop:?} beside {rect:?}"
                );
            }
            // Whatever the bar can hold, nothing of who is here stands in the banner's lane...
            assert!(
                people.x0 >= bar.center_lane.x1 + gap,
                "the people button keeps clear of the banner at {width}: {people:?}"
            );
            // ...and where it cannot hold the invite too, the invite waits in the panel the
            // people button opens, rather than pressing the button into the banner.
            if !bar.invite_rect.is_zero_area() {
                assert!(
                    apart(bar.invite_rect, people) >= gap,
                    "the invite keeps its seat at {width}: {:?} beside {people:?}",
                    bar.invite_rect
                );
            }
            assert!(
                bar.ws_pill_rect.x1 <= bar.center_lane.x0,
                "the workspace keeps its name and chevron at {width}: {:?}",
                bar.ws_pill_rect
            );
            // Everyone here is a face, counted, or the one the banner stands for: following never
            // hides anyone.
            assert_eq!(
                bar.faces_shown + bar.collaborator_overflow() + 1,
                2,
                "both people are accounted for at {width}"
            );
        }
    }

    /// Where the bar stands only some of the faces, the one this person follows is always among
    /// them, and everyone else is counted.
    #[test]
    fn a_crowded_bar_keeps_the_followed_face() {
        let crowd: Vec<(String, String)> = (0..6)
            .map(|i| (format!("p{i}"), format!("Person {i}")))
            .collect();
        let mut tree = desk(
            &crowd
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str()))
                .collect::<Vec<_>>(),
        );
        tree.workspaces[0].collaborators[4].following = true;
        // Narrower, the bar is a phone's, where the banner's face is the followed one's
        // (`a_phone_bar_never_draws_the_followed_face_twice`).
        for width in [720.0, 900.0, 1024.0] {
            let (mut bar, _chrome) = bar_over(&tree, true);
            super::super::tests::lay_out_and_draw(&mut bar, width);
            assert!(bar.faces_shown > 0, "some faces stand at {width}");
            assert!(
                bar.face_rect("p4").is_some(),
                "the followed one is among them at {width}"
            );
            assert_eq!(
                bar.faces_shown + bar.collaborator_overflow(),
                6,
                "and the badge counts the rest at {width}"
            );
        }
    }

    /// A count arriving on the bell, or growing, never moves a face: the pointer already on its way
    /// to someone still lands on them, on the web and on the desktop alike, from nothing unread on.
    #[test]
    fn the_faces_stay_put_as_unread_notifications_arrive() {
        let tree = desk(&[("ada", "Ada Lovelace"), ("bo", "Bo Brave")]);
        for web in [true, false] {
            let (mut bar, chrome) = bar_over(&tree, true);
            chrome.borrow_mut().notifications_only = web;
            super::super::tests::lay_out_and_draw(&mut bar, 1024.0);
            let resting = bar.face_rects.clone();
            assert_eq!(resting.len(), 2, "both faces stand");
            for unread in [1, 7, 42, 100, 5000, 0] {
                chrome.borrow_mut().notification_badge = unread;
                super::super::tests::lay_out_and_draw(&mut bar, 1024.0);
                assert_eq!(
                    bar.face_rects, resting,
                    "with {unread} unread (the web: {web})"
                );
            }
        }
    }
}
