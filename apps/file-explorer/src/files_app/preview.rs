//! Quick Look: Space shows the selected file large, without opening an app.
//!
//! Pictures, PDFs, ebooks and videos show their picture (decoded off the UI
//! thread by the shared thumbnail generator, at the size the sheet needs);
//! text shows its first lines; anything else its icon. The sheet names the
//! file and says what it is. Space or Escape closes it; the arrow keys move
//! through the folder with it open. A file whose bytes are not on this device
//! is never fetched just to be previewed.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

use thumbnail::{FileCategory, category_for_path};
use ui::widget::modal_dialog;
use ui::widget::{ModalHit, ModalLayout, ModalSpec};
use vello::kurbo::{Point, Rect};
use vello::peniko::ImageData as PenikoImage;

use crate::model::Entry;

/// The longest edge a picture is decoded at.
const PICTURE_EDGE: u32 = 1600;
/// How much of a text file is read, and how many of its lines are shown.
const TEXT_BYTES: usize = 64 * 1024;
const TEXT_LINES: usize = 80;

/// What the sheet shows in its body.
enum Art {
    /// A picture being decoded.
    Loading(Receiver<Option<thumbnail::generate::RgbaThumb>>),
    Picture(PenikoImage),
    Text(String),
    /// Nothing to show but what the file is.
    Plain,
}

/// The file being previewed.
pub(crate) struct Preview {
    name: String,
    detail: String,
    icon: ui::icons::Icon,
    art: Art,
    layout: Option<ModalLayout>,
}

impl Preview {
    /// Preview `entry`; `local` says its bytes are on this device.
    pub(crate) fn of(entry: &Entry, local: bool) -> Self {
        let art = if !local || entry.is_dir {
            Art::Plain
        } else {
            match category_for_path(&entry.path) {
                category @ (FileCategory::Image
                | FileCategory::Pdf
                | FileCategory::Ebook
                | FileCategory::Video) => decode_picture(entry.path.clone(), category),
                _ if is_text(entry) => read_text(&entry.path).map_or(Art::Plain, Art::Text),
                _ => Art::Plain,
            }
        };
        Self {
            name: entry.name.clone(),
            detail: detail_line(entry),
            icon: ui::icons::icon_for_path(&entry.path, entry.is_dir),
            art,
            layout: None,
        }
    }

    /// Take a decoded picture when it is ready; whether the sheet changed.
    pub(crate) fn poll(&mut self) -> bool {
        let Art::Loading(rx) = &self.art else {
            return false;
        };
        match rx.try_recv() {
            Ok(Some(thumb)) => {
                self.art = Art::Picture(ui::raster::rgba8(thumb.rgba, thumb.width, thumb.height));
                true
            }
            Ok(None) | Err(TryRecvError::Disconnected) => {
                self.art = Art::Plain;
                true
            }
            Err(TryRecvError::Empty) => false,
        }
    }

    /// Whether a picture is still being decoded (the window checks back).
    pub(crate) fn is_loading(&self) -> bool {
        matches!(self.art, Art::Loading(_))
    }

    /// Whether a click at `point` closes the sheet (its close button or
    /// outside it).
    pub(crate) fn closes_at(&self, point: Point) -> bool {
        self.layout.as_ref().is_some_and(|layout| {
            matches!(layout.hit(point), Some(ModalHit::Close)) || layout.dismisses(point)
        })
    }

    pub(crate) fn title(&self) -> &str {
        &self.name
    }

    pub(crate) fn detail(&self) -> &str {
        &self.detail
    }

    /// Paint the sheet over `window`.
    pub(crate) fn paint(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        window: Rect,
    ) {
        let body_h = (window.height() * 0.62).max(120.0);
        let spec = ModalSpec::new(&self.name)
            .closable(true)
            .dismiss_on_scrim(true)
            .max_width((window.width() * 0.8).max(320.0))
            .body_height(body_h);
        let layout = modal_dialog::layout(window, &spec, text_renderer, theme);
        modal_dialog::paint_backdrop(scene, transform, text_renderer, theme, &layout, &spec);
        let (ink, dim) = modal_dialog::ink(theme);
        let body = layout.body;
        let caption = theme.text(ui::TextRole::Caption);
        let (_, detail_h) =
            text_renderer.measure_style(&self.detail, Some(body.width() as f32), &caption);
        text_renderer.draw_style(
            scene,
            &self.detail,
            dim,
            transform * vello::kurbo::Affine::translate((body.x0, body.y0)),
            Some(body.width() as f32),
            &caption,
        );
        let gap = f64::from(theme.tokens.spacing.rhythm.group);
        let art = Rect::new(
            body.x0,
            body.y0 + f64::from(detail_h) + gap,
            body.x1,
            body.y1,
        );
        match &self.art {
            Art::Picture(image) => {
                let fitted = fit(art, f64::from(image.width), f64::from(image.height));
                ui::image_store::paint_image_cover(
                    scene,
                    transform,
                    fitted,
                    image,
                    f64::from(theme.tokens.radii.detail),
                );
            }
            Art::Text(text) => {
                let style = theme.text(ui::TextRole::Code);
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    transform,
                    &art,
                );
                text_renderer.draw_style(
                    scene,
                    text,
                    ink,
                    transform * vello::kurbo::Affine::translate((art.x0, art.y0)),
                    Some(art.width() as f32),
                    &style,
                );
                scene.pop_layer();
            }
            Art::Loading(_) | Art::Plain => {
                let side = f64::from(theme.tokens.metrics.icon_lg) * 3.0;
                let icon = Rect::from_center_size(art.center(), (side, side));
                ui::icons::draw_icon(scene, transform, icon, self.icon, dim, 1.5);
            }
        }
        self.layout = Some(layout);
    }
}

/// `width` × `height` scaled to fit inside `area`, centred, never enlarged
/// past twice its size.
fn fit(area: Rect, width: f64, height: f64) -> Rect {
    if width <= 0.0 || height <= 0.0 {
        return area;
    }
    let scale = (area.width() / width).min(area.height() / height).min(2.0);
    Rect::from_center_size(area.center(), (width * scale, height * scale))
}

fn decode_picture(path: PathBuf, category: FileCategory) -> Art {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let thumb = thumbnail::generate::generate(
            &thumbnail::generate::Source::Path(&path),
            category,
            PICTURE_EDGE,
        );
        let _ = tx.send(thumb);
    });
    Art::Loading(rx)
}

fn is_text(entry: &Entry) -> bool {
    const TEXT: &[&str] = &[
        "txt", "md", "markdown", "rs", "toml", "json", "yaml", "yml", "csv", "log", "ini", "conf",
        "py", "js", "ts", "c", "h", "cpp", "sh", "html", "css", "xml", "ftl",
    ];
    entry.mime.starts_with("text/")
        || entry
            .path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| TEXT.contains(&ext.to_ascii_lowercase().as_str()))
}

/// The first lines of a text file; `None` when it isn't text after all.
fn read_text(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(TEXT_BYTES as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    let text = String::from_utf8_lossy(&bytes);
    if text.contains('\u{0}') {
        return None;
    }
    Some(text.lines().take(TEXT_LINES).collect::<Vec<_>>().join("\n"))
}

/// What the file is: its size (or that it is a folder) and when it last
/// changed.
fn detail_line(entry: &Entry) -> String {
    use crate::i18n::{arg, t, t_args};
    let what = if entry.is_dir {
        t("files-a11y-folder")
    } else {
        crate::model::human_size(entry)
    };
    match entry.modified {
        Some(modified) => t_args(
            "files-preview-detail",
            &[
                arg("what", what),
                arg("modified", crate::model::format_modified(Some(modified))),
            ],
        ),
        None => what,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_fits_its_area_without_growing_huge() {
        let area = Rect::new(0.0, 0.0, 400.0, 300.0);
        let wide = fit(area, 800.0, 200.0);
        assert!((wide.width() - 400.0).abs() < 1e-6 && (wide.height() - 100.0).abs() < 1e-6);
        let tiny = fit(area, 10.0, 10.0);
        assert!((tiny.width() - 20.0).abs() < 1e-6, "{tiny:?}");
        assert_eq!(tiny.center(), area.center());
    }
}
