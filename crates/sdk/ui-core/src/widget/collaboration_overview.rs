//! Universal visual hierarchy for “who is here”, live intent, and catch-up.
//! Inferred words are visibly marked and controlled beside the user's status;
//! they are never presented as if the participant typed them.

use crate::history_source::{ActivityBrief, ActivityDigest};
use crate::live_collaboration::{
    CollaborationExperience, PersonPresence, StatusOrigin, StatusSharing,
};
use crate::presence::{PresenceCtx, user_color};
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::avatar::Avatar;
use crate::widget::{TaffyTreeExt, Widget};
use std::collections::{HashMap, HashSet};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Vec2};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

const PAD: f64 = 16.0;
const PERSON_H: f64 = 72.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollaborationOverviewAction {
    EditStatus,
}

#[derive(Clone)]
struct OverviewTextStyle {
    style: TextStyle,
    color: Color,
}

#[derive(Clone, Copy)]
struct AvatarIdentity<'a> {
    name: &'a str,
    id: &'a str,
}

#[derive(Clone, Copy)]
struct BriefPlacement {
    index: usize,
    rect: Rect,
}

/// A reusable right-rail surface; every app can feed it the same room model.
pub struct CollaborationOverview {
    experience: CollaborationExperience,
    digest: ActivityDigest,
    rect: Rect,
    pointer: Point,
    status_hit: Rect,
    sharing_hits: Vec<(StatusSharing, Rect)>,
    brief_hits: Vec<(usize, Rect)>,
    expanded: HashSet<usize>,
    pending: Option<CollaborationOverviewAction>,
    dirty: bool,
}

impl CollaborationOverview {
    pub fn new(experience: CollaborationExperience) -> Self {
        Self {
            experience,
            digest: ActivityDigest::default(),
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            status_hit: Rect::ZERO,
            sharing_hits: Vec::new(),
            brief_hits: Vec::new(),
            expanded: HashSet::new(),
            pending: None,
            dirty: true,
        }
    }
    #[must_use]
    pub fn with_digest(mut self, digest: ActivityDigest) -> Self {
        self.digest = digest;
        self
    }
    pub fn experience(&self) -> &CollaborationExperience {
        &self.experience
    }
    pub fn experience_mut(&mut self) -> &mut CollaborationExperience {
        self.dirty = true;
        &mut self.experience
    }
    pub fn set_digest(&mut self, digest: ActivityDigest) {
        self.digest = digest;
        self.expanded.clear();
        self.dirty = true;
    }
    pub fn take_action(&mut self) -> Option<CollaborationOverviewAction> {
        self.pending.take()
    }

    fn text(
        tr: &mut TextRenderer,
        scene: &mut Scene,
        transform: Affine,
        text: &str,
        style: OverviewTextStyle,
        x: f64,
        y: f64,
    ) {
        tr.draw_style(
            scene,
            text,
            style.color,
            transform.pre_translate(Vec2::new(x, y)),
            None,
            &style.style,
        );
    }
    fn initials(name: &str) -> String {
        name.split_whitespace()
            .filter_map(|p| p.chars().next())
            .take(2)
            .collect()
    }
    fn avatar(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        identity: AvatarIdentity<'_>,
        rect: Rect,
        ringed: bool,
    ) {
        let fill = theme.tokens.colors.presence.of_rgb(user_color(identity.id));
        if ringed {
            Avatar::draw_ringed_at(
                &mut crate::paint::PaintCtx::new(scene, transform, tr, theme),
                rect,
                &Self::initials(identity.name),
                fill,
                crate::widget::AvatarRing {
                    color: theme.bg_primary,
                    width: 2.0,
                },
            );
        } else {
            Avatar::draw_at(
                scene,
                transform,
                rect,
                &Self::initials(identity.name),
                fill,
                theme,
                tr,
            );
        }
    }
    fn card(scene: &mut Scene, transform: Affine, theme: &Theme, rect: Rect) {
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.lift(0.35),
            None,
            &RoundedRect::from_rect(rect, theme.radius_standard),
        );
    }

    fn draw_header(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let people = self.experience.people();
        Self::text(
            tr,
            scene,
            transform,
            "TOGETHER",
            OverviewTextStyle {
                style: theme.text(TextRole::Subheading),
                color: theme.text_tertiary,
            },
            self.rect.x0 + PAD,
            self.rect.y0 + 12.0,
        );
        Self::text(
            tr,
            scene,
            transform,
            &format!("{} here now", people.len()),
            OverviewTextStyle {
                style: theme.text(TextRole::Title),
                color: theme.text_primary,
            },
            self.rect.x0 + PAD,
            self.rect.y0 + 31.0,
        );
        let mut x = self.rect.x1 - PAD - 28.0;
        for person in people.iter().take(4).rev() {
            Self::avatar(
                scene,
                transform,
                tr,
                theme,
                AvatarIdentity {
                    name: &person.name,
                    id: &person.id.0,
                },
                Rect::new(x, self.rect.y0 + 18.0, x + 28.0, self.rect.y0 + 46.0),
                true,
            );
            x -= 22.0;
        }
        scene.fill(
            Fill::NonZero,
            transform,
            theme.status_success,
            None,
            &Circle::new((self.rect.x1 - PAD - 1.0, self.rect.y0 + 22.0), 4.0),
        );
    }

    fn draw_own_status(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        top: f64,
    ) -> f64 {
        Self::text(
            tr,
            scene,
            transform,
            "MY STATUS",
            OverviewTextStyle {
                style: theme.text(TextRole::Subheading),
                color: theme.text_tertiary,
            },
            self.rect.x0 + PAD,
            top,
        );
        let r = Rect::new(
            self.rect.x0 + PAD,
            top + 18.0,
            self.rect.x1 - PAD,
            top + 92.0,
        );
        Self::card(scene, transform, theme, r);
        let status = self
            .experience
            .people()
            .into_iter()
            .find(|p| p.is_self)
            .and_then(|p| p.status.as_ref());
        let (mark, label, ink) = status.map_or(
            ("○", "Set what you're working toward…", theme.text_dim),
            |s| {
                (
                    if matches!(s.origin, StatusOrigin::Inferred { .. }) {
                        "✦"
                    } else {
                        "●"
                    },
                    s.text.as_str(),
                    theme.text_primary,
                )
            },
        );
        Self::text(
            tr,
            scene,
            transform,
            mark,
            OverviewTextStyle {
                style: theme.text(TextRole::Subheading),
                color: theme.tokens.colors.intent.inverted,
            },
            r.x0 + 12.0,
            r.y0 + 10.0,
        );
        Self::text(
            tr,
            scene,
            transform,
            label,
            OverviewTextStyle {
                style: theme.text(TextRole::Body),
                color: ink,
            },
            r.x0 + 34.0,
            r.y0 + 11.0,
        );
        self.status_hit = Rect::new(r.x0, r.y0, r.x1, r.y0 + 38.0);
        self.draw_sharing_controls(scene, transform, tr, theme, r);
        r.y1 + 18.0
    }

    fn draw_sharing_controls(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        r: Rect,
    ) {
        self.sharing_hits.clear();
        for (i, (mode, label)) in [
            (StatusSharing::Automatic, "✦ Auto"),
            (StatusSharing::ManualOnly, "Manual"),
            (StatusSharing::Off, "Hidden"),
        ]
        .into_iter()
        .enumerate()
        {
            let w = (r.width() - 24.0) / 3.0;
            let n = f64::from(u32::try_from(i).unwrap_or(0));
            let chip = Rect::new(
                r.x0 + 12.0 + n * w,
                r.y0 + 44.0,
                r.x0 + 8.0 + (n + 1.0) * w,
                r.y1 - 8.0,
            );
            let active = self.experience.sharing() == mode;
            scene.fill(
                Fill::NonZero,
                transform,
                if active {
                    theme.tokens.colors.intent.inverted
                } else {
                    theme.tokens.colors.lift(0.72)
                },
                None,
                &RoundedRect::from_rect(chip, 8.0),
            );
            Self::text(
                tr,
                scene,
                transform,
                label,
                OverviewTextStyle {
                    style: theme.text(TextRole::Subheading),
                    color: if active {
                        theme.tokens.colors.intent.on_inverted
                    } else {
                        theme.text_dim
                    },
                },
                chip.x0 + 8.0,
                chip.y0 + 5.0,
            );
            self.sharing_hits.push((mode, chip));
        }
    }

    fn draw_people(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        mut top: f64,
    ) -> f64 {
        Self::text(
            tr,
            scene,
            transform,
            "LIVE",
            OverviewTextStyle {
                style: theme.text(TextRole::Subheading),
                color: theme.text_tertiary,
            },
            self.rect.x0 + PAD,
            top,
        );
        top += 18.0;
        for person in self.experience.people().into_iter().filter(|p| !p.is_self) {
            let r = Rect::new(
                self.rect.x0 + PAD,
                top,
                self.rect.x1 - PAD,
                top + PERSON_H - 6.0,
            );
            if r.y1 > self.rect.y1 {
                break;
            }
            Self::draw_person(scene, transform, tr, theme, person, r);
            top += PERSON_H;
        }
        top + 8.0
    }

    fn draw_person(
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        person: &PersonPresence,
        rect: Rect,
    ) {
        Self::card(scene, transform, theme, rect);
        Self::avatar(
            scene,
            transform,
            tr,
            theme,
            AvatarIdentity {
                name: &person.name,
                id: &person.id.0,
            },
            Rect::new(
                rect.x0 + 10.0,
                rect.y0 + 13.0,
                rect.x0 + 46.0,
                rect.y0 + 49.0,
            ),
            false,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.status_success,
            None,
            &Circle::new((rect.x0 + 42.0, rect.y0 + 45.0), 4.0),
        );
        let name = if person.is_agent {
            format!("{}  ◇", person.name)
        } else {
            person.name.clone()
        };
        Self::text(
            tr,
            scene,
            transform,
            &name,
            OverviewTextStyle {
                style: theme.text(TextRole::Label),
                color: theme.text_primary,
            },
            rect.x0 + 56.0,
            rect.y0 + 8.0,
        );
        let doing = person
            .current
            .as_ref()
            .map_or("Viewing this workspace", |activity| {
                activity.action.as_str()
            });
        Self::text(
            tr,
            scene,
            transform,
            doing,
            OverviewTextStyle {
                style: theme.text(TextRole::Caption),
                color: theme.text_dim,
            },
            rect.x0 + 56.0,
            rect.y0 + 27.0,
        );
        if let Some(status) = &person.status {
            let mark = if matches!(status.origin, StatusOrigin::Inferred { .. }) {
                "✦  "
            } else {
                "↳  "
            };
            Self::text(
                tr,
                scene,
                transform,
                &format!("{mark}{}", status.text),
                OverviewTextStyle {
                    style: theme.text(TextRole::Caption),
                    color: theme.tokens.colors.intent.inverted,
                },
                rect.x0 + 56.0,
                rect.y0 + 45.0,
            );
        }
    }

    fn draw_brief(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        brief: &ActivityBrief,
        placement: BriefPlacement,
    ) {
        let BriefPlacement { index, rect: r } = placement;
        Self::card(scene, transform, theme, r);
        let open = self.expanded.contains(&index);
        Self::text(
            tr,
            scene,
            transform,
            if open { "⌄" } else { "›" },
            OverviewTextStyle {
                style: theme.text(TextRole::Title).with_weight(TextWeight::Regular),
                color: theme.text_dim,
            },
            r.x0 + 10.0,
            r.y0 + 10.0,
        );
        let mut x = r.x0 + 32.0;
        for a in brief.authors.iter().take(3) {
            Self::avatar(
                scene,
                transform,
                tr,
                theme,
                AvatarIdentity {
                    name: &a.name,
                    id: &a.identity_short,
                },
                Rect::new(x, r.y0 + 9.0, x + 24.0, r.y0 + 33.0),
                true,
            );
            x += 18.0;
        }
        Self::text(
            tr,
            scene,
            transform,
            &brief.glance(),
            OverviewTextStyle {
                style: theme.text(TextRole::Label),
                color: theme.text_primary,
            },
            r.x0 + 32.0,
            r.y0 + 38.0,
        );
        Self::text(
            tr,
            scene,
            transform,
            &brief.detail,
            OverviewTextStyle {
                style: theme.text(TextRole::Caption),
                color: theme.text_dim,
            },
            r.x0 + 32.0,
            r.y0 + 57.0,
        );
        if open {
            for (n, a) in brief.actions.iter().take(3).enumerate() {
                let y = r.y0 + 82.0 + f64::from(u32::try_from(n).unwrap_or(0)) * 22.0;
                Self::text(
                    tr,
                    scene,
                    transform,
                    &format!("·  {}  {}", a.author.name, a.summary),
                    OverviewTextStyle {
                        style: theme.text(TextRole::Caption),
                        color: theme.text_dim,
                    },
                    r.x0 + 34.0,
                    y,
                );
            }
        }
    }

    fn draw_catchup(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        mut top: f64,
    ) {
        self.brief_hits.clear();
        if self.digest.total_changes == 0 || top > self.rect.y1 - 30.0 {
            return;
        }
        Self::text(
            tr,
            scene,
            transform,
            "WHILE YOU WERE AWAY",
            OverviewTextStyle {
                style: theme.text(TextRole::Subheading),
                color: theme.text_tertiary,
            },
            self.rect.x0 + PAD,
            top,
        );
        Self::text(
            tr,
            scene,
            transform,
            &format!(
                "{} changes · {} people",
                self.digest.total_changes, self.digest.people
            ),
            OverviewTextStyle {
                style: theme.text(TextRole::Caption),
                color: theme.text_dim,
            },
            self.rect.x1 - PAD - 116.0,
            top,
        );
        top += 20.0;
        for (i, b) in self.digest.briefs.iter().enumerate() {
            let extra = if self.expanded.contains(&i) {
                f64::from(u32::try_from(b.actions.len().min(3)).unwrap_or(0)) * 22.0 + 8.0
            } else {
                0.0
            };
            let r = Rect::new(
                self.rect.x0 + PAD,
                top,
                self.rect.x1 - PAD,
                top + 78.0 + extra,
            );
            if r.y0 >= self.rect.y1 {
                break;
            }
            self.draw_brief(
                scene,
                transform,
                tr,
                theme,
                b,
                BriefPlacement { index: i, rect: r },
            );
            self.brief_hits.push((i, r));
            top = r.y1 + 8.0;
        }
    }
}

impl Widget for CollaborationOverview {
    fn build_layout(
        &mut self,
        t: &mut TaffyTree<()>,
        _: &Theme,
        m: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let n = t.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        });
        m.insert(n, std::ptr::from_mut::<dyn Widget>(self));
        n
    }
    fn apply_layout(&mut self, t: &TaffyTree<()>, n: NodeId, o: Point) {
        let l = t.layout(n).copied().unwrap_or_default();
        self.rect = Rect::new(
            o.x + f64::from(l.location.x),
            o.y + f64::from(l.location.y),
            o.x + f64::from(l.location.x + l.size.width),
            o.y + f64::from(l.location.y + l.size.height),
        );
        self.dirty = true;
    }
    fn draw(
        &mut self,
        s: &mut Scene,
        a: Affine,
        tr: &mut TextRenderer,
        th: &Theme,
        _: &PresenceCtx,
    ) {
        s.fill(Fill::NonZero, a, th.bg_primary, None, &self.rect);
        self.draw_header(s, a, tr, th);
        let top = self.draw_own_status(s, a, tr, th, self.rect.y0 + 72.0);
        let top = self.draw_people(s, a, tr, th, top);
        self.draw_catchup(s, a, tr, th, top);
        self.dirty = false;
    }
    fn handle_event(&mut self, e: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match e {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.status_hit.contains(self.pointer) {
                    self.pending = Some(CollaborationOverviewAction::EditStatus);
                    *ctx.handled = true;
                } else if let Some((mode, _)) = self
                    .sharing_hits
                    .iter()
                    .find(|(_, r)| r.contains(self.pointer))
                    .copied()
                {
                    self.experience.set_sharing(mode);
                    self.dirty = true;
                    *ctx.handled = true;
                } else if let Some((i, _)) = self
                    .brief_hits
                    .iter()
                    .find(|(_, r)| r.contains(self.pointer))
                    .copied()
                {
                    if !self.expanded.remove(&i) {
                        self.expanded.insert(i);
                    }
                    self.dirty = true;
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }
    fn name(&self) -> &'static str {
        "CollaborationOverview"
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
