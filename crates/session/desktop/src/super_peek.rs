//! The keys a held Super reaches, said at a glance over the desk.
//!
//! Holding Super alone is reaching for a chord, or looking for one: the desk answers with a quiet
//! card at the bottom of the display the person works on, naming what each key does while Super is
//! down. It grows in two steps, as a hold lasts:
//! - the glance, as the hold begins: one row from each group (the windows, the tabs, the apps,
//!   the agent and search), so a hand that held Super a moment too long on the way to a chord sees
//!   a strip and no more;
//! - the whole card [`GROW`] later: each group's title and the rest of its rows unfold under the
//!   glance's row, which rises with the card.
//!
//! It is a picture, not a surface: it takes no focus, no key and no pointer, so every chord still
//! runs while it shows, and letting Super go or pressing one takes it away. The keys are the
//! session's shortcut table as the person bound it, read when the hold began; what it shows is
//! the session's to say ([`SuperPeek::show`]), the chrome only draws it.

use std::cell::RefCell;
use std::rc::Rc;

use ui::a11y::{AccessNode, Politeness, Role};
use ui::motion::{Appear, Direction, Disclose, Presence};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::tokens::{MotionSource, TextRole};
use ui::widget::{InputHint, KeyHint, KeyHintsFace, TextLineFace};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};

/// How long after the glance the whole card unfolds: with the hold's own threshold, 600 ms into a
/// held Super, where a person still holding it is reading rather than reaching for a key.
pub const GROW: Duration = Duration::from_millis(300);

/// A group of the peek: its title, and each row's keys and what they do.
pub type PeekGroup = (String, Vec<KeyHint>);

/// The keys a held Super reaches, as the session hands them over and the chrome draws them.
/// Clones share one peek.
#[derive(Clone, Default)]
pub struct SuperPeek {
    inner: Rc<RefCell<Inner>>,
}

#[derive(Default)]
struct Inner {
    groups: Vec<PeekGroup>,
    /// Whether the session wants it up, and since when it asked for that or for it to go.
    up: bool,
    asked: Option<Instant>,
    /// Its coming and going, and the whole card unfolding from the glance.
    appear: Presence,
    grown: Presence,
    /// Moves whenever what it shows moves; the revision last drawn, and the one the layout is of.
    revision: u64,
    drawn: u64,
    layout: Option<(u64, Layout)>,
    /// Anything of it stood in the last frame drawn.
    painted: bool,
}

impl Inner {
    /// When the whole card is due, while the glance waits for it.
    fn grows_at(&self) -> Option<Instant> {
        (self.up && !self.grown.is_open())
            .then(|| self.asked.and_then(|at| at.checked_add(GROW)))
            .flatten()
    }
}

impl SuperPeek {
    /// Put the glance up with `groups` , the whole card to follow [`GROW`] later. A peek already up
    /// stays as it is.
    pub fn show(&self, groups: Vec<PeekGroup>) {
        let mut inner = self.inner.borrow_mut();
        if inner.up {
            return;
        }
        inner.groups = groups;
        inner.up = true;
        inner.asked = Some(ui::time::now());
        inner.revision = inner.revision.wrapping_add(1);
    }

    /// Take it away: the hold ended.
    pub fn hide(&self) {
        let mut inner = self.inner.borrow_mut();
        if !inner.up {
            return;
        }
        inner.up = false;
        inner.asked = Some(ui::time::now());
        inner.revision = inner.revision.wrapping_add(1);
    }

    /// Whether the session wants it up.
    #[must_use]
    pub fn is_up(&self) -> bool {
        self.inner.borrow().up
    }

    /// What it shows, while it is up: each group's title and rows.
    #[must_use]
    pub fn groups(&self) -> Option<Vec<PeekGroup>> {
        let inner = self.inner.borrow();
        inner.up.then(|| inner.groups.clone())
    }

    /// Whether a frame is owed at `now` : what it shows changed, it moves, the whole card is due,
    /// or the last of it still stands in the scene.
    #[must_use]
    pub fn needs_redraw(&self, now: Instant) -> bool {
        let inner = self.inner.borrow();
        inner.drawn != inner.revision
            || self.is_moving(now)
            || inner.grows_at().is_some_and(|at| at <= now)
            || (inner.painted && !inner.appear.is_visible(now))
    }

    /// Whether it moves at `now` , so the frame clock owes frames.
    #[must_use]
    pub fn is_moving(&self, now: Instant) -> bool {
        let inner = self.inner.borrow();
        inner.appear.is_moving(now) || inner.grown.is_moving(now)
    }

    /// When the whole card is due, while the glance waits for it.
    #[must_use]
    pub fn next_wake_deadline(&self, now: Instant) -> Option<Instant> {
        self.inner.borrow().grows_at().filter(|at| *at > now)
    }

    /// What it says to assistive technology while it is up: each group's keys and what they do.
    #[must_use]
    pub fn accessibility(&self) -> Option<AccessNode> {
        let inner = self.inner.borrow();
        if !inner.up {
            return None;
        }
        let said: Vec<String> = inner
            .groups
            .iter()
            .map(|(title, hints)| {
                format!("{title}: {}", KeyHintsFace::new(hints).spoken().join(", "))
            })
            .collect();
        Some(
            AccessNode::new(Role::StaticText)
                .with_hashed_key("chrome-super-peek")
                .with_live(Politeness::Polite)
                .with_value(said.join(". ")),
        )
    }

    /// Paint it at the bottom of `host` , the display the person works on, as far as it has come.
    pub fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        host: Rect,
    ) {
        let now = ui::time::now();
        let motion = theme.motion();
        let mut guard = self.inner.borrow_mut();
        let inner = &mut *guard;
        inner.drawn = inner.revision;
        if let Some(asked) = inner.asked
            && inner.up != inner.appear.is_open()
        {
            // Started at the instant it was asked for, not at the frame that draws it.
            inner
                .appear
                .show(inner.up, Appear::SPEC, motion, MotionSource::Pointer, asked);
            want_frames(&inner.appear);
        }
        if let Some(at) = inner.grows_at().filter(|at| *at <= now) {
            inner
                .grown
                .show(true, Disclose::SPEC, motion, MotionSource::Pointer, at);
            want_frames(&inner.grown);
        }
        if !inner.appear.is_visible(now) {
            // Gone: the next hold starts from the glance.
            inner.grown.snap(false);
            inner.painted = false;
            return;
        }
        if inner
            .layout
            .as_ref()
            .is_none_or(|(of, _)| *of != inner.revision)
        {
            let layout = Layout::of(&inner.groups, text, theme);
            inner.layout = Some((inner.revision, layout));
        }
        let Some((_, layout)) = &inner.layout else {
            return;
        };
        let grown = inner.grown.progress(now);
        let card = layout.card(host, grown, theme);
        let anchor = Point::new(card.center().x, host.y1);
        let pose = Appear::pose(
            inner.appear.progress(now),
            Direction::of(inner.appear.is_open()),
            card,
            anchor,
            motion,
        );
        inner.painted = true;
        let groups = &inner.groups;
        pose.paint(
            scene,
            transform,
            ui::widget::floating_surface::painted_bounds(card, theme),
            |scene, at| {
                let painted = ui::chrome::shell::paint_panel(scene, at, card, theme, 1.0);
                scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, at, &painted.body);
                layout.paint(scene, at, text, &painted.theme, groups, (card, grown));
                scene.pop_layer();
            },
        );
    }
}

/// Ask the frame pump for frames until `presence` settles.
fn want_frames(presence: &Presence) {
    if let Some(until) = presence.settles_at() {
        ui::motion::want_frames_until(until);
    }
}

/// Where the card's columns stand, measured once per hold.
#[derive(Debug, Default, PartialEq)]
struct Layout {
    /// Each group's column: its left edge from the card's content edge, the width of its keys,
    /// and each row's own keys' width, which stand against the row's words.
    columns: Vec<(f64, f64, Vec<f64>)>,
    /// The card's content width, and its rows' count at the most.
    width: f64,
    rows: usize,
    title: f64,
    keycap: f64,
}

impl Layout {
    fn of(groups: &[PeekGroup], text: &mut TextRenderer, theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let related = f64::from(spacing.rhythm.related);
        // Titles and a hint's words are both Caption: `KeyHintsFace` sets the words in it.
        let style = theme.text(TextRole::Caption);
        let mut columns = Vec::with_capacity(groups.len());
        let mut x = 0.0;
        for (title, hints) in groups {
            let widths: Vec<f64> = hints
                .iter()
                .map(|hint| InputHint::measure_keycap_labels(&hint.keys, text, theme))
                .collect();
            let keys = widths.iter().copied().fold(0.0, f64::max);
            let labels = hints
                .iter()
                .map(|hint| f64::from(text.measure_style(&hint.label, None, &style).0))
                .fold(0.0, f64::max);
            let titled = f64::from(text.measure_style(title, None, &style).0);
            if !columns.is_empty() {
                x += f64::from(spacing.s8);
            }
            columns.push((x, keys, widths));
            x += (keys + related + labels).max(titled);
        }
        Self {
            columns,
            width: x,
            rows: groups
                .iter()
                .map(|(_, hints)| hints.len())
                .max()
                .unwrap_or(0),
            title: f64::from(text.measure_style("Ag", None, &style).1),
            keycap: InputHint::keycap_height(theme),
        }
    }

    /// The card on `host` , `grown` of the way from the glance to the whole card: its bottom edge
    /// stays where it is and its top rises.
    fn card(&self, host: Rect, grown: f64, theme: &Theme) -> Rect {
        let spacing = &theme.tokens.spacing;
        let (glance, whole) = self.heights(theme);
        let height = glance + (whole - glance) * grown;
        let width = self.width + 2.0 * f64::from(spacing.s6);
        let bottom = host.y1 - f64::from(spacing.s16);
        let centre = host.center().x;
        Rect::new(
            centre - width / 2.0,
            bottom - height,
            centre + width / 2.0,
            bottom,
        )
    }

    /// The glance's height, one row of keys; and the whole card's, every group's title and rows.
    fn heights(&self, theme: &Theme) -> (f64, f64) {
        let spacing = &theme.tokens.spacing;
        #[allow(
            clippy::cast_precision_loss,
            reason = "a group holds a handful of rows"
        )]
        let rows = self.rows as f64;
        let glance = self.keycap + 2.0 * f64::from(spacing.s3);
        let whole = 2.0 * f64::from(spacing.s6)
            + self.title
            + f64::from(spacing.s3)
            + rows * self.keycap
            + (rows - 1.0).max(0.0) * f64::from(spacing.s3);
        (glance, whole)
    }

    /// The card's contents: the glance's row of each group, and the titles and the other rows
    /// fading in over the second half of the card's unfolding.
    fn paint(
        &self,
        scene: &mut Scene,
        at: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        groups: &[PeekGroup],
        (card, grown): (Rect, f64),
    ) {
        let spacing = &theme.tokens.spacing;
        let (gap, edge) = (f64::from(spacing.s3), f64::from(spacing.s6));
        let colors = &theme.tokens.colors;
        // The glance's row, centred in the strip, rises to stand under its group's title.
        let first = card.y0 + gap + (edge + self.title) * grown;
        let rest = Disclose::reveal(grown, 1.0).opacity;
        let title_style = theme.text(TextRole::Caption);
        let left = card.x0 + edge;
        let mut ctx = ui::paint::PaintCtx::new(scene, at, text, theme);
        let row = |ctx: &mut ui::paint::PaintCtx<'_>,
                   x: f64,
                   y: f64,
                   (keys, own): (f64, f64),
                   hint: &KeyHint| {
            // Right-aligned against the words they go with, so a wide pair elsewhere in the column
            // leaves no gap after a single key.
            let face = KeyHintsFace::new(std::slice::from_ref(hint));
            let from = x + keys - own;
            let to = from + face.width(ctx.text, ctx.theme);
            face.paint(ctx, Rect::new(from, y, to, y + self.keycap));
        };
        for ((_, hints), (x, keys, widths)) in groups.iter().zip(&self.columns) {
            if let (Some(hint), Some(own)) = (hints.first(), widths.first()) {
                row(&mut ctx, left + x, first, (*keys, *own), hint);
            }
        }
        if rest <= 0.0 {
            return;
        }
        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), rest, at, &card);
        for ((title, hints), (x, keys, widths)) in groups.iter().zip(&self.columns) {
            let line = TextLineFace::styled(title, &title_style, TextRole::Caption).start();
            let (width, height) = line.measure(ctx.text);
            let top = first - gap - height;
            line.paint(
                &mut ctx,
                Rect::new(left + x, top, left + x + width, top + height),
                colors.text_dim,
            );
            let mut y = first;
            for (hint, own) in hints.iter().zip(widths).skip(1) {
                y += self.keycap + gap;
                row(&mut ctx, left + x, y, (*keys, *own), hint);
            }
        }
        ctx.scene.pop_layer();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups() -> Vec<PeekGroup> {
        vec![
            (
                "Windows".to_owned(),
                vec![
                    KeyHint::new(["Left", "Right"], "Move focus"),
                    KeyHint::new(["Up", "Down"], "Maximize or restore"),
                ],
            ),
            (
                "Agent and search".to_owned(),
                vec![KeyHint::new(["space"], "Search everywhere")],
            ),
        ]
    }

    #[test]
    fn a_hold_shows_the_glance_then_owes_the_whole_card_and_letting_go_takes_it_away() {
        let peek = SuperPeek::default();
        let now = ui::time::now();
        assert!(!peek.is_up());
        assert!(!peek.needs_redraw(now), "nothing is owed before a hold");
        peek.show(groups());
        let now = ui::time::now();
        assert!(peek.is_up());
        assert!(peek.needs_redraw(now), "the glance is owed a frame");
        let due = peek
            .next_wake_deadline(now)
            .expect("the whole card is due later");
        assert!(due > now && Some(due) <= now.checked_add(GROW));
        assert_eq!(peek.groups().map(|groups| groups.len()), Some(2));
        let said = peek.accessibility().expect("it speaks while up");
        assert!(
            said.value
                .as_deref()
                .is_some_and(|value| value.contains("Search everywhere"))
        );

        peek.hide();
        assert!(!peek.is_up());
        assert_eq!(
            peek.next_wake_deadline(now),
            None,
            "nothing grows once it goes"
        );
        assert!(peek.accessibility().is_none());
    }

    #[test]
    fn a_second_show_while_up_keeps_what_the_hold_began_with() {
        let peek = SuperPeek::default();
        peek.show(groups());
        peek.show(Vec::new());
        assert_eq!(peek.groups().map(|groups| groups.len()), Some(2));
    }
}
