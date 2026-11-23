//! What an attachment does when a person touches it.

use vello::kurbo::{Point, Rect, Size};

use crate::format;
use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::Tone;
use crate::widget::{ChipFace, ChipLead};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Video,
    File,
}

impl AttachmentKind {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Image => "conversation-attachment-image",
            Self::Video => "conversation-attachment-video",
            Self::File => "conversation-attachment-file",
        }
    }

    #[must_use]
    pub fn label(self) -> String {
        super::t(self.key())
    }

    #[must_use]
    pub const fn icon(self) -> Icon {
        match self {
            Self::Image => Icon::Image,
            Self::Video => Icon::Film,
            Self::File => Icon::FileText,
        }
    }

    /// Whether an inline preview shows the thing itself (a picture, a frame of
    /// a clip) rather than a card naming it.
    #[must_use]
    pub const fn previews(self) -> bool {
        matches!(self, Self::Image | Self::Video)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    /// The bytes are here: the whole ladder is on offer.
    Ready,
    /// The sender never shared these bytes with this person.
    NotShared,
    /// The bytes are not on this device and could not be fetched.
    NotOnDevice,
    Uploading(UploadProgress),
    /// The transport refused it, or failed partway.
    Failed(String),
}

impl Availability {
    /// The tone the state's word reads in: quiet until something is wrong.
    #[must_use]
    pub const fn tone(&self) -> Tone {
        match self {
            Self::Ready => Tone::Neutral,
            Self::NotShared | Self::NotOnDevice | Self::Failed(_) => Tone::Danger,
            Self::Uploading(_) => Tone::Info,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UploadProgress {
    /// Bytes the transport has taken so far.
    pub sent: u64,
    /// Bytes in total, when known.
    pub total: Option<u64>,
}

impl UploadProgress {
    /// A fraction of the way, `0.0..=1.0`.
    ///
    /// `None` when the total is unknown: a progress bar with no denominator is
    /// a lie, so the caller shows the word without a number.
    #[must_use]
    pub fn fraction(self) -> Option<f32> {
        let total = self.total?;
        if total == 0 {
            return Some(1.0);
        }
        let sent = self.sent.min(total);
        Some(crate::num::f32_of_f64(
            crate::num::f64_of_u64(sent) / crate::num::f64_of_u64(total),
        ))
    }

    /// What the line says: "Uploading 1.2 MB of 200 MB", or just "Uploading"
    /// when the size is not known.
    #[must_use]
    pub fn text(self) -> String {
        let word = super::t("conversation-attachment-uploading");
        match self.total {
            Some(total) if total > 0 => {
                format!(
                    "{word} {} of {}",
                    format::size(self.sent),
                    format::size(total)
                )
            }
            _ => word,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedCopy {
    pub name: String,
    pub target: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentAction {
    OpenFullSize,
    Save,
    OpenSaved,
    CancelUpload,
}

impl AttachmentAction {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::OpenFullSize => "conversation-attachment-open-full-size",
            Self::Save => "conversation-attachment-save-as",
            Self::OpenSaved => "conversation-attachment-open-saved",
            Self::CancelUpload => "conversation-attachment-cancel-upload",
        }
    }

    #[must_use]
    pub fn label(self) -> String {
        super::t(self.key())
    }

    #[must_use]
    pub const fn icon(self) -> Icon {
        match self {
            Self::OpenFullSize => Icon::Maximize,
            Self::Save => Icon::Download,
            Self::OpenSaved => Icon::FolderOpen,
            Self::CancelUpload => Icon::X,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AttachmentSummary<'a> {
    pub kind: AttachmentKind,
    pub name: &'a str,
    /// Its size in bytes, when known.
    pub size: Option<u64>,
    pub availability: Availability,
    pub saved: Option<SavedCopy>,
}

impl<'a> AttachmentSummary<'a> {
    /// An attachment whose bytes are here.
    #[must_use]
    pub fn ready(kind: AttachmentKind, name: &'a str, size: Option<u64>) -> Self {
        Self {
            kind,
            name,
            size,
            availability: Availability::Ready,
            saved: None,
        }
    }

    #[must_use]
    pub fn in_state(mut self, availability: Availability) -> Self {
        self.availability = availability;
        self
    }

    #[must_use]
    pub fn saved_as(mut self, saved: SavedCopy) -> Self {
        self.saved = Some(saved);
        self
    }

    /// The rungs on offer, in order: open, save, open the saved copy — or
    /// cancel, while the transport is still working.
    ///
    /// A picture that cannot be read offers nothing: the ladder starts below a
    /// preview that is not there, and [`Self::unavailable_reason`] says why in
    /// the place the rungs would have been.
    #[must_use]
    pub fn actions(&self) -> Vec<AttachmentAction> {
        match &self.availability {
            Availability::Uploading(_) => vec![AttachmentAction::CancelUpload],
            Availability::Ready => {
                let mut actions = vec![AttachmentAction::OpenFullSize, AttachmentAction::Save];
                if self.saved.is_some() {
                    actions.push(AttachmentAction::OpenSaved);
                }
                actions
            }
            Availability::NotShared | Availability::NotOnDevice | Availability::Failed(_) => {
                Vec::new()
            }
        }
    }

    #[must_use]
    pub fn unavailable_reason(&self) -> Option<String> {
        match &self.availability {
            Availability::Ready | Availability::Uploading(_) => None,
            Availability::NotShared => Some(super::t("conversation-attachment-no-access")),
            Availability::NotOnDevice => Some(super::t("conversation-attachment-unavailable")),
            Availability::Failed(reason) => Some(reason.clone()),
        }
    }

    #[must_use]
    pub fn suggested_name(&self) -> String {
        self.name.to_owned()
    }

    /// What a screen reader hears for this attachment: what it is, what it is
    /// called, how big it is, and what state it is in. A copy on this device is
    /// stated as a state rather than announced as an event: saving says nothing
    /// unless it stalls or fails, and a person reading a list with a screen
    /// reader still needs to know where the file is.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut parts = vec![self.kind.label(), self.name.to_owned()];
        if let Some(size) = self.size {
            parts.push(format::size(size));
        }
        match &self.availability {
            Availability::Ready => {
                if let Some(saved) = &self.saved {
                    parts.push(format!(
                        "{} {}",
                        super::t("conversation-attachment-saved"),
                        saved.name
                    ));
                }
            }
            Availability::Uploading(progress) => parts.push(progress.text()),
            Availability::NotShared | Availability::NotOnDevice | Availability::Failed(_) => {
                parts.extend(self.unavailable_reason())
            }
        }
        parts.join(", ")
    }
}

/// Where a picture of `width`×`height` sits in `view` at full size.
///
/// Contained — the whole picture is always visible — and never enlarged past
/// its own pixels: a 96 px thumbnail blown up to a window is not "full size",
/// it is an invention. A picture smaller than its viewport therefore sits
/// centred at its own size.
#[must_use]
pub fn full_size_rect(view: Rect, width: u32, height: u32) -> Rect {
    if width == 0 || height == 0 || view.width() <= 0.0 || view.height() <= 0.0 {
        return Rect::new(view.x0, view.y0, view.x0, view.y0);
    }
    let w = f64::from(width);
    let h = f64::from(height);
    let scale = (view.width() / w).min(view.height() / h).min(1.0);
    let size = Size::new(w * scale, h * scale);
    Rect::from_center_size(view.center(), size)
}

pub struct AttachmentFace<'a> {
    summary: &'a AttachmentSummary<'a>,
}

impl<'a> AttachmentFace<'a> {
    #[must_use]
    pub fn new(summary: &'a AttachmentSummary<'a>) -> Self {
        Self { summary }
    }

    #[must_use]
    pub fn meta(&self) -> String {
        let mut parts = vec![self.summary.kind.label()];
        if let Some(size) = self.summary.size {
            parts.push(format::size(size));
        }
        match &self.summary.availability {
            Availability::Ready => {}
            Availability::Uploading(progress) => parts.push(progress.text()),
            Availability::NotShared | Availability::NotOnDevice | Availability::Failed(_) => {
                parts.extend(self.summary.unavailable_reason());
            }
        }
        parts.join(", ")
    }

    pub fn paint_meta(&self, paint: &mut PaintCtx<'_>, origin: Point, width: f64) -> f64 {
        let theme = paint.theme;
        let caption = self.meta();
        let style = theme.text(TextRole::Caption);
        let wrap = Some(crate::num::f32_of_f64(width));
        let (_, height) = paint.text.measure_style(&caption, wrap, &style);
        let ink = if self.summary.availability == Availability::Ready {
            theme.text_dim
        } else {
            self.summary.availability.tone().colour(theme)
        };
        paint.text(&caption, &style, ink, origin, wrap);
        f64::from(height)
    }

    pub fn paint_open_badge(&self, paint: &mut PaintCtx<'_>, preview: Rect, hovered: bool) {
        if self.summary.availability != Availability::Ready {
            return;
        }
        let PaintCtx {
            scene,
            transform,
            text,
            theme,
        } = paint;
        let word = super::t("conversation-attachment-open");
        let inset = f64::from(theme.tokens.spacing.s2);
        // The badge is its word while the pointer is on the picture, and its
        // glyph alone otherwise, so it never covers more of the picture than it
        // has to.
        let label = if hovered { word.as_str() } else { "" };
        let chip = chip_rect(text, theme, label, preview, inset);
        ChipFace::new(chip, label)
            .lead(ChipLead::Glyph(Icon::Maximize))
            .hovered(hovered)
            .paint(scene, *transform, text, theme);
    }
}

fn chip_rect(
    text: &mut TextRenderer,
    theme: &Theme,
    label: &str,
    preview: Rect,
    inset: f64,
) -> Rect {
    let pad_x = f64::from(theme.tokens.spacing.s2);
    let width =
        ChipFace::intrinsic_width(label, ChipLead::Glyph(Icon::Maximize), false, text, theme);
    let height = ChipFace::height(theme);
    let x1 = preview.x1 - inset;
    let x0 = (x1 - width - pad_x).max(preview.x0 + inset);
    Rect::new(
        x0,
        preview.y0 + inset,
        x0 + width + pad_x,
        preview.y0 + inset + height,
    )
}
