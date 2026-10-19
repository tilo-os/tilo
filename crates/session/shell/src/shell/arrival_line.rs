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
use ui::num::f32_of_f64;
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
/// The faces' line.
const FACE: f64 = 24.0;
/// How the faces stand: as the top bar's, five at most.
const FACES: ui::presence::FaceStack = ui::presence::FaceStack {
    side: 20.0,
    overlap: 6.0,
    max: FACES_SHOWN,
};
/// The space under the top bar.
const BELOW_BAR: f64 = 8.0;

/// Someone here now, as the line shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Present {
    /// Their name.
    pub name: String,
    /// Their stable id: what their face's colour is derived from when the
    /// roster gives none.
    pub id: String,
    /// The colour the roster paints them in everywhere else, as `(r, g, b)`.
    pub colour: Option<(u8, u8, u8)>,
    /// Software, not a person: the agent mark instead of an initial.
    pub is_agent: bool,
}

/// What the line says.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shown {
    workspace: String,
    people: Vec<Present>,
    /// What the people here see of the person, told the first time in a
    /// public workspace, with "Change" beside it.
    intro: Option<String>,
}

/// The arrival line.
#[derive(Default)]
pub struct ArrivalLine {
    appearing: ui::motion::Appearing,
    shown: Option<Shown>,
    until: Option<Instant>,
    /// Where it was drawn, and its "Change" link, for the pointer.
    rect: Rect,
    change: Rect,
}

impl ArrivalLine {
    /// Show that the person arrived in `workspace`, where `people` are now,
    /// until [`SHOWN_FOR`] after `now`.
    pub fn show(&mut self, workspace: &str, people: Vec<Present>, now: Instant) {
        self.shown = Some(Shown {
            workspace: workspace.to_owned(),
            people,
            intro: None,
        });
        self.until = now.checked_add(SHOWN_FOR);
        self.appearing.show(true);
    }

    /// Also tell the person, under the line, what the people here see of
    /// them, with "Change" to choose otherwise.
    pub fn tell(&mut self, intro: String) {
        if let Some(shown) = self.shown.as_mut() {
            shown.intro = Some(intro);
        }
    }

    /// Whether `at` is on the line's "Change", while it is up.
    #[must_use]
    pub fn change_at(&self, at: Point) -> bool {
        self.is_open() && self.change.contains(at)
    }

    /// Where the line is drawn, while it is up.
    #[must_use]
    pub fn rect(&self) -> Rect {
        if self.is_open() {
            self.rect
        } else {
            Rect::ZERO
        }
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
            FACES.width_of(faces, 0)
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
        let change_label = crate::shell::i18n::t("presence-arrival-change");
        let intro_size = shown.intro.as_deref().map(|intro| {
            let (w, h) =
                text.measure_style(intro, Some(INTRO_WRAP), &theme.text(TextRole::Caption));
            let (cw, _) = text.measure_style(
                &change_label,
                None,
                &theme.text(TextRole::Label).with_weight(TextWeight::Medium),
            );
            (
                f64::from(w),
                f64::from(h).max(CONTROL),
                f64::from(cw) + 2.0 * f64::from(tokens.spacing.s4),
            )
        });
        let content = content.max(intro_size.map_or(0.0, |(w, _, cw)| w + gap + cw));
        let w = (content + 2.0 * pad).min(max_w);
        let h = FACE + 2.0 * pad + intro_size.map_or(0.0, |(_, h, _)| gap + h);
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
        self.rect = rect;
        let colors = &tokens.colors;
        let cy = rect.y0 + pad + FACE / 2.0;
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
        // The same faces as the top bar's: a colour ring, one initial.
        let faces: Vec<ui::presence::Face<'_>> = shown
            .people
            .iter()
            .take(FACES_SHOWN)
            .map(|person| ui::presence::Face {
                is_agent: person.is_agent,
                ..ui::presence::Face::new(
                    &person.name,
                    &person.id,
                    colors.presence.of_rgb(
                        person
                            .colour
                            .unwrap_or_else(|| ui::presence::user_color(&person.id)),
                    ),
                )
            })
            .collect();
        if !faces.is_empty() {
            FACES.paint(&mut paint, (x + faces_w, cy), (&faces, 0), ground);
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
        self.change = Rect::ZERO;
        if let (Some(intro), Some((_, row_h, cw))) = (shown.intro.as_deref(), intro_size) {
            let top = rect.y0 + pad + FACE + gap;
            let caption = theme.text(TextRole::Caption);
            let (_, ih) = paint.text.measure_style(intro, Some(INTRO_WRAP), &caption);
            paint.text.draw_style(
                paint.scene,
                intro,
                colors.text_dim,
                drawn.pre_translate((rect.x0 + pad, top + (row_h - f64::from(ih)) / 2.0).into()),
                Some(INTRO_WRAP),
                &caption,
            );
            let change = Rect::new(
                rect.x1 - pad - cw,
                top + (row_h - CONTROL) / 2.0,
                rect.x1 - pad,
                top + (row_h + CONTROL) / 2.0,
            );
            ui::widget::Button::paint(
                paint.scene,
                drawn,
                paint.text,
                theme,
                &ui::widget::ButtonFace::new(change, &change_label)
                    .role(ui::widget::ButtonRole::Plain),
            );
            self.change = change;
        }
        pose.end(scene);
    }
}

/// Widest the sentence about what people see runs before it wraps.
const INTRO_WRAP: f32 = 300.0;
/// A control's height: the "Change" link's.
const CONTROL: f64 = 28.0;

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
                colour: None,
                is_agent: false,
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
