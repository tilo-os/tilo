//! Frozen theme-library presentation. Applying an OS theme remains a live action.
use crate::{
    doc,
    model::{ThemeSpec, TokenId},
};
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc};
use ui::history_mode::adapter::navigate_pages;
use ui::history_mode::restore::{HistoryRestoreRoot, HistoryRestoreSchema, RestoreValueType};
use ui::history_mode::{
    HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition,
    PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView,
};
use ui::tokens::Spacing;
use ui::{TextRenderer, Theme};
use vello::{
    Scene,
    kurbo::{Affine, Rect},
    peniko::{BlendMode, Fill},
};

pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.themes",
    scopes: &[HistoryScopeDefinition {
        key: "library",
        title: "Theme library (Apply separately)",
        presentation: HistoryPresentation::Domain { create: preview },
        content_schema: Some(&SCHEMA),
    }],
};
pub static SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "themes.library",
    version: 1,
    roots: &[
        HistoryRestoreRoot {
            key: "themes",
            required: false,
            expected_type: RestoreValueType::Object(ObjType::Map),
        },
        HistoryRestoreRoot {
            key: "theme_tombstones",
            required: false,
            expected_type: RestoreValueType::Object(ObjType::Map),
        },
    ],
    selectors: &[],
    preserved_roots: &["active"],
    preserved: &[],
    validate,
    validate_result: validate,
};
fn validate(d: &AutoCommit) -> Result<(), String> {
    doc::validate_history_catalog(d)
}
fn preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let d = AutoCommit::load(input.snapshot.bytes()).map_err(|e| e.to_string())?;
    validate(&d)?;
    let recorded = d.get(ROOT, "themes").ok().flatten().is_some();
    let mut specs = if recorded {
        doc::load_catalog(&d, &ROOT).unwrap_or_default()
    } else {
        vec![]
    };
    // Authored entries first, followed by the unchanged shipped presets.
    specs.sort_by_key(|s| s.builtin);
    let mut view = input.initial_view;
    if !specs.iter().any(|s| Some(&s.id) == view.page.as_ref()) {
        view.page = specs.first().map(|s| s.id.clone());
    }
    Ok(Box::new(LibraryPreview {
        specs,
        view,
        bounds: Rect::ZERO,
        specimen: crate::preview::PreviewPane::new(),
    }))
}
struct LibraryPreview {
    specs: Vec<ThemeSpec>,
    view: PreviewView,
    bounds: Rect,
    specimen: crate::preview::PreviewPane,
}
impl HistoryPreview for LibraryPreview {
    fn layout(&mut self, bounds: Rect, _: &mut TextRenderer, _: &Theme) -> Result<(), String> {
        self.bounds = bounds;
        Ok(())
    }
    fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        // The library panel is a raised surface on the history host's ground,
        // so it carries a derived rest edge (`ui::surface::card`, HIG §9.5)
        // instead of a bare fill. Its content follows one rhythm: the row,
        // the specimen band and the token list are `s3` siblings inset `s3`
        // from the panel edge, so none of them paints over that edge.
        let inset = f64::from(Spacing::BASE.s3);
        let radius = f64::from(theme.tokens.radii.md);
        ui::surface::card(scene, t, self.bounds, radius, theme.bg_primary);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1., t, &self.bounds);
        if let Some(s) = self
            .specs
            .iter()
            .find(|s| Some(&s.id) == self.view.page.as_ref())
        {
            let y = self.bounds.y0 - self.view.scroll.1;
            let (x0, x1) = (self.bounds.x0 + inset, self.bounds.x1 - inset);
            let row = Rect::new(
                x0,
                y + inset,
                x1,
                y + inset + f64::from(crate::widgets::ROW_H),
            );
            crate::widgets::ThemeRow::from_spec(s, true).draw_snapshot(scene, t, tr, theme, row);
            self.specimen.set_spec(s);
            let height = f64::from(self.specimen.narrow_intrinsic_height(theme.text_scale));
            let specimen_top = row.y1 + inset;
            self.specimen.draw_snapshot(
                scene,
                t,
                tr,
                Rect::new(x0, specimen_top, x1, specimen_top + height),
            );
            // The token list is text and chips: one `s4` of padding inside the
            // panel's content edge so its glyphs breathe like a card's.
            let tokens_inset = f64::from(Spacing::BASE.s4);
            let tokens_top = specimen_top + height + inset;
            crate::customizer::draw_snapshot_tokens(
                scene,
                t,
                tr,
                theme,
                s,
                Rect::new(
                    self.bounds.x0 + tokens_inset,
                    tokens_top,
                    self.bounds.x1 - tokens_inset,
                    self.bounds.y1.max(tokens_top),
                ),
            );
        } else {
            tr.draw_text(
                scene,
                "No theme library recorded in this version",
                16.,
                theme.text_dim,
                t * Affine::translate((self.bounds.x0 + 16., self.bounds.y0 + 16.)),
                Some(self.bounds.width() as f32 - 32.),
            );
        }
        scene.pop_layer();
    }
    fn navigate(&mut self, command: PreviewNavigation) -> bool {
        let pages = self.accessibility().pages;
        navigate_pages(&mut self.view, command, &pages)
    }
    fn view(&self) -> PreviewView {
        self.view.clone()
    }
    fn accessibility(&self) -> PreviewAccessibility {
        let description = self.specs.iter().find(|s|Some(&s.id)==self.view.page.as_ref()).map_or_else(||"No theme library recorded in this version".into(), |s| {
            let mut text = format!("{}\nTheme library only; applying the OS theme is separate.\nFont: {}\nRadius: {}\nSpacing: {}",s.name,s.font,s.radius_norm,s.spacing_norm);
            for token in TokenId::ALL { use std::fmt::Write as _; let _ = write!(text,"\n{}: {}",token.label(),crate::model::hex(s.token(token))); }
            text
        });
        PreviewAccessibility {
            description,
            pages: self
                .specs
                .iter()
                .map(|s| (s.id.clone(), s.name.clone()))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_catalog_scroll_reaches_last_painted_label_at_both_widths() {
        let spec = crate::model::seed_custom();
        let mut preview = LibraryPreview {
            specs: vec![spec.clone()],
            view: PreviewView {
                page: Some(spec.id),
                ..Default::default()
            },
            bounds: Rect::ZERO,
            specimen: crate::preview::PreviewPane::new(),
        };
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        for width in [360., 1000.] {
            preview.view.scroll = (0., 0.);
            let clip = Rect::new(0., 0., width, 240.);
            preview.layout(clip, &mut tr, &theme).unwrap();
            let mut before = Scene::new();
            preview.draw(&mut before, Affine::IDENTITY, &mut tr, &theme);
            let last = before.encoding().resources.glyph_runs.last().unwrap();
            let y = f64::from(last.transform.translation[1]);
            assert!(y > clip.y1);
            preview.navigate(PreviewNavigation::Scroll {
                dx: 0.,
                dy: -(y - 100.),
            });
            let mut after = Scene::new();
            preview.draw(&mut after, Affine::IDENTITY, &mut tr, &theme);
            let last = after.encoding().resources.glyph_runs.last().unwrap();
            let reached = f64::from(last.transform.translation[1]);
            assert!(reached >= clip.y0 && reached < clip.y1);
            assert!((reached - 100.).abs() < 0.01);
        }
    }
}
