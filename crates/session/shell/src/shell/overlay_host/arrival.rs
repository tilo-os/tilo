//! Arriving and being waved at.
//!
//! Entering a workspace with people in it (joining it, or switching to it)
//! puts up the arrival line ([`crate::shell::arrival_line`]): its name and
//! the faces of who is here. It is not put up again while nobody arrived
//! or left since the last time. The first time in a public workspace it
//! also says what the people there see of you, with "Change" to choose
//! otherwise. Someone waving at you shows a line of the same kind: their
//! face with a hand, "Ada waves". Nothing on either asks for anything; any
//! input dismisses them.

use std::collections::{BTreeSet, HashMap};
use std::time::Instant;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::shell::i18n::{t, t_args};
use crate::shell::state::RoomId;
use tilo_desktop::collaborators::RosterRow;

/// Widest the line's "what people see" sentence runs before it wraps.
const INTRO_WRAP: f32 = 300.0;

/// What the people in a public workspace see of the person, as the
/// arrival line tells them the first time.
fn intro_text(visibility: ui::PresenceVisibility) -> String {
    t(if visibility.shows_where() {
        "presence-arrival-intro-where"
    } else if visibility.shows_presence() {
        "presence-arrival-intro-here"
    } else {
        "presence-arrival-intro-nothing"
    })
}

/// What the line says.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NoticeKind {
    /// Someone waved at you.
    Wave { from: String },
}

#[derive(Clone, Debug)]
pub(crate) struct Notice {
    pub kind: NoticeKind,
    pub shown: Instant,
    /// Hand-shake of the wave: one pulse of the hand.
    hand: ui::motion::Attention,
}

#[derive(Default)]
pub(crate) struct PresenceNotices {
    pub(crate) current: Option<Notice>,
    /// Waves waiting their turn behind the line up now.
    pub(crate) waiting: Vec<String>,
    /// Who was here when the line last spoke of each workspace.
    pub(crate) seen: HashMap<RoomId, BTreeSet<String>>,
    /// The workspace on screen at the last look, and who was here.
    pub(crate) looked: Option<(RoomId, BTreeSet<String>)>,
    /// Public workspaces whose people this person was told about.
    pub(crate) introduced: BTreeSet<RoomId>,
    /// Where the line was painted, and its "Change" link.
    pub(crate) rect: Rect,
    pub(crate) change: Rect,
    /// The workspace tree's write generation at the last look: who is here
    /// is read again only after it changed.
    pub(crate) looked_at: Option<u64>,
}

impl PresenceNotices {
    /// The workspace on screen is `room` with `people` in it: whether to
    /// put the arrival line up, because this is a new arrival or who is here
    /// changed since the line last spoke of it. `intro` says the person is
    /// to be told about being seen there.
    pub(crate) fn look(&mut self, room: RoomId, people: &[String], intro: bool) -> bool {
        let here: BTreeSet<String> = people.iter().cloned().collect();
        let same = self
            .looked
            .as_ref()
            .is_some_and(|(seen_room, seen)| *seen_room == room && *seen == here);
        if same {
            return false;
        }
        let arrived = self
            .looked
            .as_ref()
            .is_none_or(|(seen_room, _)| *seen_room != room);
        self.looked = Some((room, here.clone()));
        // Within a workspace already on screen, the line speaks only of
        // arriving there; people coming and going show on their faces.
        if !arrived || here.is_empty() {
            return false;
        }
        if self.seen.get(&room) == Some(&here) {
            return false;
        }
        self.seen.insert(room, here);
        if intro {
            self.introduced.insert(room);
        }
        true
    }

    /// `from` waved: their line goes up, after the one up now.
    pub(crate) fn wave(&mut self, from: String, now: Instant) {
        if self.current.is_some() {
            self.waiting.push(from);
        } else {
            self.show(NoticeKind::Wave { from }, now);
        }
    }

    fn show(&mut self, kind: NoticeKind, now: Instant) {
        let mut hand = ui::motion::Attention::default();
        if matches!(kind, NoticeKind::Wave { .. }) {
            hand.pulse(
                ui::tokens::EffectiveMotion::new(ui::tokens::Motion::default(), false),
                now,
            );
        }
        self.current = Some(Notice {
            kind,
            shown: now,
            hand,
        });
    }

    /// Any input takes the line away (and brings up a waiting wave).
    pub(crate) fn dismiss(&mut self, now: Instant) -> bool {
        if self.current.take().is_none() {
            return false;
        }
        self.rect = Rect::ZERO;
        self.change = Rect::ZERO;
        if !self.waiting.is_empty() {
            let from = self.waiting.remove(0);
            self.show(NoticeKind::Wave { from }, now);
        }
        true
    }

    /// When the line up now goes on its own.
    pub(crate) fn expires(&self) -> Option<Instant> {
        self.current
            .as_ref()
            .map(|notice| notice.shown + super::presence_self::NOTICE_FOR)
    }

    /// Take the line away once it has been up its while. Returns whether
    /// it went.
    pub(crate) fn expire(&mut self, now: Instant) -> bool {
        if self.expires().is_some_and(|at| now >= at) {
            return self.dismiss(now);
        }
        false
    }
}

impl super::ShellOverlayHost {
    /// Look at who is in the workspace on screen, and at waves that came
    /// in, and put the line up when there is something to say.
    pub(crate) fn refresh_presence_notices(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = self.presence_notices.expire(now);
        // A wave waits while the arrival line is up: one line at a time.
        if !self.arrival_line.is_open() {
            for wave in std::mem::take(&mut self.self_presence.notices) {
                self.presence_notices.wave(wave.from, wave.at);
                changed = true;
            }
        }
        let Some(chrome) = self.chrome.clone() else {
            return changed;
        };
        let generation = chrome.borrow().tree_handle().generation();
        if self.presence_notices.looked_at == Some(generation) {
            return changed;
        }
        self.presence_notices.looked_at = Some(generation);
        let (room, workspace, rows) = {
            let state = chrome.borrow();
            let local = state.active_local_user();
            let tree = state.tree();
            let Some(active) = tree.desktop.active_workspace() else {
                return changed;
            };
            let rows: Vec<RosterRow> =
                tilo_desktop::collaborators::collaborator_roster_rows(&tree.desktop, &local)
                    .into_iter()
                    .filter(|row| !row.invited)
                    .collect();
            (active.room, active.name.clone(), rows)
        };
        let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
        let intro =
            self.active_workspace_is_public() && !self.presence_notices.introduced.contains(&room);
        if self.presence_notices.look(room, &ids, intro) {
            let people = rows
                .into_iter()
                .map(|row| crate::shell::arrival_line::Present {
                    name: row.label,
                    id: row.id,
                })
                .collect();
            self.arrival_line.show(&workspace, people, now);
            if intro {
                self.arrival_line
                    .tell(intro_text(self.presence_visibility(room)));
                self.persist_collaboration_preferences();
            }
            changed = true;
        }
        changed
    }

    /// The first time in a public workspace when it is joined: the arrival
    /// line the join put up says what its people see of the person.
    pub(crate) fn introduce_joined(&mut self, room: RoomId) {
        if self.presence_notices.introduced.contains(&room) || !self.room_is_public(room) {
            return;
        }
        self.presence_notices.introduced.insert(room);
        self.arrival_line
            .tell(intro_text(self.presence_visibility(room)));
        self.persist_collaboration_preferences();
    }

    /// A pointer press anywhere but on the line's "Change" dismisses it;
    /// one on "Change" opens the avatar menu. Returns whether it was the
    /// line's.
    pub(crate) fn press_presence_notice(&mut self, at: Point) -> bool {
        let change = self.arrival_line.change_at(at);
        if self.presence_notices.current.is_none() && !change {
            return false;
        }
        self.presence_notices.dismiss(Instant::now());
        if change {
            self.arrival_line.dismiss();
        }
        if change && let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().bar_popover_request =
                Some(crate::shell::chrome_state::BarPopoverRequest::Profile);
            self.harvest_bar_popover_request();
        }
        change
    }

    /// A key dismisses the line.
    pub(crate) fn dismiss_presence_notice(&mut self) -> bool {
        self.presence_notices.dismiss(Instant::now())
    }

    /// Whether the line is arriving or fading.
    pub(crate) fn presence_notice_moving(&self, now: Instant) -> bool {
        let Some(notice) = &self.presence_notices.current else {
            return false;
        };
        let expires = notice.shown + super::presence_self::NOTICE_FOR;
        ui::motion::Transient::is_moving(
            ui::motion::Transient::READOUT,
            (notice.shown, expires),
            now,
            self.motion.get(),
        ) || notice.hand.is_moving(now)
    }

    /// Paint the line under the top bar, centred on the display in use.
    /// Returns the bounds it painted.
    pub(super) fn draw_presence_notice(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
    ) -> Vec<Rect> {
        let Some(notice) = self.presence_notices.current.clone() else {
            return Vec::new();
        };
        let now = Instant::now();
        let motion = self.motion.get();
        let expires = notice.shown + super::presence_self::NOTICE_FOR;
        let opacity = ui::motion::Transient::opacity(
            ui::motion::Transient::READOUT,
            (notice.shown, expires),
            now,
            motion,
        );
        if opacity <= 0.0 {
            return Vec::new();
        }
        let rows = self.notice_rows();
        let content = NoticeContent::of(&notice.kind, &rows);
        let mut layer = Scene::new();
        let (rect, change) = content.paint(
            &mut layer,
            text_renderer,
            theme,
            self.notice_anchor(theme),
            &notice,
            now,
        );
        self.presence_notices.rect = rect;
        self.presence_notices.change = change;
        let bounds = ui::widget::floating_surface::painted_bounds(rect, theme);
        // Arrives from the bar it hangs under, and fades on its own.
        let pose = ui::motion::Appear::pose(
            f64::from(opacity),
            Point::new(rect.center().x, rect.y0),
            motion,
        );
        let placed = pose.applied_to(transform);
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            pose.opacity.min(opacity),
            placed,
            &bounds,
        );
        scene.append(&layer, Some(placed));
        scene.pop_layer();
        vec![transform.transform_rect_bbox(bounds.inflate(4.0, 12.0))]
    }

    /// The top centre under the top bar of the display in use.
    fn notice_anchor(&self, theme: &ui::Theme) -> Point {
        let area = self.overlay_area();
        Point::new(
            area.center().x,
            area.y0
                + f64::from(tilo_desktop::topbar::topbar_height(theme))
                + f64::from(theme.tokens.spacing.s3),
        )
    }

    fn notice_rows(&self) -> Vec<RosterRow> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let state = chrome.borrow();
        let local = state.active_local_user();
        tilo_desktop::collaborators::collaborator_roster_rows(&state.tree().desktop, &local)
    }
}

/// What a line shows, resolved against the roster.
struct NoticeContent<'a> {
    faces: Vec<&'a RosterRow>,
    /// The line's first words: the workspace's name, or "Ada waves".
    title: String,
    /// "and 12 others", after the faces.
    others: Option<String>,
    /// What the people here see of you, and the link to change it.
    intro: Option<String>,
    wave: bool,
}

impl<'a> NoticeContent<'a> {
    fn of(kind: &NoticeKind, rows: &'a [RosterRow]) -> Self {
        let NoticeKind::Wave { from } = kind;
        let row = rows.iter().find(|row| row.id == *from);
        let name = row.map_or_else(|| t("presence-someone"), |row| row.label.clone());
        Self {
            faces: row.into_iter().collect(),
            title: t_args("presence-wave-notice", &[("person", name.into())]),
            others: None,
            intro: None,
            wave: true,
        }
    }

    /// Paint the card hanging from `anchor` (its top centre). Returns the
    /// card and its "Change" link.
    fn paint(
        &self,
        scene: &mut Scene,
        text: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        anchor: Point,
        notice: &Notice,
        now: Instant,
    ) -> (Rect, Rect) {
        let spacing = &theme.tokens.spacing;
        let pad_x = f64::from(spacing.s4);
        let pad_y = f64::from(spacing.s3);
        let gap = f64::from(spacing.s3);
        let title_style = theme
            .text(ui::TextRole::Label)
            .with_weight(ui::text_render::TextWeight::Semibold);
        let caption = theme.text(ui::TextRole::Caption);
        let stack = ui::presence::FaceStack::WINDOW;
        let faces_w = stack.width_of(self.faces.len(), 0);
        let hand_w = if self.wave { 20.0 } else { 0.0 };
        let (title_w, title_h) = text.measure_style(&self.title, None, &title_style);
        let others = self
            .others
            .as_ref()
            .map(|others| text.measure_style(others, None, &caption));
        let line_h = f64::from(title_h).max(stack.side + 6.0);
        let mut line_w = f64::from(title_w);
        if faces_w > 0.0 {
            line_w += gap + faces_w;
        }
        if self.wave {
            line_w += hand_w;
        }
        if let Some((w, _)) = others {
            line_w += gap + f64::from(w);
        }
        let change_label = t("presence-arrival-change");
        let intro = self.intro.as_ref().map(|intro| {
            let (w, h) = text.measure_style(intro, Some(INTRO_WRAP), &caption);
            let (cw, _) = text.measure_style(
                &change_label,
                None,
                &theme
                    .text(ui::TextRole::Label)
                    .with_weight(ui::text_render::TextWeight::Medium),
            );
            (
                f64::from(w),
                f64::from(h),
                f64::from(cw) + 2.0 * f64::from(spacing.s4),
            )
        });
        let intro_line = intro.map_or(0.0, |(w, _, cw)| w + gap + cw);
        let width = line_w.max(intro_line) + 2.0 * pad_x;
        let height = line_h
            + intro.map_or(0.0, |(_, h, _)| f64::from(spacing.s2) + h.max(28.0))
            + 2.0 * pad_y;
        let rect = Rect::new(
            anchor.x - width / 2.0,
            anchor.y,
            anchor.x + width / 2.0,
            anchor.y + height,
        );
        ui::widget::floating_surface::paint_card(scene, Affine::IDENTITY, rect, theme);
        let center_y = rect.y0 + pad_y + line_h / 2.0;
        let mut x = rect.x0 + pad_x;
        let mut ctx = ui::paint::PaintCtx::new(scene, Affine::IDENTITY, text, theme);
        // Their face, waving, then who it is.
        if faces_w > 0.0 {
            let faces: Vec<ui::presence::Face<'_>> = self
                .faces
                .iter()
                .map(|row| {
                    let mut face = ui::presence::Face::new(&row.label, &row.id, row.color);
                    face.is_agent = row.is_agent;
                    face
                })
                .collect();
            stack.paint(
                &mut ctx,
                (x + faces_w, center_y),
                (&faces, 0),
                ui::widget::floating_surface::card_ground(theme),
            );
            x += faces_w;
        }
        if self.wave {
            // The hand waves once beside their face.
            let side = 16.0 * notice.hand.scale(now);
            let hand = Rect::from_center_size(Point::new(x + 2.0 + 8.0, center_y), (side, side));
            ui::icons::draw_icon(
                ctx.scene,
                Affine::IDENTITY,
                hand,
                ui::icons::Icon::Hand,
                theme.tokens.colors.text_primary,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
            x += hand_w;
        }
        x += gap;
        ctx.text(
            &self.title,
            &title_style,
            theme.tokens.colors.text_primary,
            Point::new(x, center_y - f64::from(title_h) / 2.0),
            None,
        );
        x += f64::from(title_w);
        if let (Some(others), Some((_, h))) = (&self.others, others) {
            x += gap;
            ctx.text(
                others,
                &caption,
                theme.tokens.colors.text_dim,
                Point::new(x, center_y - f64::from(h) / 2.0),
                None,
            );
        }
        let mut change = Rect::ZERO;
        if let (Some(intro_text), Some((_, h, cw))) = (&self.intro, intro) {
            let top = rect.y0 + pad_y + line_h + f64::from(spacing.s2);
            let row_h = h.max(28.0);
            ctx.text(
                intro_text,
                &caption,
                theme.tokens.colors.text_dim,
                Point::new(rect.x0 + pad_x, top + (row_h - h) / 2.0),
                Some(INTRO_WRAP),
            );
            let x1 = rect.x1 - pad_x;
            change = Rect::new(
                x1 - cw,
                top + (row_h - 28.0) / 2.0,
                x1,
                top + (row_h + 28.0) / 2.0,
            );
            ui::widget::Button::paint(
                ctx.scene,
                Affine::IDENTITY,
                ctx.text,
                theme,
                &ui::widget::ButtonFace::new(change, &change_label)
                    .role(ui::widget::ButtonRole::Plain),
            );
        }
        (rect, change)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    /// Arriving where people are says who; coming back with nobody new
    /// says nothing; a workspace with nobody else says nothing.
    #[test]
    fn arriving_says_who_is_here_once_until_it_changes() {
        let mut notices = PresenceNotices::default();
        let lounge = RoomId::new();
        let home = RoomId::new();
        assert!(notices.look(lounge, &ids(&["peer@1", "peer@2"]), false));
        assert!(
            !notices.look(lounge, &ids(&["peer@1", "peer@2", "peer@3"]), false),
            "people coming and going while you are here show on their faces"
        );
        assert!(
            !notices.look(home, &[], false),
            "nobody else: nothing to say"
        );
        assert!(
            notices.look(lounge, &ids(&["peer@1", "peer@2", "peer@3"]), false),
            "back, and someone new is here"
        );
        notices.look(home, &[], false);
        assert!(
            !notices.look(lounge, &ids(&["peer@1", "peer@2", "peer@3"]), false),
            "back with nothing changed: not again"
        );
    }

    /// The line goes on its own after its while, and any input takes it
    /// away; waves wait their turn.
    #[test]
    fn the_line_goes_on_its_own_and_waves_queue() {
        let mut notices = PresenceNotices::default();
        let now = Instant::now();
        notices.wave("peer@1".into(), now);
        notices.wave("peer@2".into(), now);
        assert!(!notices.expire(now + std::time::Duration::from_secs(1)));
        assert!(notices.expire(now + super::super::presence_self::NOTICE_FOR));
        assert!(
            matches!(
                notices.current.as_ref().map(|notice| &notice.kind),
                Some(NoticeKind::Wave { from }) if from == "peer@2"
            ),
            "the waiting wave comes up next"
        );
        assert!(notices.dismiss(now));
        assert!(notices.current.is_none());
    }

    /// The first time in a public workspace the line tells you what its
    /// people see of you; never again there.
    #[test]
    fn a_public_workspace_tells_you_once_what_its_people_see() {
        let mut notices = PresenceNotices::default();
        let lounge = RoomId::new();
        assert!(notices.look(lounge, &ids(&["peer@1"]), true));
        assert!(notices.introduced.contains(&lounge));
    }
}
