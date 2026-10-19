//! The arrival line: joining a workspace shows, for a moment at the top
//! centre under the top bar, the workspace's name and up to five faces of
//! who is here now, "and 12 others" for the rest. A toast-sized floating
//! surface on the standard [`ui::motion::Appear`] motion, with no buttons;
//! anything the person does dismisses it.
//!
//! The host shows it once a join installs ([`ArrivalLine::show`]), draws it
//! every frame it is visible, wakes for its end ([`ArrivalLine::deadline`])
//! and dismisses it on any input ([`ArrivalLine::dismiss`]).

use std::time::{Duration, Instant};

use ui::TextRole;
use ui::num::{f32_of_f64, f64_of_usize};
use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::shell::i18n::t_args;

/// How long the line stays.
const SHOWN_FOR: Duration = Duration::from_millis(2500);
/// Faces shown before the rest are counted.
const FACES_SHOWN: usize = 5;
/// One face.
const FACE: f64 = 24.0;
/// How far each face overlaps the one before.
const FACE_STEP: f64 = 16.0;
/// The space under the top bar.
const BELOW_BAR: f64 = 8.0;

/// Someone here now, as the line shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Present {
    /// Their name.
    pub name: String,
    /// What their face's colour is derived from: their stable id.
    pub id: String,
}

/// What the line says.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shown {
    workspace: String,
    people: Vec<Present>,
}

/// The arrival line.
#[derive(Default)]
pub struct ArrivalLine {
    appearing: ui::motion::Appearing,
    shown: Option<Shown>,
    until: Option<Instant>,
}

impl ArrivalLine {
    /// Show that the person arrived in `workspace`, where `people` are now,
    /// until [`SHOWN_FOR`] after `now`.
    pub fn show(&mut self, workspace: &str, people: Vec<Present>, now: Instant) {
        self.shown = Some(Shown {
            workspace: workspace.to_owned(),
            people,
        });
        self.until = now.checked_add(SHOWN_FOR);
        self.appearing.show(true);
    }

    /// Take it away; it leaves on its motion.
    pub fn dismiss(&mut self) {
        if self.until.take().is_some() {
            self.appearing.show(false);
        }
    }

    /// Whether it is meant to be up now.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.until.is_some()
    }

    /// Whether anything of it is drawn: up, or still leaving.
    #[must_use]
    pub fn visible(&self) -> bool {
        self.shown.is_some() && (self.until.is_some() || self.appearing.is_drawn())
    }

    /// Whether it moves this frame.
    #[must_use]
    pub fn needs_animation_frame(&self) -> bool {
        self.appearing.is_moving()
    }

    /// When it goes by itself, while it is up.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.until
    }

    /// End it once its time is over, as of `now`.
    pub fn expire(&mut self, now: Instant) {
        if self.until.is_some_and(|until| now >= until) {
            self.dismiss();
        }
    }

    /// The text the line says, for assistive technology and tests: the
    /// workspace, then who is here.
    #[must_use]
    pub fn text(&self) -> Option<String> {
        let shown = self.shown.as_ref().filter(|_| self.is_open())?;
        let mut text = shown.workspace.clone();
        let names: Vec<&str> = shown
            .people
            .iter()
            .take(FACES_SHOWN)
            .map(|person| person.name.as_str())
            .collect();
        if !names.is_empty() {
            text.push_str(": ");
            text.push_str(&names.join(", "));
        }
        if let Some(others) = others(shown) {
            text.push(' ');
            text.push_str(&others);
        }
        Some(text)
    }

    /// Paint it at the top centre of `host_rect`, under the top bar.
    pub fn draw(
        &mut self,
        host_rect: Rect,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.appearing.follow(self.until.is_some(), theme.motion());
        if !self.appearing.is_drawn() {
            if self.until.is_none() {
                self.shown = None;
            }
            return;
        }
        let Some(shown) = self.shown.clone() else {
            return;
        };
        let tokens = &theme.tokens;
        let pad = f64::from(tokens.spacing.s3);
        let gap = f64::from(tokens.spacing.s2);
        let title = theme.text(TextRole::Body).with_weight(TextWeight::Semibold);
        let caption = theme.text(TextRole::Body);
        let faces = shown.people.len().min(FACES_SHOWN);
        let faces_w = if faces == 0 {
            0.0
        } else {
            FACE + FACE_STEP * f64_of_usize(faces - 1)
        };
        let others = others(&shown);
        let name_w = f64::from(text.measure_style(&shown.workspace, None, &title).0);
        let others_w = others.as_deref().map_or(0.0, |others| {
            f64::from(text.measure_style(others, None, &caption).0)
        });
        let max_w = (host_rect.width() - 2.0 * f64::from(tokens.spacing.s4)).max(0.0);
        let content = name_w
            + if faces_w > 0.0 { gap + faces_w } else { 0.0 }
            + if others_w > 0.0 { gap + others_w } else { 0.0 };
        let w = (content + 2.0 * pad).min(max_w);
        let h = FACE + 2.0 * pad;
        let top = host_rect.y0 + f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX) + BELOW_BAR;
        let rect = Rect::new(
            host_rect.center().x - w / 2.0,
            top,
            host_rect.center().x + w / 2.0,
            top + h,
        );
        let pose = self
            .appearing
            .pose(rect, Point::new(rect.center().x, rect.y0));
        let Some(drawn) = pose.begin(scene, transform, rect.inflate(32.0, 32.0)) else {
            return;
        };
        let _ = ui::chrome::shell::paint_panel(scene, drawn, rect, theme, 1.0);
        let colors = &tokens.colors;
        let cy = rect.center().y;
        let mut x = rect.x0 + pad;
        let line = |style: &ui::text_render::TextStyle| f64::from(style.line_height_px());
        let name_room = (rect.x1 - pad - x - faces_w - others_w).max(0.0);
        let name = text.elide_to_width_style(&shown.workspace, f32_of_f64(name_room), &title);
        text.draw_style(
            scene,
            &name,
            colors.text_primary,
            drawn.pre_translate((x, cy - line(&title) / 2.0).into()),
            None,
            &title,
        );
        x += f64::from(text.measure_style(&name, None, &title).0) + gap;
        let ground = ui::chrome::shell::panel_ground(theme);
        let mut paint = PaintCtx::new(scene, drawn, text, theme);
        for (k, person) in shown.people.iter().take(FACES_SHOWN).enumerate() {
            let fx = x + FACE_STEP * f64_of_usize(k);
            ui::widget::AvatarFace::new(&person.name, &person.id)
                .ringed_on(ground)
                .paint(
                    &mut paint,
                    Rect::new(fx, cy - FACE / 2.0, fx + FACE, cy + FACE / 2.0),
                );
        }
        if faces_w > 0.0 {
            x += faces_w + gap;
        }
        if let Some(others) = others.as_deref() {
            let shown_others = paint.text.elide_to_width_style(
                others,
                f32_of_f64((rect.x1 - pad - x).max(0.0)),
                &caption,
            );
            paint.text.draw_style(
                paint.scene,
                &shown_others,
                colors.text_dim,
                drawn.pre_translate((x, cy - line(&caption) / 2.0).into()),
                None,
                &caption,
            );
        }
        pose.end(scene);
    }
}

/// "and 12 others" for the people past the faces shown, if any.
fn others(shown: &Shown) -> Option<String> {
    let rest = shown.people.len().saturating_sub(FACES_SHOWN);
    (rest > 0).then(|| t_args("arrival-others", &[("count", rest.into())]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn people(count: usize) -> Vec<Present> {
        (0..count)
            .map(|k| Present {
                name: format!("Person {k}"),
                id: format!("{k:064x}"),
            })
            .collect()
    }

    #[test]
    fn it_names_the_workspace_and_who_is_here_and_ends_after_its_moment() {
        let now = Instant::now();
        let mut line = ArrivalLine::default();
        assert_eq!(line.text(), None);
        line.show("Lounge", people(7), now);
        let text = line.text().expect("up");
        assert!(text.starts_with("Lounge: Person 0, Person 1"), "{text}");
        assert!(!text.contains("Person 5"), "five faces at most: {text}");
        assert!(text.ends_with("2 others"), "{text}");
        assert_eq!(line.deadline(), now.checked_add(SHOWN_FOR));
        line.expire(now + Duration::from_millis(100));
        assert!(line.is_open(), "still in its moment");
        line.expire(now + SHOWN_FOR);
        assert!(!line.is_open(), "gone once its moment is over");
        assert_eq!(line.deadline(), None, "nothing to wake for");
    }

    #[test]
    fn anything_the_person_does_dismisses_it() {
        let mut line = ArrivalLine::default();
        line.show("Lounge", Vec::new(), Instant::now());
        assert_eq!(line.text().as_deref(), Some("Lounge"));
        line.dismiss();
        assert!(!line.is_open());
        assert_eq!(line.text(), None);
    }
}
