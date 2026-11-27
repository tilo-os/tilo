//! Avatar: a person's (or an account's) picture, or their initials when no picture exists.

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Point, Rect};
use vello::peniko::{self, Color};

use tilo_ui_base::ImageStore;
use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::widget::Widget;

#[cfg(feature = "local-daemons")]
use tilo_ui_base::widget::avatar_face::profile_image;
use tilo_ui_base::{
    widget::avatar_face::AvatarFace, widget::avatar_face::AvatarSize,
    widget::avatar_face::PresenceStatus,
};

enum Picture {
    None,
    /// Resolved from the store on `sync`, so a picture that finishes loading
    /// repaints the avatar without rebuilding the tree.
    Lazy {
        store: ImageStore,
        url: String,
    },
}

pub struct Avatar {
    name: String,
    name_state: Option<tilo_ui_base::local_state::LocalBinding<String>>,
    identity: String,
    presence: Option<Color>,
    /// The member it shows, whose kind and hue pick the face ([`AvatarFace::of`]).
    who: Option<tilo_ui_base::component::Person>,
    picture: Picture,
    image: Option<peniko::ImageData>,
    size: AvatarSize,
    square: bool,
    neutral: bool,
    status: Option<PresenceStatus>,
    glyph: Option<tilo_ui_base::icons::Icon>,
    side: f64,
    rect: Rect,
    dirty: bool,
    /// Only the place it takes, shown empty: see [`Self::placeholder`].
    placeholder: bool,
    /// They are in a call now: an arc sweeps round their face while they are.
    call: bool,
    /// When the call was first drawn, so the sweep reads from where it began.
    call_since: Option<crate::time::Instant>,
    /// Whether this theme's motion loops at all: a face whose sweep is still asks for no frame.
    sweeping: bool,
}

impl Avatar {
    pub fn person(name: impl Into<String>, identity: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            name_state: None,
            identity: identity.into(),
            presence: None,
            who: None,
            picture: Picture::None,
            image: None,
            size: AvatarSize::Regular,
            square: false,
            neutral: false,
            status: None,
            glyph: None,
            side: 0.0,
            rect: Rect::ZERO,
            dirty: true,
            placeholder: false,
            call: false,
            call_since: None,
            sweeping: false,
        }
    }

    #[must_use]
    pub fn placeholder(mut self) -> Self {
        self.placeholder = true;
        self
    }

    #[must_use]
    pub fn with_glyph(mut self, glyph: tilo_ui_base::icons::Icon) -> Self {
        self.glyph = Some(glyph);
        self.neutral = true;
        self.square = true;
        self
    }

    /// `who`'s face as every surface shows it ([`AvatarFace::of`]), showing the picture they
    /// set where they set one.
    #[must_use]
    pub fn of(who: tilo_ui_base::component::Person) -> Self {
        let mut avatar = Self::person(who.name.clone(), who.identity.id());
        if let Some(picture) = who.picture.clone().filter(|picture| !picture.is_empty()) {
            avatar = avatar.with_picture_url(picture);
        }
        avatar.who = Some(who);
        avatar
    }

    #[must_use]
    pub fn with_name_state(mut self, state: &tilo_ui_base::LocalState<String>) -> Self {
        let (name, binding) = tilo_ui_base::local_state::LocalBinding::new(state);
        self.name = name;
        self.name_state = Some(binding);
        self.dirty = true;
        self
    }

    /// Take the picture's bytes once the window's pictures have them, through the same reader
    /// `ui.Image` uses: a content address the host holds, or a picture a `data:` address carries.
    fn follow_picture(&mut self) -> bool {
        if self.image.is_some() {
            return false;
        }
        let Picture::Lazy { store, url } = &self.picture else {
            return false;
        };
        // The store the avatar was built with first (a person's own picture), then the window's
        // pictures, which also read a `data:` address or a content address a record carries.
        let image = store.get(url).or_else(|| {
            let picture = crate::widget::PictureSource { src: url.clone() };
            crate::pictures::Pictures::window().get(&picture)
        });
        let Some(image) = image else {
            return false;
        };
        self.image = Some(image);
        self.dirty = true;
        true
    }

    fn refresh_name(&mut self) -> bool {
        if let Some(binding) = &mut self.name_state
            && let Some(name) = binding.poll()
            && self.name != name
        {
            self.name = name;
            self.dirty = true;
            return true;
        }
        false
    }

    #[must_use]
    pub fn with_presence(mut self, colour: Color) -> Self {
        self.presence = Some(colour);
        self
    }

    #[cfg(feature = "local-daemons")]
    #[must_use]
    pub fn with_profile_photo(self, photo: &tilo_identity_client::ProfilePhoto) -> Self {
        self.with_image(profile_image(photo.rgba(), photo.width(), photo.height()))
    }

    #[must_use]
    pub fn with_image(mut self, image: peniko::ImageData) -> Self {
        self.picture = Picture::None;
        self.image = Some(image);
        self
    }

    /// The person's picture, loaded from `store` in the background. The
    /// initials show until it arrives; an empty `url` keeps them.
    #[must_use]
    pub fn with_image_url(mut self, store: ImageStore, url: impl Into<String>) -> Self {
        self.picture = Picture::Lazy {
            store,
            url: url.into(),
        };
        self.image = None;
        self
    }

    /// The picture at `url`, where the window's own pictures read it: a content address the
    /// host holds, or a `data:` address a record carries. The initials show until it arrives.
    #[must_use]
    pub fn with_picture_url(self, url: impl Into<String>) -> Self {
        self.with_image_url(crate::pictures::Pictures::window().store().clone(), url)
    }

    #[must_use]
    pub fn size(mut self, size: AvatarSize) -> Self {
        self.size = size;
        self
    }

    #[must_use]
    pub fn square(mut self) -> Self {
        self.square = true;
        self
    }

    #[must_use]
    pub fn neutral(mut self) -> Self {
        self.neutral = true;
        self.square = true;
        self
    }

    #[must_use]
    pub fn status(mut self, status: PresenceStatus) -> Self {
        self.status = Some(status);
        self
    }

    pub fn set_status(&mut self, status: Option<PresenceStatus>) {
        if status != self.status {
            self.status = status;
            self.dirty = true;
        }
    }

    #[must_use]
    pub fn initials(&self) -> String {
        self.face().initials()
    }

    fn face(&self) -> AvatarFace<'_> {
        let mut face = match &self.who {
            Some(who) => AvatarFace::of(who),
            None => AvatarFace::new(&self.name, &self.identity),
        }
        .image(self.image.as_ref());
        // A glyph draws on the neutral square unless a colour is given: the colour comes after
        // it, so an agent's mark wears its client's hue.
        if let Some(glyph) = self.glyph {
            face = face.glyph(glyph);
        }
        if let Some(colour) = self.presence {
            face = face.presence(colour);
        }
        if self.square {
            face = face.square();
        }
        if self.neutral {
            face = face.neutral();
        }
        if let Some(status) = self.status {
            face = face.status(status);
        }
        face
    }
}

impl Widget for Avatar {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn sync(
        &mut self,
        _state: &tilo_ui_base::widget::AppModel,
        _path: &tilo_ui_base::widget::ModelPath,
    ) {
        self.refresh_name();
        self.follow_picture();
    }

    fn update(
        &mut self,
        _state: &tilo_ui_base::widget::AppModel,
        _path: &tilo_ui_base::widget::ModelPath,
    ) -> bool {
        let renamed = self.refresh_name();
        let followed = self.follow_picture();
        renamed || followed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.side = self.size.side(theme);
        let side = tilo_ui_base::num::f32_of_f64(self.side);
        let style = Style {
            size: Size::from_lengths(side, side),
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        if rect != self.rect {
            self.dirty = true;
        }
        self.rect = rect;
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if self.placeholder || env.should_cull(self.rect) {
            return;
        }
        // Someone in a call wears the arc sweeping round their face, from where the call began.
        let sweeping = if self.call {
            let now = crate::time::now();
            let since = *self.call_since.get_or_insert(now);
            let motion = theme.motion();
            self.sweeping = motion.loops();
            Some(tilo_ui_base::motion::Sweep::at(
                now.saturating_duration_since(since),
                motion,
            ))
        } else {
            self.call_since = None;
            self.sweeping = false;
            None
        };
        let face = match sweeping {
            Some(sweep) => self.face().active(sweep),
            None => self.face(),
        };
        face.paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
        );
    }

    fn needs_redraw(&self) -> bool {
        // A picture whose bytes are on their way asks for the frames that read them in; a dirty
        // avatar draws, as every widget's does, and a face in a call keeps drawing while its arc
        // sweeps.
        self.dirty
            || (self.image.is_none() && matches!(self.picture, Picture::Lazy { .. }))
            || (self.call && self.sweeping)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    /// Keyed off the laid-out rect, never a draw-time cache: an avatar
    /// culled off-screen is never drawn, and must not report a pending
    /// measure on every scroll frame because of it.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "Avatar"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::Role};
        if self.placeholder {
            return None;
        }
        let name = (!self.name.trim().is_empty()).then(|| match self.status {
            Some(status) => format!("{}, {}", self.name, status.spoken()),
            None => self.name.clone(),
        });
        Some(AccessNode::new(Role::Image).with_optional_name(name))
    }
}

/// What an avatar follows and shows, for a test outside this crate.
#[cfg(any(test, feature = "test-support"))]
impl Avatar {
    /// The name the avatar shows, after a bound name state last changed it.
    pub fn name_for_tests(&self) -> &str {
        &self.name
    }

    /// Whether a picture's bytes are read and drawn instead of the initials.
    pub fn has_image_for_tests(&self) -> bool {
        self.image.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_ui_base::presence::PresenceCtx;
    use tilo_ui_base::text_render::TextRenderer;
    use tilo_ui_base::tokens::colors::PresenceHue;
    use tilo_ui_base::{widget::avatar_face::initial_ink, widget::avatar_face::initials_for_name};
    use vello::Scene;
    use vello::kurbo::Affine;
    use vello::peniko::ImageData as PenikoImage;
    use vello_encoding::DrawTag;

    fn image(width: u32, height: u32) -> PenikoImage {
        let pixels = usize::try_from(width * height * 4).unwrap();
        crate::raster::rgba8(vec![200; pixels], width, height)
    }

    fn paint_face(face: AvatarFace<'_>, theme: &Theme) -> Scene {
        let mut text = TextRenderer::new();
        let mut scene = Scene::new();
        face.paint(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, theme),
            Rect::new(0.0, 0.0, 40.0, 40.0),
        );
        scene
    }

    /// What a face paints, to tell two faces apart.
    fn drawn(scene: &Scene) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
        let encoding = scene.encoding();
        let tags = encoding.draw_tags.iter().map(|tag| tag.0).collect();
        (tags, encoding.draw_data.clone(), encoding.path_data.clone())
    }

    /// A face in a call wears the arc sweeping round it: the ring is not the quiet face's, it
    /// moves as the sweep turns, and where motion is reduced the whole ring is lit and still.
    #[test]
    fn a_face_in_a_call_wears_the_sweeping_ring() {
        use tilo_ui_base::motion::Sweep;
        let theme = Theme::dark();
        let quiet = drawn(&paint_face(AvatarFace::new("Ada", "ada"), &theme));
        let early = drawn(&paint_face(
            AvatarFace::new("Ada", "ada").active(Sweep::Round(0.125)),
            &theme,
        ));
        let later = drawn(&paint_face(
            AvatarFace::new("Ada", "ada").active(Sweep::Round(0.625)),
            &theme,
        ));
        let still = drawn(&paint_face(
            AvatarFace::new("Ada", "ada").active(Sweep::Still),
            &theme,
        ));
        assert_ne!(
            quiet, early,
            "a face in a call wears a ring the quiet face does not"
        );
        assert_ne!(early, later, "the arc moves as the sweep turns");
        assert_ne!(early, still, "with motion reduced the whole ring is lit");
    }

    #[test]
    fn a_member_wears_one_face_on_every_surface() {
        use tilo_ui_base::{component::Described, component::Person};
        let theme = Theme::dark();
        let agent = Person::new(tilo_actions::MemberId::agent("helper", "ada"), "Helper");
        let mark = drawn(&paint_face(
            AvatarFace::new("Helper", "helper").hue(agent.hue).agent(),
            &theme,
        ));
        let initials = drawn(&paint_face(AvatarFace::new("Helper", "helper"), &theme));
        assert_ne!(mark, initials, "the agent mark is not a letter");
        assert_eq!(drawn(&paint_face(AvatarFace::of(&agent), &theme)), mark);
        // A described avatar: a history row's, a tilolang `ui.Avatar`.
        let props = AvatarProps {
            who: Some(agent.clone()),
            ..AvatarProps::default()
        };
        let described = Avatar::from_props("who", props);
        assert_eq!(drawn(&paint_face(described.face(), &theme)), mark);
        // A described avatar that names a picture shows it instead of the initials, read through
        // the window's pictures as `ui.Image` reads one: the same prop a contact's photo takes.
        let props = AvatarProps {
            name: Some("Ada".into()),
            picture: Some(crate::pictures::FIXTURE_PICTURE.into()),
            ..AvatarProps::default()
        };
        let mut pictured = Avatar::from_props("name", props);
        let initials = drawn(&paint_face(pictured.face(), &theme));
        assert!(
            pictured.image.is_none(),
            "the picture's bytes are read as it syncs"
        );
        assert!(
            pictured.follow_picture(),
            "the picture in its address is read"
        );
        assert!(pictured.image.is_some(), "the picture's bytes are here");
        assert_ne!(
            drawn(&paint_face(pictured.face(), &theme)),
            initials,
            "a picture is not the initials of the same name"
        );
        // A member the workspace pictured shows that picture through the actor a view is
        // handed, as a roster's row names them: an author's face.
        let pictured_member = Person {
            picture: Some(crate::pictures::FIXTURE_PICTURE.into()),
            ..Person::new(tilo_actions::MemberId::person("ada"), "Ada")
        };
        let mut author = Avatar::of(pictured_member);
        assert!(
            author.follow_picture(),
            "the roster's picture is read where the author's face is drawn"
        );
        assert!(author.image.is_some());
        // A bot wears its own picture or initials on the rounded square.
        let bot = Person::new(tilo_actions::MemberId::bot("greeter"), "Greeter");
        assert!(AvatarFace::of(&bot).is_square());
        let person = Person::new(tilo_actions::MemberId::person("ada"), "Ada");
        assert!(!AvatarFace::of(&person).is_square());
    }

    fn tags(scene: &Scene, tag: DrawTag) -> usize {
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == tag)
            .count()
    }

    #[cfg(feature = "local-daemons")]
    #[cfg(feature = "local-daemons")]
    #[test]
    fn explicit_canonical_photo_is_drawn_by_shared_avatar() {
        let photo = tilo_identity_client::ProfilePhoto::new(1, 1, vec![200, 100, 50, 255]).unwrap();
        let mut avatar = Avatar::person("Ada", "ada").with_profile_photo(&photo);
        let mut tree = TaffyTree::new();
        let mut map = std::collections::HashMap::new();
        let theme = Theme::dark();
        let node = avatar.build_layout(&mut tree, &theme, &mut map);
        tree.compute_layout(
            node,
            <taffy::Size<taffy::AvailableSpace> as taffy::prelude::TaffyMaxContent>::MAX_CONTENT,
        )
        .unwrap();
        avatar.apply_layout(&tree, node, Point::ZERO);
        let mut scene = Scene::new();
        avatar.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            PresenceCtx::empty().draw_env(),
        ));
        assert_eq!(tags(&scene, DrawTag::IMAGE), 1);
        assert_eq!(
            Widget::accessibility(&avatar).unwrap().name.as_deref(),
            Some("Ada")
        );
    }

    tilo_test_env::scenario! {
        /// Avatar initials and their ink.
        /// Each phase was a test of its own and builds its own fixture.
        fn initials_come_from_the_name_and_read_on_every_fill;

        phase initials_come_from_the_name_and_colour_from_the_identity;
        {
            assert_eq!(initials_for_name("Ada Lovelace"), "AL");
            assert_eq!(initials_for_name("  grace   brewster hopper "), "GH");
            assert_eq!(initials_for_name("(Ops) team"), "OT");
            assert_eq!(initials_for_name(""), "");

            let theme = Theme::dark();
            let a = AvatarFace::new("Ada Lovelace", "ada@example.org").colour(&theme);
            let again = AvatarFace::new("A. Lovelace", "ada@example.org").colour(&theme);
            assert_eq!(a, again);
            let expected = theme
                .tokens
                .colors
                .presence
                .hue(PresenceHue::of_identity("ada@example.org"));
            assert_eq!(a, expected);
            let own = AvatarFace::new("Bob", "bob")
                .presence(Color::from_rgb8(0xe0, 0x60, 0x20))
                .colour(&theme);
            assert_eq!(
                own,
                theme
                    .tokens
                    .colors
                    .presence
                    .of(Color::from_rgb8(0xe0, 0x60, 0x20))
            );
        }

        phase initials_ink_reads_on_every_presence_hue_in_every_preset;
        {
            use tilo_ui_base::tokens::colors::ON_FILL_MIN_CONTRAST;
            use vello::peniko::Color;
            for theme in [Theme::dark(), Theme::light()] {
                for hue in PresenceHue::ALL {
                    let fill = theme.tokens.colors.presence.hue(hue);
                    let ink = initial_ink(fill, &theme);
                    let ratio = crate::color::contrast_ratio(ink, fill);
                    assert!(
                        ratio >= ON_FILL_MIN_CONTRAST,
                        "{}: initials on {hue:?} reach only {ratio:.2}:1",
                        theme.name
                    );
                }
                // The ink is chosen against the fill it is drawn on, through the fade an idle
                // face and the fill adjustment a bright hue take, and on fills a workspace or a
                // member's own colour puts under it: every one clears the on-fill floor.
                let palette_fills = PresenceHue::ALL.into_iter().flat_map(|hue| {
                    let fill = theme.tokens.colors.presence.hue(hue);
                    [
                        tilo_ui_base::widget::avatar_face::initials_fill(fill, &theme),
                        theme
                            .tokens
                            .colors
                            .presence
                            .idle_on(fill, theme.tokens.colors.surfaces.base),
                    ]
                });
                let other_fills = [
                    Color::from_rgb8(211, 163, 41),
                    Color::from_rgb8(0, 193, 209),
                    Color::from_rgb8(128, 128, 128),
                ];
                for fill in palette_fills.chain(other_fills) {
                    let ink = initial_ink(fill, &theme);
                    let ratio = crate::color::contrast_ratio(ink, fill);
                    assert!(
                        ratio >= ON_FILL_MIN_CONTRAST,
                        "{}: initials on {fill:?} reach only {ratio:.2}:1",
                        theme.name
                    );
                }
            }
        }

        phase initials_skip_words_without_letters;
        {
            assert_eq!(initials_for_name("Tilo 4"), "T");
            assert_eq!(initials_for_name("Ada 2"), "A");
            assert_eq!(initials_for_name("Pixel 7 Pro"), "PP");
            assert_eq!(initials_for_name("2024"), "");
        }
    }

    tilo_test_env::scenario! {
        /// What an avatar draws: glyph, picture, size, overflow disc and status badge.
        /// Each phase was a test of its own and builds its own fixture.
        fn an_avatar_paints_its_face_shape_and_badge;

        phase an_app_glyph_replaces_the_initials_on_the_neutral_square;
        {
            let theme = Theme::dark();
            // A nameless neutral tile paints only the tile.
            let blank = paint_face(AvatarFace::new("", "so.tilo.notes").neutral(), &theme);
            let glyph = paint_face(
                AvatarFace::new("", "so.tilo.notes").glyph(tilo_ui_base::icons::Icon::FileText),
                &theme,
            );
            assert!(
                tags(&glyph, DrawTag::COLOR) > tags(&blank, DrawTag::COLOR),
                "the glyph's strokes are painted on the tile"
            );
        }

        phase a_person_without_a_name_wears_the_person_glyph_never_initials;
        {
            let theme = Theme::dark();
            let person = tilo_actions::MemberId::person("aa");
            let blank = paint_face(AvatarFace::new("", "aa").neutral(), &theme);
            let glyph = paint_face(
                AvatarFace::new("", "aa").glyph(tilo_ui_base::icons::Icon::User),
                &theme,
            );
            for name in [
                String::new(),
                "ab".repeat(32),
                tilo_ui_base::presence::unnamed_person_label(),
                tilo_ui_base::presence::unnamed_person_short_label(),
                tilo_ui_base::presence::UNKNOWN_NAME.to_owned(),
            ] {
                let face = AvatarFace::member(&name, &person);
                assert_eq!(face.initials(), "", "no initials from {name:?}");
                let painted = paint_face(face, &theme);
                assert!(
                    tags(&painted, DrawTag::COLOR) > tags(&blank, DrawTag::COLOR),
                    "the person glyph is drawn for {name:?}"
                );
                assert_eq!(
                    tags(&painted, DrawTag::COLOR),
                    tags(&glyph, DrawTag::COLOR),
                    "the same glyph as everywhere for {name:?}"
                );
            }
            // A named person keeps their initials; an agent keeps its mark.
            assert_eq!(AvatarFace::new("Ada Lovelace", "aa").initials(), "AL");
            assert!(AvatarFace::new("", "aa").agent().initials().is_empty());
        }

        phase a_picture_replaces_the_initials_and_is_cover_fitted;
        {
            let theme = Theme::dark();
            let picture = image(64, 32);
            let with_image = paint_face(
                AvatarFace::new("Ada Lovelace", "ada").image(Some(&picture)),
                &theme,
            );
            assert_eq!(tags(&with_image, DrawTag::IMAGE), 1);
            assert_eq!(tags(&with_image, DrawTag::COLOR), 0, "no initials disc");
            let fallback = paint_face(AvatarFace::new("Ada Lovelace", "ada"), &theme);
            assert_eq!(tags(&fallback, DrawTag::IMAGE), 0);
            assert!(tags(&fallback, DrawTag::COLOR) >= 2, "disc and glyphs");
        }

        phase overflow_disc_paints_on_its_ground;
        {
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            let mut scene = Scene::new();
            AvatarFace::paint_overflow(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
                Rect::new(0.0, 0.0, 32.0, 32.0),
                4,
                Some(theme.tokens.colors.surfaces.content),
            );
            assert!(tags(&scene, DrawTag::COLOR) >= 3, "ring, disc, label");
            let mut empty = Scene::new();
            AvatarFace::paint_overflow(
                &mut PaintCtx::new(&mut empty, Affine::IDENTITY, &mut text, &theme),
                Rect::new(0.0, 0.0, 32.0, 32.0),
                0,
                Some(theme.tokens.colors.surfaces.content),
            );
            assert_eq!(tags(&empty, DrawTag::COLOR), 0);
        }

        phase a_status_badge_sits_on_the_corner_and_is_spoken;
        {
            let theme = Theme::dark();
            let face = AvatarFace::new("Ada Lovelace", "ada").status(PresenceStatus::Online);
            let rect = Rect::new(0.0, 0.0, 40.0, 40.0);
            let badge = face.status_rect(rect, &theme);
            assert!(badge.center().x > rect.center().x && badge.center().y > rect.center().y);
            assert!(badge.x1 <= rect.x1 + 0.5 && badge.y1 <= rect.y1 + 0.5);
            let mut text = TextRenderer::new();
            let mut with = Scene::new();
            face.paint(
                &mut PaintCtx::new(&mut with, Affine::IDENTITY, &mut text, &theme),
                rect,
            );
            let mut without = Scene::new();
            AvatarFace::new("Ada Lovelace", "ada").paint(
                &mut PaintCtx::new(&mut without, Affine::IDENTITY, &mut text, &theme),
                rect,
            );
            assert!(with.encoding().draw_tags.len() > without.encoding().draw_tags.len());
            let avatar = Avatar::person("Ada Lovelace", "ada").status(PresenceStatus::Away);
            assert_eq!(
                Widget::accessibility(&avatar).unwrap().name.as_deref(),
                Some("Ada Lovelace, away")
            );
            let neutral = AvatarFace::new("Notes", "notes").neutral();
            assert_eq!(neutral.colour(&theme), theme.tokens.colors.surfaces.raised);
        }
    }
}

mod component;
pub use component::AvatarProps;
pub(crate) use component::colour_of;
