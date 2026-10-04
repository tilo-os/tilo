//! Worker-free Photos rendering shared with the live library and viewer.
use crate::strings::{tr, tr_args, tr_count, tr_with};
use crate::{
    framework_history::asset_id,
    model::{Edits, OpenImage, PhotoLibrary},
    pixels,
};
use std::collections::BTreeMap;
use ui::ArgValue;
use ui::history_mode::{
    FrozenPreviewAssets, HistoryPreview, PreviewAccessibility, PreviewNavigation, PreviewView,
};
use ui::paint::PaintCtx;
use ui::widget::image_viewer::ImageViewport;
use ui::widget::{MediaArt, MediaAspect, MediaCardFace};
use ui::{TextRenderer, TextRole, Theme};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect, Vec2},
    peniko::{BlendMode, Fill, ImageData},
};
/// The shape every photograph takes in the history grid.
const CARD_ASPECT: MediaAspect = MediaAspect::Ratio(1.5);

#[derive(Clone)]
enum Page {
    Image,
    Photo(usize),
    Grid(Vec<usize>),
}
/// This value cannot access a live document or any effect service. Decoded
/// images are keyed by immutable content plus crop/rotation, never photo ID.
pub(crate) struct PhotoPresentation {
    open: Option<OpenImage>,
    edits: Edits,
    library: PhotoLibrary,
    assets: FrozenPreviewAssets,
    pages: Vec<(String, String)>,
    kinds: Vec<Page>,
    view: PreviewView,
    bounds: Rect,
    canvas: Rect,
    thumbs: BTreeMap<String, Option<ImageData>>,
    full: Option<(String, Option<ImageData>)>,
    cards: Vec<(usize, Rect, Rect)>,
    heading: String,
    caption: String,
}
impl PhotoPresentation {
    pub(crate) fn new(
        open: Option<OpenImage>,
        edits: Edits,
        library: PhotoLibrary,
        assets: FrozenPreviewAssets,
        mut view: PreviewView,
    ) -> Self {
        let mut pages = Vec::new();
        let mut kinds = Vec::new();
        if let Some(o) = &open {
            pages.push((
                "image".into(),
                tr_with("photos-history-open-photo", "name", &o.name),
            ));
            kinds.push(Page::Image);
        }
        let all = (0..library.photos.len()).collect::<Vec<_>>();
        for (i, chunk) in all.chunks(24).enumerate() {
            pages.push((
                format!("library:{i}"),
                tr_count("photos-history-library-page", i + 1),
            ));
            kinds.push(Page::Grid(chunk.to_vec()));
        }
        let photo_indices = library
            .photos
            .iter()
            .enumerate()
            .map(|(i, p)| (p.id.as_str(), i))
            .collect::<std::collections::HashMap<_, _>>();
        for album in &library.albums {
            let members = album
                .photo_ids
                .iter()
                .filter_map(|id| photo_indices.get(id.as_str()).copied())
                .collect::<Vec<_>>();
            if members.is_empty() {
                pages.push((
                    format!("album:{}:0", album.id),
                    tr_with("photos-history-album-empty", "name", &album.name),
                ));
                kinds.push(Page::Grid(Vec::new()));
            }
            for (i, chunk) in members.chunks(24).enumerate() {
                pages.push((
                    format!("album:{}:{i}", album.id),
                    tr_args(
                        "photos-history-album-page",
                        &[
                            ("name", ArgValue::Text(album.name.clone())),
                            ("count", ArgValue::Number((i + 1) as f64)),
                        ],
                    ),
                ));
                kinds.push(Page::Grid(chunk.to_vec()));
            }
        }
        for (i, p) in library.photos.iter().enumerate() {
            pages.push((format!("photo:{}", p.id), p.image.name.clone()));
            kinds.push(Page::Photo(i));
        }
        if !pages.iter().any(|(id, _)| Some(id) == view.page.as_ref()) {
            view.page = pages.first().map(|(id, _)| id.clone());
        }
        if !view.zoom.is_finite() {
            view.zoom = 1.;
        }
        view.zoom = view.zoom.clamp(0.1, 10.);
        if !view.scroll.0.is_finite() || !view.scroll.1.is_finite() {
            view.scroll = (0., 0.);
        }
        let mut presentation = Self {
            open,
            edits,
            library,
            assets,
            pages,
            kinds,
            view,
            bounds: Rect::ZERO,
            canvas: Rect::ZERO,
            thumbs: BTreeMap::new(),
            full: None,
            cards: Vec::new(),
            heading: String::new(),
            caption: String::new(),
        };
        presentation.prepare_full();
        presentation
    }
    fn page(&self) -> Option<&Page> {
        self.pages
            .iter()
            .position(|(id, _)| Some(id) == self.view.page.as_ref())
            .and_then(|i| self.kinds.get(i))
    }
    fn selected(&self) -> Option<(&OpenImage, Edits)> {
        match self.page()? {
            Page::Image => self.open.as_ref().map(|o| (o, self.edits)),
            Page::Photo(i) => self
                .library
                .photos
                .get(*i)
                .map(|p| (&p.image, Edits::default())),
            Page::Grid(_) => None,
        }
    }
    fn photo_label(&self, i: usize) -> String {
        let Some(p) = self.library.photos.get(i) else {
            return String::new();
        };
        let albums = self
            .library
            .albums
            .iter()
            .filter(|a| a.photo_ids.contains(&p.id))
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let mut label = p.image.name.clone();
        if p.favorite {
            label.push_str(". ");
            label.push_str(&tr("photos-favorite"));
        }
        label.push_str(". ");
        label.push_str(&crate::app::dimensions_label(p.image.width, p.image.height));
        let summary = p.metadata_summary();
        if !summary.is_empty() {
            label.push_str(". ");
            label.push_str(&summary);
        }
        if !albums.is_empty() {
            label.push_str(". ");
            label.push_str(&tr_with("photos-history-in-albums", "names", &albums));
        }
        label
    }
    fn decode(&self, open: &OpenImage, edits: Edits, thumbnail: bool) -> Option<ImageData> {
        let id = asset_id(open)?;
        let bytes = self.assets.get(&id)?;
        // Recheck even manually assembled inputs: admitted maps are public values.
        if !tilo_content::verify(open.content.as_deref()?, bytes) {
            return None;
        }
        let original = pixels::decode(bytes)?;
        let rendered = pixels::render(&original, edits.crop, edits.rotate_quarters);
        Some(pixels::to_peniko(&if thumbnail {
            pixels::thumbnail(&rendered, 320, 180)
        } else {
            rendered
        }))
    }
    fn prepare_full(&mut self) {
        if let Some((open, edits)) = self.selected() {
            let key = format!(
                "{}:{:?}:{:?}",
                asset_id(open).unwrap_or_default(),
                edits.crop,
                edits.rotate_quarters
            );
            if self.full.as_ref().is_none_or(|(old, _)| old != &key) {
                let image = self.decode(open, edits, false);
                self.full = Some((key, image));
            }
        } else {
            self.full = None;
        }
    }
    fn viewport(&self) -> ImageViewport {
        ImageViewport {
            rect: self.canvas,
            zoom: self.view.zoom,
            pan: Vec2::new(self.view.scroll.0, self.view.scroll.1),
        }
    }
    fn clamp_pan(&mut self) {
        if let Some((_, Some(image))) = &self.full {
            let scale = self.viewport().base_scale(image.width, image.height) * self.view.zoom;
            if scale > 0. {
                let max_x =
                    ((f64::from(image.width) * scale - self.canvas.width()) / 2.).max(0.) / scale;
                let max_y =
                    ((f64::from(image.height) * scale - self.canvas.height()) / 2.).max(0.) / scale;
                self.view.scroll.0 = self.view.scroll.0.clamp(-max_x, max_x);
                self.view.scroll.1 = self.view.scroll.1.clamp(-max_y, max_y);
            }
        }
    }
}
impl HistoryPreview for PhotoPresentation {
    #[expect(
        clippy::too_many_lines,
        reason = "the heading, caption and grid geometry are measured in one pass"
    )]
    fn layout(&mut self, bounds: Rect, tr: &mut TextRenderer, theme: &Theme) -> Result<(), String> {
        self.bounds = bounds;
        self.cards.clear();
        self.prepare_full();
        (self.heading, self.caption) = match self.page() {
            Some(Page::Image) => self.open.as_ref().map_or_else(Default::default, |o| {
                let mut facts = vec![crate::app::dimensions_label(o.width, o.height)];
                if self.edits.crop.w < 1.0 || self.edits.crop.h < 1.0 {
                    facts.push("cropped".to_owned());
                }
                if self.edits.rotate_quarters != 0 {
                    facts.push(format!(
                        "turned {}°",
                        u32::from(self.edits.rotate_quarters) * 90
                    ));
                }
                (o.name.clone(), facts.join(", "))
            }),
            Some(Page::Photo(i)) => {
                self.library
                    .photos
                    .get(*i)
                    .map_or_else(Default::default, |p| {
                        (
                            p.image.name.clone(),
                            crate::app::dimensions_label(p.image.width, p.image.height),
                        )
                    })
            }
            _ => (
                self.pages
                    .iter()
                    .find(|(id, _)| Some(id) == self.view.page.as_ref())
                    .map_or_else(
                        || crate::strings::tr("photos-history-nothing"),
                        |(_, label)| label.clone(),
                    ),
                String::new(),
            ),
        };
        let gutter = f64::from(theme.tokens.spacing.rhythm.section);
        let width = (bounds.width() - 2.0 * gutter).max(1.);
        let mut ctx_h = f64::from(
            tr.measure_style(
                &self.heading,
                Some(width as f32),
                &theme.text(TextRole::Title),
            )
            .1,
        );
        if !self.caption.is_empty() {
            ctx_h += f64::from(
                tr.measure_style(
                    &self.caption,
                    Some(width as f32),
                    &theme.text(TextRole::Caption),
                )
                .1,
            );
        }
        let top = (bounds.y0 + gutter + ctx_h + gutter).min(bounds.y1);
        self.canvas = Rect::new(
            bounds.x0 + gutter,
            top,
            (bounds.x1 - gutter).max(bounds.x0 + gutter),
            bounds.y1.max(top),
        );
        if let Some(Page::Grid(indices)) = self.page().cloned() {
            let rhythm = &theme.tokens.spacing.rhythm;
            let gap = f64::from(rhythm.section);
            let row_gap = f64::from(rhythm.group);
            let extent = ui::widget::MediaSize::Regular.extent(theme);
            let columns = ui::widget::card_columns(width, extent, gap).max(1);
            let card_w = ui::widget::card_column_width(width, columns, gap).max(1.);
            let probe = MediaCardFace::new(MediaArt::Placeholder)
                .aspect(CARD_ASPECT)
                .title("Ag")
                .meta("Ag");
            let preview_h = card_w / CARD_ASPECT.ratio();
            let card_h = preview_h + probe.text_height(tr, theme, card_w);
            let rows = indices.len().div_ceil(columns);
            let extent = rows as f64 * (card_h + row_gap);
            self.view.scroll.0 = 0.;
            self.view.scroll.1 = self
                .view
                .scroll
                .1
                .clamp(0., (extent - self.canvas.height()).max(0.));
            if self.thumbs.len() > 48 {
                self.thumbs.clear();
            }
            for (n, i) in indices.into_iter().enumerate() {
                let x = self.canvas.x0 + (n % columns) as f64 * (card_w + gap);
                let y =
                    self.canvas.y0 + (n / columns) as f64 * (card_h + row_gap) - self.view.scroll.1;
                let card = Rect::new(x, y, x + card_w, y + card_h);
                let preview = Rect::new(x, y, x + card_w, y + preview_h);
                if card.y1 < self.canvas.y0 || card.y0 > self.canvas.y1 {
                    continue;
                }
                if let Some(open) = self.library.photos.get(i).map(|p| &p.image) {
                    if let Some(id) = asset_id(open)
                        && !self.thumbs.contains_key(&id)
                    {
                        let img = self.decode(open, Edits::default(), true);
                        self.thumbs.insert(id, img);
                    }
                    self.cards.push((i, card, preview));
                }
            }
        } else {
            self.clamp_pan();
        }
        Ok(())
    }
    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme) {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.,
            transform,
            &self.bounds,
        );
        let gutter = f64::from(theme.tokens.spacing.rhythm.section);
        let width = (self.bounds.width() - 2.0 * gutter).max(1.) as f32;
        {
            let mut ctx = PaintCtx::new(scene, transform, tr, theme);
            let mut origin = Point::new(self.bounds.x0 + gutter, self.bounds.y0 + gutter);
            let title_h = ctx
                .measure_role(&self.heading, TextRole::Title, Some(width))
                .height;
            ctx.text_role(
                &self.heading,
                TextRole::Title,
                theme.text_primary,
                origin,
                Some(width),
            );
            origin.y += title_h;
            if !self.caption.is_empty() {
                ctx.text_role(
                    &self.caption,
                    TextRole::Caption,
                    theme.text_dim,
                    origin,
                    Some(width),
                );
            }
        }
        if matches!(self.page(), Some(Page::Grid(_))) {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.,
                transform,
                &self.canvas,
            );
            let not_recorded = crate::strings::tr("photos-history-not-recorded");
            for (i, card, _) in &self.cards {
                let Some(photo) = self.library.photos.get(*i) else {
                    continue;
                };
                let image = asset_id(&photo.image)
                    .and_then(|id| self.thumbs.get(&id))
                    .and_then(Option::as_ref);
                let meta = photo.metadata_card_summary();
                let mut face =
                    MediaCardFace::new(image.map_or(MediaArt::Placeholder, MediaArt::Image))
                        .aspect(CARD_ASPECT)
                        .title(&photo.image.name);
                if image.is_none() {
                    face = face.meta(&not_recorded);
                } else if !meta.is_empty() {
                    face = face.meta(&meta);
                }
                face.paint(&mut PaintCtx::new(scene, transform, tr, theme), *card);
            }
            scene.pop_layer();
        } else if let Some((_, Some(image))) = &self.full {
            self.viewport().draw(image, scene, transform, theme);
        } else if self.selected().is_some() {
            PaintCtx::new(scene, transform, tr, theme).text_role(
                &crate::strings::tr("photos-history-version-missing"),
                TextRole::Body,
                theme.text_dim,
                Point::new(self.canvas.x0, self.canvas.y0),
                Some(self.canvas.width().max(1.) as f32),
            );
        }
        scene.pop_layer();
    }
    fn navigate(&mut self, command: PreviewNavigation) -> bool {
        match command {
            PreviewNavigation::Zoom { factor, anchor } => {
                if !factor.is_finite()
                    || factor <= 0.
                    || !anchor.x.is_finite()
                    || !anchor.y.is_finite()
                    || self.selected().is_none()
                {
                    return false;
                }
                self.prepare_full();
                let before = self
                    .full
                    .as_ref()
                    .and_then(|(_, image)| image.as_ref())
                    .and_then(|i| {
                        self.viewport()
                            .screen_to_image(i.width, i.height, anchor.to_vec2())
                    });
                self.view.zoom = (self.view.zoom * factor).clamp(0.1, 10.);
                if let (Some(before), Some((_, Some(i)))) = (before, &self.full)
                    && let Some(after) =
                        self.viewport()
                            .screen_to_image(i.width, i.height, anchor.to_vec2())
                {
                    self.view.scroll.0 += after.x - before.x;
                    self.view.scroll.1 += after.y - before.y;
                }
                self.clamp_pan();
                true
            }
            PreviewNavigation::Scroll { dx, dy } if self.selected().is_some() => {
                if !dx.is_finite() || !dy.is_finite() {
                    return false;
                }
                self.prepare_full();
                let scale = self
                    .full
                    .as_ref()
                    .and_then(|(_, i)| i.as_ref())
                    .map_or(1., |i| {
                        self.viewport().base_scale(i.width, i.height) * self.view.zoom
                    })
                    .max(0.0001);
                self.view.scroll.0 += dx / scale;
                self.view.scroll.1 += dy / scale;
                self.clamp_pan();
                true
            }
            other => {
                let changed =
                    ui::history_mode::adapter::navigate_pages(&mut self.view, other, &self.pages);
                if changed {
                    self.prepare_full();
                }
                changed
            }
        }
    }
    fn view(&self) -> PreviewView {
        self.view.clone()
    }
    fn accessibility(&self) -> PreviewAccessibility {
        let mut descriptions = Vec::new();
        if let Some(open) = &self.open {
            descriptions.push(format!(
                "{}. {}",
                tr_with("photos-history-open-photo", "name", &open.name),
                crate::app::dimensions_label(open.width, open.height)
            ));
        }
        match self.page() {
            Some(Page::Grid(indices)) => {
                for i in indices {
                    descriptions.push(self.photo_label(*i));
                    if self.library.photos.get(*i).is_some_and(|p| {
                        asset_id(&p.image).is_none_or(|id| {
                            self.thumbs
                                .get(&id)
                                .map_or_else(|| self.assets.get(&id).is_none(), Option::is_none)
                        })
                    }) {
                        descriptions.push(tr("photos-history-pixels-missing"));
                    }
                }
            }
            Some(Page::Photo(i)) => descriptions.push(self.photo_label(*i)),
            _ => {}
        }
        if let Some((_, edits)) = self.selected() {
            descriptions.push(tr_args(
                "photos-history-edit",
                &[
                    ("width", ArgValue::Number(edits.crop.w)),
                    ("height", ArgValue::Number(edits.crop.h)),
                    ("x", ArgValue::Number(edits.crop.x)),
                    ("y", ArgValue::Number(edits.crop.y)),
                    ("turns", ArgValue::Number(f64::from(edits.rotate_quarters))),
                ],
            ));
            if self.full.as_ref().is_none_or(|(_, image)| image.is_none()) {
                descriptions.push(tr("photos-history-pixels-missing"));
            }
        }
        if descriptions.is_empty() {
            descriptions.push(tr("photos-history-empty"));
        }
        PreviewAccessibility {
            description: descriptions.join("\n"),
            pages: self.pages.clone(),
        }
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::*;
    fn fixture() -> PhotoPresentation {
        let rgba = crate::shots_pattern(6, 4);
        let bytes = pixels::encode_png(&rgba).unwrap();
        let hash = tilo_content::hash_bytes(&bytes);
        let open = OpenImage::from_path("historic.png", 6, 4).with_content(hash);
        let mut library = PhotoLibrary::default();
        for i in 0..24 {
            let mut p = crate::model::LibraryPhoto::from_image(open.clone());
            p.id = format!("{i}");
            p.image.name = format!("Photo {i}");
            library.photos.push(p);
        }
        PhotoPresentation::new(
            Some(open.clone()),
            Edits::default(),
            library,
            FrozenPreviewAssets::new(BTreeMap::from([(
                asset_id(&open).unwrap(),
                std::sync::Arc::from(bytes),
            )])),
            PreviewView::default(),
        )
    }
    fn render(p: &mut PhotoPresentation, width: f64, scale: f32) -> Scene {
        let theme = Theme::dark().with_text_scale(scale);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(scale);
        p.layout(Rect::new(0., 0., width, 600.), &mut tr, &theme)
            .unwrap();
        let mut scene = Scene::new();
        p.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme);
        scene
    }
    fn has_transform(scene: &Scene, want: Affine) {
        let expected = vello_encoding::Transform::from_kurbo(&want);
        assert!(
            scene.encoding().transforms.iter().any(|t| t
                .matrix
                .iter()
                .zip(expected.matrix)
                .all(|(a, b)| (a - b).abs() < 0.001)
                && t.translation
                    .iter()
                    .zip(expected.translation)
                    .all(|(a, b)| (a - b).abs() < 0.01)),
            "painted transform missing: {want:?}"
        );
    }
    fn has_clip(scene: &Scene, rect: Rect) {
        assert!(scene.encoding().n_clips >= 2);
        assert_eq!(scene.encoding().n_open_clips, 0);
        for (x, y) in [
            (rect.x0, rect.y0),
            (rect.x1, rect.y0),
            (rect.x1, rect.y1),
            (rect.x0, rect.y1),
        ] {
            assert!(
                scene
                    .encoding()
                    .path_data
                    .windows(2)
                    .any(|v| (f32::from_bits(v[0]) - x as f32).abs() < 0.01
                        && (f32::from_bits(v[1]) - y as f32).abs() < 0.01),
                "painted clip corner {x},{y} missing"
            );
        }
    }
    #[test]
    fn actual_viewer_transform_clip_pan_reverse_and_resize_recreation() {
        for (width, scale) in [(1000., 1.), (360., 2.)] {
            let mut p = fixture();
            let scene = render(&mut p, width, scale);
            let image = p.full.as_ref().unwrap().1.as_ref().unwrap();
            let transform = p
                .viewport()
                .image_transform(image.width, image.height)
                .unwrap();
            has_transform(&scene, transform);
            has_clip(&scene, p.canvas);
            let top = transform * vello::kurbo::Point::ZERO;
            let bottom = transform * vello::kurbo::Point::new(6., 4.);
            assert!(
                top.x >= p.canvas.x0 - 0.01
                    && top.y >= p.canvas.y0 - 0.01
                    && bottom.x <= p.canvas.x1 + 0.01
                    && bottom.y <= p.canvas.y1 + 0.01
            );
            p.navigate(PreviewNavigation::Zoom {
                factor: 3.,
                anchor: p.canvas.center(),
            });
            let scene = render(&mut p, width, scale);
            let baseline = p.viewport().image_transform(6, 4).unwrap();
            has_transform(&scene, baseline);
            p.navigate(PreviewNavigation::Scroll { dx: 20., dy: 20. });
            let moved = render(&mut p, width, scale);
            let shifted = p.viewport().image_transform(6, 4).unwrap();
            has_transform(&moved, shifted);
            has_clip(&moved, p.canvas);
            assert!((shifted.as_coeffs()[4] - baseline.as_coeffs()[4] - 20.).abs() < 0.01);
            assert!((shifted.as_coeffs()[5] - baseline.as_coeffs()[5] - 20.).abs() < 0.01);
            p.navigate(PreviewNavigation::Scroll { dx: -20., dy: -20. });
            let reversed = render(&mut p, width, scale);
            has_transform(&reversed, baseline);
            let view = p.view();
            let mut recreated = PhotoPresentation::new(
                p.open.clone(),
                p.edits,
                p.library.clone(),
                p.assets.clone(),
                view.clone(),
            );
            let scene = render(&mut recreated, width, scale);
            assert_eq!(recreated.view(), view);
            has_transform(&scene, baseline);
            let resized = render(
                &mut recreated,
                if width < 500. { 1000. } else { 360. },
                scale,
            );
            has_transform(
                &resized,
                recreated.viewport().image_transform(6, 4).unwrap(),
            );
            has_clip(&resized, recreated.canvas);
        }
    }
    #[test]
    fn final_grid_card_is_reachable_with_actual_thumbnail_transform_and_clipping() {
        for (width, scale) in [(1000., 1.), (360., 2.)] {
            let mut p = fixture();
            assert!(p.navigate(PreviewNavigation::SelectPage("library:0".into())));
            render(&mut p, width, scale);
            let first = p.cards.first().unwrap().1;
            p.navigate(PreviewNavigation::Scroll {
                dx: 0.,
                dy: -100_000.,
            });
            let scene = render(&mut p, width, scale);
            let (_, card, preview) = p
                .cards
                .iter()
                .find(|(i, _, _)| *i == 23)
                .expect("last photo actually materialized at end");
            assert!(
                card.x0 >= p.canvas.x0 - 0.01
                    && card.x1 <= p.canvas.x1 + 0.01
                    && card.y1 <= p.canvas.y1 + 0.01
                    && card.y0 >= p.canvas.y0 - 0.01
            );
            // The thumbnail fills the card's art, cover-fitted.
            let image = p
                .thumbs
                .values()
                .flatten()
                .next()
                .expect("the fixture thumbnail decoded");
            let fit = (preview.width() / f64::from(image.width))
                .max(preview.height() / f64::from(image.height));
            let transform = Affine::translate((
                preview.x0 + (preview.width() - f64::from(image.width) * fit) / 2.,
                preview.y0 + (preview.height() - f64::from(image.height) * fit) / 2.,
            )) * Affine::scale(fit);
            has_transform(&scene, transform);
            has_clip(&scene, p.canvas);
            let scroll = p.view.scroll.1;
            p.navigate(PreviewNavigation::Scroll { dx: 0., dy: 32. });
            render(&mut p, width, scale);
            assert!((scroll - p.view.scroll.1 - 32.).abs() < 0.01);
            let view = p.view();
            let mut recreated = PhotoPresentation::new(
                p.open.clone(),
                p.edits,
                p.library.clone(),
                p.assets.clone(),
                view.clone(),
            );
            render(&mut recreated, width, scale);
            assert_eq!(recreated.view(), view);
            p.navigate(PreviewNavigation::Scroll {
                dx: 0.,
                dy: 100_000.,
            });
            render(&mut p, width, scale);
            assert_eq!(p.cards.first().unwrap().1, first);
        }
    }
}
