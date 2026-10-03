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
use ui::paint::PaintCtx;
use ui::tokens::TextRole;
use ui::widget::{PropertyListFace, PropertyRow};
use ui::{TextRenderer, Theme};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
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
                    .is_none_or(|(v, _)| v.as_str().is_none())
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
        let spacing = &theme.tokens.spacing;
        let gutter = f64::from(spacing.rhythm.group);
        let width = (self.bounds.width() - gutter * 2.0).max(1.0);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1., t, &self.bounds);
        let x = self.bounds.x0 + gutter - self.view.scroll.0;
        let mut y = self.bounds.y0 + gutter - self.view.scroll.1;
        let snippet = self
            .snippets
            .iter()
            .find(|s| Some(s.id.to_string()) == self.view.page);
        if let Some(s) = snippet {
            let mut ctx = PaintCtx::new(scene, t, tr, theme);
            ctx.text_role(
                &s.title,
                TextRole::Title,
                theme.text_primary,
                Point::new(x, y),
                Some(width as f32),
            );
            y += ctx
                .measure_role(&s.title, TextRole::Title, Some(width as f32))
                .height
                + f64::from(spacing.rhythm.section);
            let facts = facts(s);
            let rows: Vec<PropertyRow<'_>> = facts
                .iter()
                .map(|(label, value)| PropertyRow::new(label, value))
                .collect();
            let list = PropertyListFace::new(&rows);
            let height = list.height(ctx.text, theme, width);
            list.paint(&mut ctx, Rect::new(x, y, x + width, y + height));
            y += height + f64::from(spacing.rhythm.section);
            if !s.description.trim().is_empty() {
                ctx.text_role(
                    &s.description,
                    TextRole::Body,
                    theme.text_dim,
                    Point::new(x, y),
                    Some(width as f32),
                );
                y += ctx
                    .measure_role(&s.description, TextRole::Body, Some(width as f32))
                    .height
                    + f64::from(spacing.rhythm.section);
            }
            let height = crate::draw::code_height(tr, theme, width, s);
            crate::draw::paint_code(
                scene,
                t,
                tr,
                theme,
                Rect::new(x, y, x + width, y + height),
                s,
            );
        } else {
            PaintCtx::new(scene, t, tr, theme).text_role(
                &crate::tr("snippets-history-empty"),
                TextRole::Body,
                theme.text_dim,
                Point::new(x, y),
                Some(width as f32),
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
                    || crate::tr("snippets-history-empty"),
                    |s| {
                        let mut lines = vec![s.title.clone()];
                        lines.extend(
                            facts(s)
                                .into_iter()
                                .map(|(label, value)| format!("{label}: {value}")),
                        );
                        lines.push(s.description.clone());
                        lines.push(s.body.clone());
                        lines.join("\n")
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

/// A past version's facts, as label and value.
fn facts(s: &crate::model::Snippet) -> Vec<(String, String)> {
    use crate::tr;
    let mut facts = vec![(tr("snippets-fact-language"), s.language.name())];
    if !s.folder.is_empty() {
        facts.push((tr("snippets-fact-folder"), s.folder.clone()));
    }
    if !s.tags.is_empty() {
        facts.push((tr("snippets-fact-tags"), s.tags.join(", ")));
    }
    facts.push((
        tr("snippets-fact-favorite"),
        tr(if s.favorite {
            "snippets-yes"
        } else {
            "snippets-no"
        }),
    ));
    facts.push((
        tr("snippets-fact-copied"),
        crate::tr_count("snippets-uses", s.uses as usize),
    ));
    facts
}

// Closed, local navigation only. The host owns the accessible page controls at
// every width; no pointer/key events or live Widget actions enter this object.
