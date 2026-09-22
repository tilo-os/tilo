//! Frozen domain History presentation and declared authored-content restoration.
use automerge::{AutoCommit, ObjType, ReadDoc, Value};
use ui::history_mode::adapter::{
    FieldType, map_collection as collection, navigate_pages, validate_fields,
};
use ui::history_mode::restore::{
    HistoryMapEntries, HistoryMapField, HistoryMapOverlay, HistoryMembership, HistoryRestoreNode,
    HistoryRestoreSchema, HistoryRootPattern, HistoryRootSelector, PreservedOnCreate,
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
pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.snippets",
    scopes: &[HistoryScopeDefinition {
        key: "snippets",
        title: "Snippet library",
        presentation: HistoryPresentation::Domain { create: preview },
        content_schema: Some(&SCHEMA),
    }],
};
pub static SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "snippets.content",
    version: 1,
    roots: &[],
    selectors: &[HistoryRootSelector {
        pattern: HistoryRootPattern::Exact("snippets"),
        required: false,
        node: HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
            fields: &[],
            entries: Some(&HistoryMapEntries {
                key_limit_bytes: 36,
                membership: HistoryMembership::TargetAuthoritative,
                on_create: PreservedOnCreate::Omit,
                node: HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
                    entries: None,
                    fields: &[
                        HistoryMapField {
                            key: "title",
                            required: false,
                            node: HistoryRestoreNode::Whole {
                                expected_type: RestoreValueType::Object(ObjType::Text),
                            },
                        },
                        HistoryMapField {
                            key: "description",
                            required: false,
                            node: HistoryRestoreNode::Whole {
                                expected_type: RestoreValueType::Object(ObjType::Text),
                            },
                        },
                        HistoryMapField {
                            key: "body",
                            required: false,
                            node: HistoryRestoreNode::Whole {
                                expected_type: RestoreValueType::Object(ObjType::Text),
                            },
                        },
                        HistoryMapField {
                            key: "language",
                            required: false,
                            node: HistoryRestoreNode::Whole {
                                expected_type: RestoreValueType::Scalar(ScalarType::String),
                            },
                        },
                        HistoryMapField {
                            key: "folder",
                            required: false,
                            node: HistoryRestoreNode::Whole {
                                expected_type: RestoreValueType::Scalar(ScalarType::String),
                            },
                        },
                        HistoryMapField {
                            key: "favorite",
                            required: false,
                            node: HistoryRestoreNode::Whole {
                                expected_type: RestoreValueType::Scalar(ScalarType::Boolean),
                            },
                        },
                        HistoryMapField {
                            key: "tags",
                            required: false,
                            node: HistoryRestoreNode::Whole {
                                expected_type: RestoreValueType::Object(ObjType::List),
                            },
                        },
                    ],
                }),
            }),
        }),
    }],
    preserved_roots: &[],
    preserved: &[],
    validate,
    validate_result: validate,
};

fn validate(doc: &AutoCommit) -> Result<(), String> {
    let Some(map) = collection(doc, "snippets")? else {
        return Ok(());
    };
    for key in doc.keys(&map) {
        key.parse::<crate::model::SnippetId>()
            .map_err(|_| "Invalid snippet identity")?;
        let (Value::Object(ObjType::Map), row) = doc
            .get(&map, &key)
            .map_err(|e| e.to_string())?
            .ok_or("Missing snippet")?
        else {
            return Err("Invalid snippet row".into());
        };
        validate_fields(
            doc,
            &row,
            &[
                ("title", FieldType::Text),
                ("description", FieldType::Text),
                ("body", FieldType::Text),
                ("language", FieldType::String),
                ("folder", FieldType::String),
                ("favorite", FieldType::Boolean),
                ("tags", FieldType::List),
                ("uses", FieldType::Integer),
            ],
        )?;
        if let Some((_, tags)) = doc.get(&row, "tags").map_err(|e| e.to_string())? {
            for i in 0..doc.length(&tags) {
                if doc
                    .get(&tags, i)
                    .map_err(|e| e.to_string())?
                    .is_none_or(|(v, _)| v.to_str().is_none())
                {
                    return Err("Invalid snippet tag".into());
                }
            }
        }
    }
    Ok(())
}
fn preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let doc = AutoCommit::load(input.snapshot.bytes()).map_err(|e| e.to_string())?;
    validate(&doc)?;
    let mut snippets = crate::doc::SnippetsDoc::list(&doc);
    snippets.sort_by_key(|s| s.title.clone());
    let mut p = SnippetsPreview {
        snippets,
        bounds: Rect::ZERO,
        view: input.initial_view,
    };
    if !p
        .snippets
        .iter()
        .any(|s| Some(s.id.to_string()) == p.view.page)
    {
        p.view.page = p.snippets.first().map(|s| s.id.to_string());
    }
    Ok(Box::new(p))
}
struct SnippetsPreview {
    snippets: Vec<crate::model::Snippet>,
    bounds: Rect,
    view: PreviewView,
}
impl HistoryPreview for SnippetsPreview {
    fn layout(&mut self, bounds: Rect, _: &mut TextRenderer, _: &Theme) -> Result<(), String> {
        self.bounds = bounds;
        Ok(())
    }
    fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        scene.fill(Fill::NonZero, t, theme.bg_primary, None, &self.bounds);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1., t, &self.bounds);
        let x = self.bounds.x0 + 20. - self.view.scroll.0;
        let mut y = self.bounds.y0 + 20. - self.view.scroll.1;
        if let Some(s) = self
            .snippets
            .iter()
            .find(|s| Some(s.id.to_string()) == self.view.page)
        {
            for (line, size) in [
                (&s.title, 24.),
                (
                    &format!(
                        "{} · {} · {}",
                        s.language.label(),
                        s.folder,
                        s.tags.join(", ")
                    ),
                    14.,
                ),
                (&s.description, 14.),
                (
                    &format!("Favorite: {} · Recorded uses: {}", s.favorite, s.uses),
                    12.,
                ),
            ] {
                let width = (self.bounds.width() - 40.).max(1.) as f32;
                tr.draw_text(
                    scene,
                    line,
                    size,
                    theme.text_primary,
                    t * Affine::translate((x, y)),
                    Some(width),
                );
                y += f64::from(tr.measure(line, size, Some(width)).1) + 12.;
            }
            // The code body's line pitch at the current text scale — the same
            // rule the live app's metrics use — so the card holds its lines at
            // 2x; the card keeps its 24 px-per-line allowance at 1x.
            let line_h =
                crate::app::BODY_LINE + crate::draw::line_growth(tr, crate::app::BODY_FONT);
            let pitch = 24. + (line_h - crate::app::BODY_LINE);
            let height = (s.body.lines().count() as f64 * pitch + 40.).max(80.);
            crate::app::SnippetsApp::draw_code(
                scene,
                t,
                tr,
                theme,
                Rect::new(x, y, (self.bounds.x1 - 20.).max(x + 2000.), y + height),
                s,
                line_h,
            );
        } else {
            tr.draw_text(
                scene,
                "No snippets in this historical version",
                18.,
                theme.tokens.colors.text_dim,
                t * Affine::translate((x, y)),
                Some((self.bounds.width() - 40.) as f32),
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
        PreviewAccessibility {
            description: self
                .snippets
                .iter()
                .find(|s| Some(s.id.to_string()) == self.view.page)
                .map_or_else(
                    || "No snippets in this historical version".into(),
                    |s| {
                        format!(
                            "{}\n{} · {} · {}\n{}\n{}\nFavorite: {} · Recorded uses: {}",
                            s.title,
                            s.language.label(),
                            s.folder,
                            s.tags.join(", "),
                            s.description,
                            s.body,
                            s.favorite,
                            s.uses
                        )
                    },
                ),
            pages: self
                .snippets
                .iter()
                .map(|s| (s.id.to_string(), s.title.clone()))
                .collect(),
        }
    }
}

// Closed, local navigation only. The host owns the accessible page controls at
// every width; no pointer/key events or live Widget actions enter this object.
