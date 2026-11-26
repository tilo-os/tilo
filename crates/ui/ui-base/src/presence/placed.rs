//! Other people's pointers that a host has already placed in its own
//! surface's coordinates (the browser page, which interpolates each member's
//! live pointer itself): drawn by the frame with the same arrow, name tag
//! and colour a native shell draws them with, never by a second renderer.

use super::{ChatBubble, PeerCursor, PresenceCtx, ReactionFloat};
use crate::paint::PaintCtx;
use crate::permissions::MemberId;
use vello::kurbo::{Point, Rect, RoundedRect};
use vello::peniko::Color;
use vello::peniko::Fill;

/// One placed pointer as the host hands it over.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedPointer {
    /// Who points.
    pub user: MemberId,
    /// Where, in the surface's logical pixels.
    pub at: (f64, f64),
    /// The name their own presence gives, for someone presence does not
    /// name yet.
    pub name: Option<String>,
    /// What an agent is doing, shown after its name.
    pub activity: Option<String>,
    /// What they are saying beside their pointer.
    pub chat: Option<ChatBubble>,
    /// A reaction floating up from their pointer.
    pub reaction: Option<ReactionFloat>,
}

impl PresenceCtx {
    /// `placed` as the frame draws it: in the colour and under the name
    /// this presence knows the person by, so a pointer always matches their
    /// avatar. Someone presence has no name for yet goes by the one their
    /// own presence gives, else a guest: never their key.
    #[must_use]
    pub fn placed_cursor(&self, placed: PlacedPointer) -> PeerCursor {
        let is_agent = !placed.user.is_person();
        let name = self.placed_name(&placed.user, placed.name.as_deref());
        let label = match placed.activity.as_deref().map(str::trim) {
            Some(activity) if is_agent && !activity.is_empty() => format!("{name} · {activity}"),
            _ => name,
        };
        let (r, g, b) = self.color_for(&placed.user);
        PeerCursor {
            pos: placed.at,
            label,
            color: Color::from_rgb8(r, g, b),
            is_agent,
            opacity: 1.0,
            name_opacity: 1.0,
            chat: placed.chat,
            reaction: placed.reaction,
        }
    }
}

impl PresenceCtx {
    /// The name `user` is shown by where an app placed them: the one this presence knows them
    /// by, else `given` (their own presence's), else a guest's; never their key. An agent's
    /// tag carries the agent mark, so its name stands alone.
    fn placed_name(&self, user: &MemberId, given: Option<&str>) -> String {
        let known = if user.is_person() {
            self.names
                .contains_key(user)
                .then(|| self.display_name(user))
        } else {
            self.names.get(user).cloned()
        };
        known.unwrap_or_else(|| super::person_label(given.unwrap_or_default()))
    }

    /// Members an app placed at one thing it shows (agents at a map's pin): the first one's face
    /// in one pill in their colour that names them ("Scout", or "Scout +1"), and the others'
    /// faces set left of it outside the pill's end, each under the one before, with the hindmost
    /// one's centre at `dock`. A ring of `ground` (the surface under them) round the pill and
    /// each face, and the contact shadow, lift them off any colour. `followed`: this person
    /// follows the first one, and the pill wears a ring in their colour. Kept inside
    /// `viewport`. Answers where it painted.
    pub fn draw_placed_tag(
        &self,
        ctx: &mut PaintCtx<'_>,
        (dock, viewport): (Point, Rect),
        members: &[(MemberId, Option<String>)],
        ground: Color,
        followed: bool,
    ) -> Rect {
        let Some((first, given)) = members.first() else {
            return Rect::ZERO;
        };
        let theme = ctx.theme;
        let k = super::render::label_magnification(theme, ctx.text);
        let side = FACE * k;
        let step = side * FACE_STEP;
        let pad = PAD * k;
        let ring = 2.0 * f64::from(theme.tokens.metrics.chrome_rule_px);
        let style = super::render::pill_label_style(theme);
        let name = self.placed_name(first, given.as_deref());
        let label = match members.len() {
            1 => name,
            many => format!("{name} +{}", many - 1),
        };
        let (text_w, text_h) = ctx.text.measure_style(&label, None, &style);
        let height = side + 2.0 * pad;
        let behind = step * (members.len() - 1) as f64;
        let dock = dock + vello::kurbo::Vec2::new(behind, 0.0);
        let mut pill = Rect::new(
            dock.x - side / 2.0 - pad,
            dock.y - height / 2.0,
            dock.x + side / 2.0 + 2.0 * pad + f64::from(text_w) + LABEL_END * k,
            dock.y + height / 2.0,
        );
        let hindmost = Rect::from_center_size((dock.x - behind, dock.y), (side, side));
        let whole = pill.union(hindmost);
        let inside = viewport.inset(-ring);
        let shift = vello::kurbo::Vec2::new(
            (inside.x1 - whole.x1).min(0.0).max(inside.x0 - whole.x0),
            (inside.y1 - whole.y1).min(0.0).max(inside.y0 - whole.y0),
        );
        pill = pill + shift;
        let dock = dock + shift;
        let radius = height / 2.0;
        let colour = super::render::color_for(self, first, theme);
        let outer = pill.inflate(ring, ring);
        crate::surface::shadow(
            ctx.scene,
            ctx.transform,
            outer,
            radius + ring,
            &theme.tokens.elevation.contact,
        );
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            ground,
            None,
            &RoundedRect::from_rect(outer, radius + ring),
        );
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            colour,
            None,
            &RoundedRect::from_rect(pill, radius),
        );
        // The first face is drawn last, over the ones set behind it.
        for (index, (member, given)) in members.iter().enumerate().rev() {
            let name = self.placed_name(member, given.as_deref());
            let face = crate::widget::AvatarFace::member(&name, member)
                .presence(super::presence_colour(self.color_for(member)))
                .ringed_on(ground);
            let centre = Point::new(dock.x - step * index as f64, dock.y);
            face.paint(ctx, Rect::from_center_size(centre, (side, side)));
        }
        let mut painted = outer.union((hindmost + shift).inflate(ring, ring));
        let text_x = dock.x + side / 2.0 + 2.0 * pad;
        let text_y = pill.center().y - f64::from(text_h) / 2.0;
        ctx.text.draw_style(
            ctx.scene,
            &label,
            super::render::contrast_ink(colour),
            ctx.transform * vello::kurbo::Affine::translate((text_x, text_y)),
            None,
            &style,
        );
        if followed {
            let around = outer.inflate(FOLLOW_GAP, FOLLOW_GAP);
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(FOLLOW_RING),
                ctx.transform,
                colour,
                None,
                &RoundedRect::from_rect(around, radius + ring + FOLLOW_GAP),
            );
            painted = painted.union(around.inflate(FOLLOW_RING, FOLLOW_RING));
        }
        painted
    }
}

/// A placed tag's face, at 1x.
const FACE: f64 = 18.0;
/// How far each further face is set left of the one before, as a share of a face: past the
/// pill's end, so three quarters of it shows.
const FACE_STEP: f64 = 0.75;
/// The pill round its faces, at 1x.
const PAD: f64 = 3.0;
/// Padding after the name, at 1x.
const LABEL_END: f64 = 6.0;
/// The ring round a followed member's pill, and the ground between them.
const FOLLOW_RING: f64 = 2.0;
const FOLLOW_GAP: f64 = 2.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(user: &str) -> PlacedPointer {
        PlacedPointer {
            user: MemberId::person(user),
            at: (40.0, 50.0),
            name: None,
            activity: Some("Reading the notes".into()),
            chat: None,
            reaction: None,
        }
    }

    tilo_test_env::scenario! {
        fn a_placed_pointer_is_named_and_coloured_as_presence_says;

        phase a_placed_pointer_wears_the_persons_colour_and_name;
        // The pointer wears the colour and name presence gives the person, so
        // it reads as the same person as their avatar.
        {
            let mut presence = PresenceCtx::empty();
            let ada = MemberId::person("ada-device");
            presence.set_display_name(ada.clone(), "Ada");
            presence.set_color_override(ada.clone(), "#38b8f6");
            let cursor = presence.placed_cursor(placed("ada-device"));
            assert_eq!(
                cursor.label, "Ada",
                "a person's activity is not theirs to show"
            );
            assert_eq!(cursor.color, Color::from_rgb8(0x38, 0xb8, 0xf6));
            assert_eq!(cursor.pos, (40.0, 50.0));
            assert!(!cursor.is_agent);
        }

        phase an_agent_says_what_it_does_and_an_unnamed_member_is_not_a_raw_key;
        {
            let mut presence = PresenceCtx::empty();
            let bot = MemberId::agent("bot", "ada-device");
            presence.set_display_name(bot.clone(), "Scribe");
            let cursor = presence.placed_cursor(PlacedPointer {
                user: bot,
                ..placed("bot")
            });
            assert_eq!(cursor.label, "Scribe · Reading the notes");
            assert!(cursor.is_agent);

            let key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
            let stranger = presence.placed_cursor(placed(key));
            assert_eq!(
                stranger.label, "Someone without a name yet",
                "a person nobody named is someone without a name yet"
            );
            let keyed = presence.placed_cursor(PlacedPointer {
                name: Some(key.into()),
                ..placed(key)
            });
            assert_eq!(
                keyed.label, "Someone without a name yet",
                "a key their presence says is no name"
            );
            let named = presence.placed_cursor(PlacedPointer {
                user: MemberId::agent(key, "ada-device"),
                name: Some("Ada".into()),
                ..placed(key)
            });
            assert_eq!(
                named.label, "Ada · Reading the notes",
                "until the workspace names them, by their own presence"
            );
            assert!(named.is_agent);
            assert_eq!(
                stranger.color,
                {
                    let (r, g, b) = super::super::user_color(key);
                    Color::from_rgb8(r, g, b)
                },
                "without a colour from the workspace, the identity's own"
            );
            presence.set_display_name(MemberId::person(key), key);
            assert_eq!(
                presence.display_name(&MemberId::person(key)),
                "Someone without a name yet",
                "nor is a key the roster says"
            );
        }
    }
}
