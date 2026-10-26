//! Who is here, in the bar: the others' faces (a "+N" badge when crowded),
//! the people button, a quiet count of spectators, and the invite button.
//!
//! One who's-here bar. Each face is the person as everyone looking sees
//! them (one initials rule, one colour source) and opens their card. The
//! people button beside the faces opens everyone here (the panel, which
//! can be kept on screen); so do the badge and the spectators' count. The
//! invite button stays beside them however many people are present, and
//! invites in one step: the host's own invite flow, not the list.
use super::{
    Affine, BAR_CONTROL_PX, ChromeIntent, Point, Rect, RoundedRect, SMALL_GLYPH_PX, Scene, Stroke,
    TextRenderer, TextRole, TextWeight, Theme, TopBarChrome, at, paint_bar_selector, t_args,
};

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
        self.collaborator_ids
            .iter()
            .position(|shown| shown == id)
            .and_then(|index| self.face_rects.get(index).copied())
    }

    /// The person whose face is under `point`, by roster id.
    pub(super) fn face_at(&self, point: Point) -> Option<String> {
        // Faces overlap left over right: the later one is on top.
        self.face_rects
            .iter()
            .zip(&self.collaborator_ids)
            .rev()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, id)| id.clone())
    }

    /// What a press at `point` on the people cluster asks for: invite, open
    /// one person, or open everyone.
    pub(super) fn people_intent_at(&self, point: Point) -> Option<ChromeIntent> {
        if self.invite_rect.contains(point) {
            return Some(ChromeIntent::Invite);
        }
        if let Some(id) = self.face_at(point) {
            return Some(ChromeIntent::ShowPerson(id));
        }
        (self.people_button_rect.contains(point)
            || self.avatar_stack_rect.contains(point)
            || self.sharing_indicator_rect.contains(point))
        .then_some(ChromeIntent::ToggleCollaborators)
    }

    /// Where the people button was last drawn; zero when it is not shown.
    #[must_use]
    pub fn people_button_rect(&self) -> Rect {
        self.people_button_rect
    }

    /// The people button, ending at `right_x`: the people glyph in a round
    /// seat, plain at rest and lifting under the pointer like the bar's
    /// other controls. It opens everyone here; the faces beside it each
    /// open one person's card. Returns where the controls before it end.
    fn draw_people_button(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        center_y: f64,
        right_x: f64,
    ) -> f64 {
        let colors = &theme.tokens.colors;
        let seat = BAR_CONTROL_PX;
        let pill = Rect::new(
            right_x - seat,
            center_y - seat / 2.0,
            right_x,
            center_y + seat / 2.0,
        );
        let hovered = self.hovered_control().as_deref() == Some(Self::CONTROL_COLLABORATORS)
            && self.last_cursor.is_some_and(|point| pill.contains(point));
        paint_bar_selector(
            scene,
            transform,
            &RoundedRect::from_rect(pill, seat / 2.0),
            ui::chrome::SelectionState::new(false, hovered, false),
            theme,
        );
        let glyph = SMALL_GLYPH_PX;
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::from_center_size(pill.center(), (glyph, glyph)),
            ui::icons::Icon::Users,
            if hovered {
                colors.text_primary
            } else {
                colors.text_dim
            },
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        self.people_button_rect = pill;
        pill.x0 - f64::from(theme.tokens.spacing.s1)
    }

    /// The invite button, ending at `right_x`: a person-plus glyph in an
    /// outlined empty seat, the seat the next person takes. Its words
    /// ("Invite") are its accessible name. Returns where the controls
    /// before it end.
    pub(super) fn draw_invite_button(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        center_y: f64,
        right_x: f64,
    ) -> f64 {
        if !self.can_invite() {
            self.invite_rect = Rect::ZERO;
            return right_x;
        }
        let colors = &theme.tokens.colors;
        let seat = BAR_CONTROL_PX;
        let pill = Rect::new(
            right_x - seat,
            center_y - seat / 2.0,
            right_x,
            center_y + seat / 2.0,
        );
        let chip = vello::kurbo::Circle::new(pill.center(), seat / 2.0 - 0.5);
        // Plain at rest, lifting under the pointer like every bar control.
        let hovered = self.hovered_control().as_deref() == Some(Self::CONTROL_INVITE);
        paint_bar_selector(
            scene,
            transform,
            &RoundedRect::from_rect(pill, seat / 2.0),
            ui::chrome::SelectionState::new(false, hovered, false),
            theme,
        );
        scene.stroke(
            &Stroke::new(1.0).with_dashes(0.0, [3.0, 3.0]),
            transform,
            ui::surface::edge_on(colors.surfaces.raised),
            None,
            &chip,
        );
        let glyph = SMALL_GLYPH_PX;
        ui::icons::draw_icon(
            scene,
            transform,
            Rect::from_center_size(pill.center(), (glyph, glyph)),
            ui::icons::Icon::UserPlus,
            if hovered {
                colors.text_primary
            } else {
                colors.text_dim
            },
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        self.invite_rect = pill;
        pill.x0 - f64::from(theme.tokens.spacing.s2)
    }

    /// The others' faces in reading order, then the "+N" badge last, the
    /// whole stack ending at `stack_right_x`. Returns how many circles were
    /// drawn (faces and the badge, if any).
    pub(super) fn draw_avatar_chips(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        stack_right_x: f64,
    ) -> usize {
        let colors = &theme.tokens.colors;
        let faces: Vec<ui::presence::Face<'_>> = self
            .collaborator_names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let look = self.face_looks.get(i).copied();
                ui::presence::Face {
                    name: name.as_str(),
                    identity: self.collaborator_ids.get(i).map_or("", String::as_str),
                    colour: self
                        .collaborator_colors
                        .get(i)
                        .copied()
                        .unwrap_or(colors.intent.you),
                    // Software is never drawn as a person: the shared agent
                    // mark in the same ring.
                    is_agent: self.collaborator_agents.get(i).copied().unwrap_or(false),
                    dimmed: look.is_some_and(|look| look.dimmed),
                    in_call: look.and_then(|look| look.call),
                    scale: look.map_or(1.0, |look| look.scale),
                }
            })
            .collect();
        let (rects, badge) = ui::presence::FaceStack::BAR.paint(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, theme),
            (stack_right_x, center_y),
            (&faces, self.collaborator_overflow),
            // The bar's own ground (`draw_overlay_surface`), so the gap
            // between faces and their rings is the bar, not a grey halo.
            colors.surfaces.base,
        );
        self.face_rects = rects;
        faces.len().saturating_add(usize::from(badge.is_some()))
    }

    /// A quiet "+N watching" label for spectators, left of the invite
    /// button, ending at `right_x`. Returns its pointer target (zero when
    /// nobody is watching): the bar's control height, like the faces'.
    pub(super) fn draw_watching_chip(
        &self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        center_y: f64,
        right_x: f64,
    ) -> Rect {
        let watching = self.watching();
        if watching == 0 {
            return Rect::ZERO;
        }
        let tokens = &theme.tokens;
        let colors = &tokens.colors;
        let label = t_args("topbar-watching-count", &[("count", watching.into())]);
        let style = tokens
            .typography
            .style(TextRole::Caption)
            .with_weight(TextWeight::Medium);
        let (label_w, label_h) = text_renderer.measure_style(&label, None, &style);
        let pad = f64::from(tokens.spacing.s2);
        // Plain dim text, no chip: spectators are a count, not people.
        let pill = Rect::new(
            right_x - f64::from(label_w) - pad * 2.0,
            center_y - BAR_CONTROL_PX / 2.0,
            right_x,
            center_y + BAR_CONTROL_PX / 2.0,
        );
        text_renderer.draw_style(
            scene,
            &label,
            colors.text_dim,
            at(
                transform,
                (pill.x0 + pad, center_y - f64::from(label_h) / 2.0),
            ),
            None,
            &style,
        );
        pill
    }

    /// Sets [`Self::avatar_stack_rect`] to the drawn faces' bounds, zero
    /// when nobody else is here. `pos` is `(center_y, stack_right_x)`.
    pub(super) fn set_avatar_stack_bounds(&mut self, pos: (f64, f64), slot_count: usize) {
        let (center_y, stack_right_x) = pos;
        if slot_count == 0 {
            self.avatar_stack_rect = Rect::ZERO;
            return;
        }
        let width = ui::presence::FaceStack::BAR
            .width_of(self.collaborator_initials.len(), self.collaborator_overflow);
        self.avatar_stack_rect = Rect::new(
            stack_right_x - width,
            center_y - BAR_CONTROL_PX / 2.0,
            stack_right_x,
            center_y + BAR_CONTROL_PX / 2.0,
        );
    }

    /// The people cluster, right to left from `right_x`: the invite
    /// button, the spectators' count, the people button, then the faces.
    /// Returns its left edge, where the controls before it end.
    pub(super) fn draw_people_cluster(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        text_renderer: &mut TextRenderer,
        (center_y, right_x): (f64, f64),
    ) -> f64 {
        let gap = f64::from(theme.tokens.spacing.s2);
        let right_x = self.draw_invite_button(scene, transform, theme, center_y, right_x);
        let watching =
            self.draw_watching_chip(scene, transform, theme, text_renderer, center_y, right_x);
        let right_x = if watching.is_zero_area() {
            right_x
        } else {
            watching.x0 - gap
        };
        // Anyone here, or watching, can be shown all together: the button
        // that does it stands beside the faces whatever else is there.
        let anyone = !self.collaborator_initials.is_empty()
            || self.collaborator_overflow > 0
            || !watching.is_zero_area();
        let right_x = if anyone {
            self.draw_people_button(scene, transform, theme, center_y, right_x)
        } else {
            self.people_button_rect = Rect::ZERO;
            right_x
        };
        let slot_count =
            self.draw_avatar_chips(scene, transform, theme, text_renderer, center_y, right_x);
        self.set_avatar_stack_bounds((center_y, right_x), slot_count);
        // The spectators' count is part of the people button: it opens who
        // is here too.
        if !watching.is_zero_area() && !self.people_button_rect.is_zero_area() {
            self.people_button_rect = self.people_button_rect.union(watching);
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
}

#[cfg(test)]
mod tests {
    use super::super::fixture::FixtureChrome;
    use super::*;
    use tilo_desktop_model::{Collaborator, DesktopTree, Workspace, WorkspaceId};

    fn room(people: &[(&str, &str)]) -> DesktopTree {
        let mut workspace = Workspace::new(WorkspaceId(1), "Hacked");
        workspace.collaborators = people
            .iter()
            .map(|(id, name)| Collaborator::new(*id, *name))
            .collect();
        DesktopTree {
            active: workspace.id,
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

    fn press(bar: &mut TopBarChrome, at: Point) -> bool {
        bar.last_cursor = Some(at);
        let mut handled = false;
        bar.handle_left_click(&mut handled);
        handled
    }

    /// The invite button stays beside the faces once people arrive, and a
    /// press on it invites: one step, never the list first.
    #[test]
    fn invite_stays_in_the_bar_and_invites_in_one_step() {
        for people in [&[][..], &[("ada", "Ada Lovelace")][..]] {
            let tree = room(people);
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
        let (bar, _) = bar_over(&room(&[]), false);
        assert!(
            bar.invite_rect().is_zero_area(),
            "nobody is offered an invite the host would refuse"
        );
    }

    /// Spectators are counted beside the invite button, never instead of
    /// it.
    #[test]
    fn spectators_never_replace_the_invite() {
        let tree = room(&[]);
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

    /// With few people here, every face opens a card: the people button
    /// beside them is how the whole panel (and its pin) is reached by
    /// pointer, and the bar names it so to a reader.
    #[test]
    fn the_people_button_opens_everyone_beside_faces_that_open_cards() {
        let tree = room(&[("ada", "Ada Lovelace")]);
        let (mut bar, chrome) = bar_over(&tree, true);
        let button = bar.people_button_rect();
        assert!(!button.is_zero_area(), "shown beside one face");
        let face = bar.face_rect("ada").expect("Ada's face");
        assert!(face.x1 <= button.x0, "faces, then the people button");
        assert!(
            button.x1 <= bar.invite_rect().x0,
            "then the seat the next person takes"
        );
        assert!(press(&mut bar, face.center()));
        assert_eq!(
            chrome.borrow().intents.last(),
            Some(&ChromeIntent::ShowPerson("ada".into()))
        );
        assert!(press(&mut bar, button.center()));
        assert_eq!(
            chrome.borrow().intents.last(),
            Some(&ChromeIntent::ToggleCollaborators)
        );
        assert_eq!(bar.collaborators_rect(), button, "the panel opens from it");
        let node = ui::widget::Widget::accessibility(&bar).expect("bar semantics");
        assert!(
            node.children.iter().any(|child| {
                child.bounds.is_some_and(|b| (b.x0 - button.x0).abs() < 0.5)
                    && child.name.as_deref()
                        == Some(super::super::t("topbar-show-collaborators").as_str())
            }),
            "a reader finds it by its name"
        );

        let (bar, _) = bar_over(&room(&[]), true);
        assert!(
            bar.people_button_rect().is_zero_area(),
            "nobody else here: nothing to show"
        );
    }

    /// A press on someone's face opens who is here at them; the badge
    /// opens everyone.
    #[test]
    fn a_face_opens_that_person() {
        let tree = room(&[("ada", "Ada Lovelace"), ("bo", "Bo Brave")]);
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
        let (mut bar, chrome) = bar_over(&room(&crowd), true);
        assert!(bar.collaborator_overflow() > 0, "crowded: a +N badge");
        let badge_x = bar.face_rects.last().unwrap().x1 + 4.0;
        let stack_y = bar.avatar_stack_rect.center().y;
        assert!(press(&mut bar, Point::new(badge_x + 6.0, stack_y)));
        assert_eq!(
            chrome.borrow().intents.last(),
            Some(&ChromeIntent::ToggleCollaborators)
        );
    }

    /// On a laptop-wide bar a wave still says who waved, in words, beside
    /// the faces: the notice yields to the clock, not to the whole lane
    /// kept for an activity.
    #[test]
    fn a_wave_is_worded_beside_the_faces_on_a_laptop_wide_bar() {
        let tree = room(&[("gd", "Gentle Dolphin")]);
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

    /// A count arriving on the bell, or growing, never moves a face: the
    /// pointer already on its way to someone still lands on them, on the
    /// web and on the desktop alike, from nothing unread on.
    #[test]
    fn the_faces_stay_put_as_unread_notifications_arrive() {
        let tree = room(&[("ada", "Ada Lovelace"), ("bo", "Bo Brave")]);
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
