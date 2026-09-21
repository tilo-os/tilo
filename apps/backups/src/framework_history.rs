//! Frozen retention-policy History presentation and exact optional restoration.

use crate::doc::{BackupsDoc, PolicyField, StoredRetention};
use crate::presentation::RetentionPresentation;
use automerge::{AutoCommit, ROOT};
use ui::history_mode::adapter::navigate_pages;
use ui::history_mode::restore::{
    HistoryRestoreNode, HistoryRestoreSchema, HistoryRootPattern, HistoryRootSelector,
    RestoreValueType, ScalarType,
};
use ui::history_mode::{
    HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition,
    PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView,
};
use ui::{TextRenderer, Theme};
use vello::{
    Scene,
    kurbo::{Affine, Rect},
    peniko::{BlendMode, Fill},
};

pub const LIMITATION: &str = "Snapshot catalogue is device-local and unavailable in History.";

const INTEGER: HistoryRestoreNode = HistoryRestoreNode::Whole {
    expected_type: RestoreValueType::Scalar(ScalarType::Int),
};

pub static SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "backups.retention",
    version: 1,
    roots: &[],
    selectors: &[
        HistoryRootSelector {
            pattern: HistoryRootPattern::Exact("ret_hourly"),
            required: false,
            node: INTEGER,
        },
        HistoryRootSelector {
            pattern: HistoryRootPattern::Exact("ret_daily"),
            required: false,
            node: INTEGER,
        },
        HistoryRootSelector {
            pattern: HistoryRootPattern::Exact("ret_weekly"),
            required: false,
            node: INTEGER,
        },
        HistoryRootSelector {
            pattern: HistoryRootPattern::Exact("ret_monthly"),
            required: false,
            node: INTEGER,
        },
    ],
    preserved_roots: &[],
    preserved: &[],
    validate,
    validate_result: validate,
};

pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.backups",
    scopes: &[HistoryScopeDefinition {
        key: "retention",
        title: "Retention policy",
        presentation: HistoryPresentation::Domain { create: preview },
        content_schema: Some(&SCHEMA),
    }],
};

fn validate(doc: &AutoCommit) -> Result<(), String> {
    for field in PolicyField::ALL {
        match BackupsDoc::stored_retention(doc, &ROOT, field) {
            StoredRetention::Absent | StoredRetention::Int(_) => {}
            StoredRetention::WrongType => {
                return Err(format!("{} retention must be an integer", field.label()));
            }
            StoredRetention::Conflict => {
                return Err(format!("{} retention has a conflict", field.label()));
            }
        }
    }
    Ok(())
}

fn preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let doc = AutoCommit::load(input.snapshot.bytes()).map_err(|error| error.to_string())?;
    validate(&doc)?;
    let mut view = input.initial_view;
    view.page = Some("retention".into());
    Ok(Box::new(RetentionPreview {
        presentation: RetentionPresentation::from_doc(&doc, &ROOT),
        view,
        bounds: Rect::ZERO,
    }))
}

struct RetentionPreview {
    presentation: RetentionPresentation,
    view: PreviewView,
    bounds: Rect,
}

impl HistoryPreview for RetentionPreview {
    fn layout(
        &mut self,
        bounds: Rect,
        _text: &mut TextRenderer,
        _theme: &Theme,
    ) -> Result<(), String> {
        self.bounds = bounds;
        Ok(())
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        scene.fill(
            Fill::NonZero,
            transform,
            theme.bg_primary,
            None,
            &self.bounds,
        );
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &self.bounds,
        );
        let transform = transform * Affine::translate(self.view.scroll);
        text.draw_text(
            scene,
            "Retention policy",
            20.0,
            theme.text_primary,
            transform * Affine::translate((self.bounds.x0 + 16.0, self.bounds.y0 + 14.0)),
            None,
        );
        text.draw_text(
            scene,
            LIMITATION,
            13.0,
            theme.text_dim,
            transform * Affine::translate((self.bounds.x0 + 16.0, self.bounds.y0 + 46.0)),
            Some((self.bounds.width() - 32.0).max(1.0) as f32),
        );
        let panel = Rect::new(
            self.bounds.x0 + 8.0,
            self.bounds.y0 + 88.0,
            self.bounds.x1 - 8.0,
            self.bounds.y0
                + 88.0
                + RetentionPresentation::panel_height(f64::from(text.text_scale())),
        );
        self.presentation
            .draw_editor(scene, transform, text, theme, panel, None, None, false);
        scene.pop_layer();
    }

    fn navigate(&mut self, command: PreviewNavigation) -> bool {
        navigate_pages(
            &mut self.view,
            command,
            &[("retention".into(), "Retention policy".into())],
        )
    }

    fn view(&self) -> PreviewView {
        self.view.clone()
    }

    fn accessibility(&self) -> PreviewAccessibility {
        PreviewAccessibility {
            description: format!("{}\n{LIMITATION}", self.presentation.accessibility()),
            pages: vec![("retention".into(), "Retention policy".into())],
        }
    }
}
