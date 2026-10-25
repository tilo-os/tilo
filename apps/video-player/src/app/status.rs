//! What the picture area says while there is no picture: nothing is open, a
//! first frame is coming, or the open failed.
//!
//! It is drawn on media: it reads the letterbox's colours, never a window
//! surface, so it looks the same in every theme.

use super::{Affine, Icon, PlayerApp, Rect, Scene, TextRenderer, Theme, fill_rect};
use ui::paint::PaintCtx;
use ui::vello::kurbo::Point;
use ui::widget::{EmptyState, EmptyStateContent};

/// The theme for what is drawn on the picture area: the person's theme on
/// the letterbox, which is near-black in every theme, so its ink and its
/// primary action are re-derived to read there.
fn media_theme(theme: &Theme) -> Theme {
    theme.on_ground(theme.tokens.colors.media.letterbox)
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
        let missing_title = crate::i18n::t("video-missing-decoder-title");
        let codec_message = self.missing_codec.as_ref().map(|codec| {
            crate::i18n::t_args(
                "video-missing-decoder-message",
                &[("codec", ui::i18n::ArgValue::from(codec.clone()))],
            )
        });
        // The decoder state's action lists the formats; every other one opens
        // a video, which can also be dropped.
        let lists_formats = matches!(status, MediaStatus::Failed(_)) && codec_message.is_some();
        let content = match status {
            MediaStatus::Idle => EmptyStateContent {
                icon: Some(Icon::Film),
                message: self.empty_picture_hint(),
                action_label: Some("Open a video"),
                ..EmptyStateContent::new("No video open")
            },
            MediaStatus::Loading => EmptyStateContent {
                icon: Some(Icon::Film),
                ..EmptyStateContent::new("Loading video")
            },
            MediaStatus::Failed(_) if codec_message.is_some() => EmptyStateContent {
                icon: Some(Icon::TriangleAlert),
                message: codec_message.as_deref(),
                action_label: Some(&self.formats_label),
                error: true,
                ..EmptyStateContent::new(&missing_title)
            },
            MediaStatus::Failed(error) => EmptyStateContent {
                icon: Some(Icon::TriangleAlert),
                message: Some(error),
                action_label: Some("Open another video"),
                error: true,
                ..EmptyStateContent::new("Can't play this video")
            },
        };
        let cursor: Point = self.last_cursor;
        let status_action = if lists_formats {
            EmptyState::paint_focused(&mut ctx, self.rect, &content, cursor, self.status_focused)
        } else {
            self.file_open
                .paint(&mut ctx, self.rect, &content, cursor, self.status_focused)
        };
        self.status_action_rect = status_action.unwrap_or(Rect::ZERO);
    }

    /// How the person reaches the library from an empty picture: the window
    /// bar's sidebar button, while the library is away.
    fn empty_picture_hint(&self) -> Option<&'static str> {
        (!self.library_visible)
            .then_some("Open Library in the window bar for videos you played before.")
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
