//! What floats over the picture: the top strip (Library, the media title and
//! Open) and the idle, loading and error states shown while there is no
//! picture.
//!
//! Everything here is chrome on media: it reads `media.*` colours or the
//! floating material, never a window surface, so it looks the same in every
//! theme.

use super::{Affine, ButtonFace, Icon, PlayerApp, Rect, Scene, TextRenderer, Theme, fill_rect};
use ui::overlay::scrim::{FadeEdge, vertical_fade};
use ui::paint::PaintCtx;
use ui::tokens::TextRole;
use ui::vello::kurbo::Point;
use ui::widget::{
    Button, ButtonRole, ControlSize, EmptyState, EmptyStateContent, floating_surface,
};

/// How strongly the strip's fade darkens the picture under the title.
const STRIP_FADE_STRENGTH: f32 = 0.7;

/// The floating top strip: the one description `place_top_strip` (the
/// Library and Open rects hit-testing and accessibility read) and
/// `paint_overlay` (fade and title) agree on. Every control shares
/// `center_y`; the fade always contains them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TopStrip {
    pub center_y: f64,
    pub button_h: f64,
    pub height: f64,
}

impl TopStrip {
    pub(super) fn at(y0: f64, theme: &Theme) -> Self {
        let margin = f64::from(theme.tokens.spacing.s4);
        let button_h = ButtonFace::height(ControlSize::Regular, theme);
        Self {
            center_y: y0 + margin + button_h / 2.0,
            button_h,
            height: button_h + margin * 2.0,
        }
    }
}

/// The theme for what is drawn on the picture area. The letterbox is
/// near-black in every theme, so what stands on it (ink and the primary
/// action alike) is the dark material at the person's own scale.
fn media_theme(theme: &Theme) -> Theme {
    let dark = Theme::dark()
        .with_ui_scale(theme.ui_scale)
        .with_text_scale(theme.text_scale)
        .with_reduce_motion(theme.reduce_motion)
        .with_reduce_transparency(theme.reduce_transparency);
    if theme.colors_are_forced() {
        dark.with_high_contrast()
    } else {
        dark
    }
}

/// What the picture area says while there is nothing to show.
enum MediaStatus<'a> {
    /// Nothing is open.
    Idle,
    /// A source is open and its first frame has not arrived.
    Loading,
    /// The open failed; the decoder's reason.
    Failed(&'a str),
}

impl PlayerApp {
    /// Whether the floating strip should show this frame. It rides the
    /// player's own controls visibility, so it auto-hides during immersive
    /// playback, and it stays up while there is no picture so the library
    /// is always reachable.
    pub(super) fn overlay_visible(&self) -> bool {
        self.player.controls_visible() || self.player.latest_frame().is_none()
    }

    /// Place Library (leading) and Open (trailing) on the strip's centre
    /// line, each as wide as its label needs. With nothing open the empty
    /// state owns Open, so the strip keeps only Library.
    pub(super) fn place_top_strip(&mut self, text: &mut TextRenderer, theme: &Theme) {
        let strip = TopStrip::at(self.rect.y0, theme);
        let margin = f64::from(theme.tokens.spacing.s4);
        let y0 = strip.center_y - strip.button_h / 2.0;
        let y1 = strip.center_y + strip.button_h / 2.0;
        let width =
            |text: &mut TextRenderer, face: ButtonFace<'_>| face.intrinsic_width(text, theme);
        let library_w = width(text, Self::library_face(Rect::ZERO, false, false));
        let lx0 = self.rect.x0 + margin;
        let lx1 = (lx0 + library_w).min(self.rect.x1 - margin).max(lx0);
        self.library_button_rect = Rect::new(lx0, y0, lx1, y1);
        self.open_button_rect = if self.player.has_media() {
            let open_w = width(text, Self::open_face(Rect::ZERO, false, false));
            let bx1 = self.rect.x1 - margin;
            let bx0 = (bx1 - open_w).max(lx1 + margin);
            Rect::new(bx0.min(bx1), y0, bx1, y1)
        } else {
            Rect::ZERO
        };
    }

    fn library_face(rect: Rect, hovered: bool, focused: bool) -> ButtonFace<'static> {
        ButtonFace::new(rect, "Library")
            .icon(Icon::ListVideo)
            .role(ButtonRole::Plain)
            .hovered(hovered)
            .focused(focused)
    }

    fn open_face(rect: Rect, hovered: bool, focused: bool) -> ButtonFace<'static> {
        ButtonFace::new(rect, "Open")
            .icon(Icon::FolderOpen)
            .role(ButtonRole::Plain)
            .hovered(hovered)
            .focused(focused)
    }

    /// Paint the strip: a fade under the media title, and Library and Open
    /// as floating panels shaped to their buttons, inked for the panel.
    pub(super) fn paint_overlay(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let opacity = if self.player.has_media() {
            self.player.controls_opacity()
        } else {
            1.0
        };
        if opacity <= 0.0 {
            return;
        }
        scene.push_layer(
            ui::vello::peniko::Fill::NonZero,
            ui::vello::peniko::BlendMode::default(),
            opacity,
            transform,
            &self.rect,
        );
        let strip = TopStrip::at(self.rect.y0, theme);
        let fade = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + strip.height,
        );
        vertical_fade(
            scene,
            transform,
            fade,
            FadeEdge::Top,
            STRIP_FADE_STRENGTH,
            theme,
        );
        let margin = f64::from(theme.tokens.spacing.s3);
        let colors = &theme.tokens.colors;
        if self.player.has_media() {
            let title_x = self.library_button_rect.x1 + margin;
            let title_end = if self.open_button_rect.width() > 0.0 {
                self.open_button_rect.x0
            } else {
                self.rect.x1
            } - margin;
            let style = theme.text(TextRole::Subheading);
            let shown = text.elide_to_width_style(
                &self.title,
                ui::num::f32_of_f64((title_end - title_x).max(0.0)),
                &style,
            );
            let (_, title_h) = text.measure_style(&shown, None, &style);
            text.draw_style(
                scene,
                &shown,
                colors.media.ink,
                transform * Affine::translate((title_x, strip.center_y - f64::from(title_h) / 2.0)),
                None,
                &style,
            );
        }
        let panel_theme = theme.on_ground(ui::chrome::panel_ground(theme));
        let hovered = |rect: Rect| rect.contains(self.last_cursor);
        let library = self.library_button_rect;
        if library.width() > 0.0 {
            floating_surface::paint(scene, transform, library, theme);
            Button::paint(
                scene,
                transform,
                text,
                &panel_theme,
                &Self::library_face(library, hovered(library), self.library_focused),
            );
        }
        let open = self.open_button_rect;
        if open.width() > 0.0 {
            floating_surface::paint(scene, transform, open, theme);
            Button::paint(
                scene,
                transform,
                text,
                &panel_theme,
                &Self::open_face(open, hovered(open), self.open_focused),
            );
        }
        scene.pop_layer();
    }

    fn media_status(&self) -> Option<MediaStatus<'_>> {
        if (!self.shot_loading && self.player.latest_frame().is_some())
            || self.rect.height() < 180.0
        {
            return None;
        }
        Some(if let Some(error) = &self.open_error {
            MediaStatus::Failed(error)
        } else if self.player.has_media() || self.kept_recall.is_some() {
            MediaStatus::Loading
        } else {
            MediaStatus::Idle
        })
    }

    /// The picture area with no picture: nothing open, a first frame still
    /// coming, or a failed open. Text only, on the letterbox, inked for it.
    /// Returns nothing to hit-test when there is no action to offer.
    pub(super) fn paint_media_status(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.status_action_rect = Rect::ZERO;
        let Some(status) = self.media_status() else {
            return;
        };
        let media_theme = media_theme(theme);
        let mut ctx = PaintCtx::new(scene, transform, text, &media_theme);
        let content = match status {
            MediaStatus::Idle => EmptyStateContent {
                icon: Some(Icon::Film),
                action_label: Some("Open a video"),
                ..EmptyStateContent::new("No video open")
            },
            MediaStatus::Loading => EmptyStateContent {
                icon: Some(Icon::Film),
                ..EmptyStateContent::new("Loading video")
            },
            MediaStatus::Failed(error) => EmptyStateContent {
                icon: Some(Icon::TriangleAlert),
                message: Some(error),
                action_label: Some("Open another video"),
                error: true,
                ..EmptyStateContent::new("Couldn't open video")
            },
        };
        let cursor: Point = self.last_cursor;
        self.status_action_rect =
            EmptyState::paint(&mut ctx, self.rect, &content, cursor).unwrap_or(Rect::ZERO);
    }

    /// The letterbox, for a staged loading state that holds back the picture.
    pub(super) fn paint_letterbox(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        fill_rect(
            scene,
            transform,
            self.rect,
            theme.tokens.colors.media.letterbox,
        );
    }
}
