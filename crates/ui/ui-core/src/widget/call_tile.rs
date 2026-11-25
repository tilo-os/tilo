//! `CallTile`: one person in a call at their camera or the screen they share. It draws the newest
//! picture the call delivered of them, and their face until one arrives, so a call shows who is on
//! camera and what is on their screen rather than saying it in words.
//!
//! The pictures are the call's, decoded where the window draws them and kept by address
//! (`tilo-live://call/<person>/<camera|screen>`): a tile is how a view asks for one, and the
//! device's call sends the layers the tiles show and nothing else.

use std::collections::HashMap;

use taffy::style_helpers::{auto, percent};
use taffy::{NodeId, Size as TaffySize, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Fill, ImageData as PenikoImage};

use crate::component::{Component, Described, Person};
use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::pictures::Pictures;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{AvatarFace, PictureSource, TaffyTreeExt, Widget};

/// Which of a person's pictures a call tile shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CallSource {
    #[default]
    Camera,
    Screen,
}

crate::component::choice!(CallSource, "CallSource", {
    Camera => "camera",
    Screen => "screen",
});

impl CallSource {
    /// Width over height a tile of it takes: a camera's frame, or a screen's.
    #[must_use]
    pub const fn ratio(self) -> f64 {
        match self {
            Self::Camera => 16.0 / 9.0,
            Self::Screen => 16.0 / 10.0,
        }
    }

    #[cfg(feature = "model")]
    /// The call's own name for this source.
    #[must_use]
    pub const fn of_call(self) -> crate::native_media::PictureSource {
        match self {
            Self::Camera => crate::native_media::PictureSource::Camera,
            Self::Screen => crate::native_media::PictureSource::Screen,
        }
    }

    #[cfg(feature = "model")]
    /// The tile's own name for one of the call's sources.
    #[must_use]
    pub const fn of_picture(source: crate::native_media::PictureSource) -> Self {
        match source {
            crate::native_media::PictureSource::Camera => Self::Camera,
            crate::native_media::PictureSource::Screen => Self::Screen,
        }
    }
}

/// The live slot a call's newest picture of `person`'s `source` is drawn from: one address,
/// published by whichever host decodes the pictures and read by every tile that shows it.
#[must_use]
pub fn call_slot(person: &str, source: CallSource) -> String {
    format!(
        "call/{person}/{}",
        match source {
            CallSource::Camera => "camera",
            CallSource::Screen => "screen",
        }
    )
}

/// The address a tile shows the newest picture of `person`'s `source` by
/// ([`call_slot`]).
#[must_use]
pub fn call_address(person: &str, source: CallSource) -> String {
    crate::live_pictures::address(&call_slot(person, source))
}

/// A person in a call at their camera or screen.
#[derive(Clone, Component, Debug, Default)]
#[component(name = "CallTile", role = "Image", label = "name")]
pub struct CallTileProps {
    /// The person the tile shows, and their face until their picture arrives.
    pub who: Option<Person>,
    /// What it shows of them: their camera, or the screen they share.
    pub source: Option<CallSource>,
    /// What it is called where `who` names nobody the workspace knows (someone a bridge carries
    /// in).
    pub name: Option<String>,
    /// They are heard talking now: their face wears the speaking ring.
    pub speaking: bool,
    /// Their microphone is off.
    pub muted: bool,
    /// The window's pictures, which load the frames; a host hands them in.
    #[component(skip)]
    pub pictures: Option<Pictures>,
}

impl Described for CallTile {
    type Props = CallTileProps;

    fn from_props(_key: &str, props: CallTileProps) -> Self {
        let CallTileProps {
            who,
            source,
            name,
            speaking,
            muted,
            pictures,
        } = props;
        let pictures = pictures.unwrap_or_else(Pictures::window);
        let mut tile = Self::new(who, source.unwrap_or_default(), &pictures);
        tile.speaking = speaking;
        tile.muted = muted;
        if let Some(name) = name.filter(|name| !name.is_empty()) {
            tile.name = name;
        }
        tile
    }

    fn set_props(&mut self, props: CallTileProps) -> bool {
        let CallTileProps {
            who,
            source,
            name,
            speaking,
            muted,
            pictures,
        } = props;
        let pictures = pictures.unwrap_or_else(Pictures::window);
        let source = source.unwrap_or_default();
        let name = name.filter(|name| !name.is_empty()).unwrap_or_else(|| {
            who.as_ref()
                .map_or_else(String::new, |who| who.name.clone())
        });
        let identity = who
            .as_ref()
            .map_or_else(String::new, |who| who.identity.id().to_owned());
        let changed = who != self.who
            || source != self.source
            || name != self.name
            || identity != self.identity
            || speaking != self.speaking
            || muted != self.muted;
        self.pictures = pictures;
        self.who = who;
        self.source = source;
        self.name = name;
        self.identity = identity;
        self.speaking = speaking;
        self.muted = muted;
        if changed {
            self.dirty = true;
            self.refresh_picture();
        }
        changed
    }
}

pub struct CallTile {
    who: Option<Person>,
    /// What the face says, when `who` names nobody (someone a bridge carries in).
    name: String,
    /// The colour the face is drawn in when `who` is none.
    identity: String,
    source: CallSource,
    speaking: bool,
    muted: bool,
    /// The live address this tile shows, once it knows the person.
    picture: Option<PictureSource>,
    pictures: Pictures,
    /// The picture a still address showed last, kept while the address stays.
    image: Option<PenikoImage>,
    rect: Rect,
    dirty: bool,
}

impl CallTile {
    /// The tile of `who` at their `source`, its frames read through `pictures`.
    #[must_use]
    pub fn new(who: Option<Person>, source: CallSource, pictures: &Pictures) -> Self {
        let name = who
            .as_ref()
            .map_or_else(String::new, |who| who.name.clone());
        let identity = who
            .as_ref()
            .map_or_else(String::new, |who| who.identity.id().to_owned());
        let mut tile = Self {
            who,
            name,
            identity,
            source,
            speaking: false,
            muted: false,
            picture: None,
            pictures: pictures.clone(),
            image: None,
            rect: Rect::ZERO,
            dirty: true,
        };
        tile.refresh_picture();
        tile
    }

    /// The tile of `person` at their `source`, through the window's pictures.
    #[must_use]
    pub fn of(person: Person, source: CallSource) -> Self {
        Self::new(Some(person), source, &Pictures::window())
    }

    /// Whether the person is heard talking now.
    #[must_use]
    pub const fn speaking(&self) -> bool {
        self.speaking
    }

    /// Their microphone is off.
    #[must_use]
    pub const fn muted(&self) -> bool {
        self.muted
    }

    /// The address the tile shows its newest frame by: the person's picture in the call, once
    /// they are one of the workspace's people.
    #[must_use]
    pub fn address(&self) -> Option<&str> {
        self.picture.as_ref().map(|picture| picture.src.as_str())
    }

    /// The person the tile is of, as a person of the workspace.
    fn person_id(&self) -> Option<&str> {
        let who = self.who.as_ref()?;
        who.identity
            .member_id()
            .and_then(tilo_actions::MemberId::person_id)
    }

    fn refresh_picture(&mut self) {
        self.picture = self.person_id().map(|person| PictureSource {
            src: call_address(person, self.source),
        });
        self.image = None;
    }

    /// The newest frame, if the call delivered one: a live address is asked for again on every
    /// draw, so a window woken after a frame arrived paints it rather than the one before.
    fn picture_now(&self) -> Option<PenikoImage> {
        let picture = self.picture.as_ref()?;
        self.pictures.get(picture)
    }

    /// What the tile shows a reader: who it is, and what they show there.
    fn spoken(&self) -> String {
        let name = if self.name.is_empty() {
            crate::i18n::widget_text("call-tile-someone")
        } else {
            self.name.clone()
        };
        let mut args = fluent::FluentArgs::new();
        args.set("name", name);
        let key = match self.source {
            CallSource::Camera => "call-tile-on-camera",
            CallSource::Screen => "call-tile-sharing-screen",
        };
        crate::i18n::widget_text_with(key, Some(&args))
    }

    fn paint_name(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.name.is_empty() {
            return;
        }
        let style = theme.text(TextRole::Caption);
        let spacing = &theme.tokens.spacing;
        let inset = f64::from(spacing.s2);
        let pad = inset * 1.5;
        let gap = inset;
        let glyph = if self.muted {
            Some(Icon::MicOff)
        } else if self.source == CallSource::Screen {
            Some(Icon::ScreenShare)
        } else {
            None
        };
        let glyph_side = if glyph.is_some() {
            f64::from(theme.tokens.metrics.icon_sm)
        } else {
            0.0
        };
        let glyph_gap = if glyph.is_some() { gap } else { 0.0 };
        let available =
            (self.rect.width() - inset * 2.0 - pad * 2.0 - glyph_side - glyph_gap).max(0.0);
        let label =
            text.elide_to_width_style(&self.name, crate::num::f32_of_f64(available), &style);
        let (width, line) = text.measure_style(&label, None, &style);
        if label.is_empty() {
            return;
        }
        let pill = Rect::from_origin_size(
            (
                self.rect.x0 + inset,
                self.rect.y1 - inset - f64::from(line) - inset,
            ),
            (
                (pad * 2.0 + glyph_side + glyph_gap + f64::from(width))
                    .min((self.rect.width() - inset * 2.0).max(0.0)),
                f64::from(line) + inset,
            ),
        );
        if pill.width() <= 1.0 || pill.height() <= 1.0 {
            return;
        }
        let radius = f64::from(theme.tokens.radii.full).min(pill.height() / 2.0);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.overlay,
            None,
            &RoundedRect::from_rect(pill, radius),
        );
        let ink = theme.text_primary;
        let cy = pill.center().y;
        let mut x = pill.x0 + pad;
        if let Some(glyph) = glyph {
            let slot = Rect::from_center_size(
                vello::kurbo::Point::new(x + glyph_side / 2.0, cy),
                (glyph_side, glyph_side),
            );
            glyph.draw(scene, transform, slot, ink);
            x += glyph_side + glyph_gap;
        }
        text.draw_style(
            scene,
            &label,
            ink,
            transform * Affine::translate((x, cy - f64::from(line) / 2.0)),
            None,
            &style,
        );
    }
}

impl Widget for CallTile {
    crate::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: TaffySize {
                width: percent(1.0_f32),
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: TaffySize<Option<f32>>,
        available: TaffySize<taffy::AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> TaffySize<f32> {
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(width) => width,
            _ => 240.0,
        });
        let width = width.max(1.0);
        #[allow(clippy::cast_possible_truncation, reason = "a widget width fits f32")]
        let height = (f64::from(width) / self.source.ratio()) as f32;
        TaffySize { width, height }
    }

    crate::rect_geometry!();

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        let rect = self.rect;
        if rect.width() <= 1.0 || rect.height() <= 1.0 {
            return;
        }
        let radius = f64::from(theme.tokens.radii.item);
        let clip = RoundedRect::from_rect(rect, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &clip,
        );
        let image = self.picture_now();
        let mut painted = false;
        if let Some(image) = &image {
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);
            crate::image_store::paint_image_cover(scene, transform, rect, image, radius);
            scene.pop_layer();
            painted = true;
        }
        if !painted {
            let side = (rect.width().min(rect.height()) * 0.4).clamp(24.0, 160.0);
            let face_rect = Rect::from_center_size(rect.center(), (side, side));
            let speaking = if self.speaking { 1.0 } else { 0.0 };
            let mut paint = PaintCtx::new(scene, transform, text, theme);
            let face = match &self.who {
                Some(who) => AvatarFace::of(who),
                None => AvatarFace::new(&self.name, &self.identity),
            };
            face.speaking(speaking).paint(&mut paint, face_rect);
        }
        if self.speaking {
            scene.stroke(
                &Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px) * 2.0),
                transform,
                theme.tokens.colors.intent.accent,
                None,
                &clip,
            );
        }
        self.paint_name(scene, transform, text, theme);
        self.dirty = false;
    }

    fn needs_redraw(&self) -> bool {
        // A live frame is asked for on every draw: a tile that shows one repaints with the
        // window, so the picture moves on the frame after it arrived. A tile nobody sends
        // frames to (a page decodes them itself) repaints only when it changed.
        self.dirty || self.picture_now().is_some()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "CallTile"
    }

    /// The address of the frame the tile shows: a host that decodes the call's stream draws it
    /// here itself, and never a still of the tile.
    fn live_picture(&self) -> Option<String> {
        self.address().map(str::to_owned)
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        let mut node = AccessNode::new(Role::Image).with_name(self.spoken());
        // The person's name alone, as the tile shows it beside their picture: what a page writes
        // in its caption, where the words after it are for a reader alone.
        if !self.name.is_empty() {
            node = node.with_title(self.name.clone());
        }
        Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::Driver;
    use crate::widget::Widget;

    tilo_test_env::scenario! {
        fn a_call_tile_shows_the_newest_picture_of_who_it_is_of;

        phase a_tile_reads_its_person_source_and_state_from_its_props;
        {
            let person = Person::new(
                tilo_actions::MemberId::person("ab".repeat(32)),
                "Ada",
            );
            let mut node = crate::component::testing::Node::new(
                "CallTile",
                "ada",
                &crate::Intents::default(),
            )
            .prop(
                "who",
                crate::component::Value::Actor {
                    identity: person.identity.clone(),
                    name: person.name.clone(),
                    hue: None,
                    picture: None,
                },
            )
            .prop("source", crate::component::Value::Choice("screen".to_owned()))
            .prop("speaking", crate::component::Value::Bool(true));
            let props = CallTileProps::from_source(&mut node).expect("the props read");
            let tile = CallTile::from_props("ada", props);
            assert_eq!(tile.source, CallSource::Screen);
            assert!(tile.speaking);
            assert_eq!(
                tile.address(),
                Some(format!("tilo-live://call/{}/screen", "ab".repeat(32)).as_str())
            );
            assert_eq!(
                tile.accessibility().and_then(|node| node.name).as_deref(),
                Some("Ada, sharing their screen")
            );
            let access = tile.accessibility().expect("the tile says what it is");
            assert_eq!(
                access.title.as_deref(),
                Some("Ada"),
                "the player's name alone is the tile's caption"
            );
            let address = format!("tilo-live://call/{}/screen", "ab".repeat(32));
            assert_eq!(
                tile.live_picture().as_deref(),
                Some(address.as_str()),
                "a host that decodes the call's stream draws it where the tile stands"
            );
        }

        phase a_tile_draws_the_frame_published_for_its_slot;
        {
            let person =
                Person::new(tilo_actions::MemberId::person("cd".repeat(32)), "Grace");
            let tile = CallTile::of(person, CallSource::Camera);
            let frame = vec![200_u8; 4 * 2 * 4];
            crate::live_pictures::publish(
                &call_slot(&"cd".repeat(32), CallSource::Camera),
                1,
                4,
                2,
                std::sync::Arc::new(frame),
            );
            assert!(
                tile.picture_now().is_some(),
                "the tile reads the frame published for its slot"
            );
            crate::live_pictures::forget(&call_slot(&"cd".repeat(32), CallSource::Camera));
            assert!(tile.picture_now().is_none(), "a tile with no frame shows the face");
        }

        phase a_window_with_a_tile_lays_it_out_at_its_ratio;
        {
            let person = Person::new(tilo_actions::MemberId::person("ef".repeat(32)), "Lin");
            let tile = CallTile::of(person, CallSource::Camera);
            let harness = Driver::new(crate::State::new(), Box::new(tile), (640, 360));
            let root = harness.root_widget();
            let rect = root.get_rect();
            assert!(rect.width() > 1.0, "the tile takes the window's width");
            assert!(
                (rect.height() - rect.width() / (16.0 / 9.0)).abs() <= 1.0,
                "a camera tile is laid out at its frame's ratio, got {}x{}",
                rect.width(),
                rect.height()
            );
        }
    }
}
