//! History adapter for the complete unsubmitted container-create draft.

use crate::doc::CreateField;
use crate::presentation::DraftProjection;
use automerge::{AutoCommit, ObjId, ObjType, Prop, ROOT, ReadDoc, ScalarValue, Value};
use ui::history_mode::adapter::navigate_pages;
use ui::history_mode::restore::{
    HistoryMapField, HistoryMapOverlay, HistoryRestoreNode, HistoryRestoreSchema,
    HistoryRootPattern, HistoryRootSelector, RestoreValueType,
};
use ui::history_mode::{
    HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition,
    PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView,
};
use ui::{TextRenderer, Theme};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{BlendMode, Fill};

pub const LIMITATION: &str = "Podman engine state, containers, images, volumes, distroboxes, installed-image suggestions, logs, stats, inspect data, and operation progress are device-local live state unavailable in History.";

const CREATE_NODE: HistoryRestoreNode = HistoryRestoreNode::Whole {
    expected_type: RestoreValueType::Object(ObjType::Map),
};

pub static SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "containers.create-draft",
    version: 1,
    roots: &[],
    selectors: &[HistoryRootSelector {
        pattern: HistoryRootPattern::Exact("view"),
        required: false,
        node: HistoryRestoreNode::MapOverlay(&HistoryMapOverlay {
            fields: &[HistoryMapField {
                key: "create",
                required: false,
                node: CREATE_NODE,
            }],
            entries: None,
        }),
    }],
    preserved_roots: &[],
    preserved: &[],
    validate,
    validate_result: validate,
};

pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.containers",
    scopes: &[HistoryScopeDefinition {
        key: "create",
        title: "Container draft",
        presentation: HistoryPresentation::Domain { create: preview },
        content_schema: Some(&SCHEMA),
    }],
};

fn single<'a>(
    doc: &'a AutoCommit,
    object: &ObjId,
    prop: impl Into<Prop>,
) -> Result<Option<(Value<'a>, ObjId)>, String> {
    let mut values = doc
        .get_all(object, prop)
        .map_err(|error| error.to_string())?;
    if values.len() > 1 {
        return Err("Container draft has conflicting values".into());
    }
    Ok(values.pop())
}

fn map_at(doc: &AutoCommit, parent: &ObjId, key: &str) -> Result<Option<ObjId>, String> {
    match single(doc, parent, key)? {
        None => Ok(None),
        Some((Value::Object(ObjType::Map), object)) => Ok(Some(object)),
        Some(_) => Err(format!("{key} must be a map")),
    }
}

fn validate_tree(doc: &AutoCommit, object: &ObjId, depth: usize) -> Result<(), String> {
    // Match the shared exact-content encoder's compatibility boundary. The
    // draft adds no app-specific size or scalar policy of its own.
    if depth > 256 {
        return Err("Container draft nesting exceeds shared History limits".into());
    }
    let kind = doc.object_type(object).map_err(|error| error.to_string())?;
    let props: Vec<Prop> = match kind {
        ObjType::Map | ObjType::Table => doc.keys(object).map(Prop::Map).collect(),
        ObjType::List | ObjType::Text => (0..doc.length(object)).map(Prop::Seq).collect(),
    };
    for prop in props {
        let (value, child) =
            single(doc, object, prop)?.ok_or("Missing container draft descendant")?;
        if kind == ObjType::Text
            && !matches!(value, Value::Scalar(ref scalar) if matches!(scalar.as_ref(), ScalarValue::Str(_)))
        {
            return Err("Container draft Text contains unsupported rich elements".into());
        }
        if matches!(value, Value::Object(_)) {
            validate_tree(doc, &child, depth.saturating_add(1))?;
        }
    }
    Ok(())
}

pub(crate) fn validate(doc: &AutoCommit) -> Result<(), String> {
    let Some(view) = map_at(doc, &ROOT, "view")? else {
        return Ok(());
    };
    let Some(create) = map_at(doc, &view, "create")? else {
        return Ok(());
    };
    for field in CreateField::ALL {
        match single(doc, &create, field.key())? {
            None | Some((Value::Object(ObjType::Text), _)) => {}
            Some(_) => {
                return Err(format!(
                    "Container draft field {} must be collaborative Text",
                    field.key()
                ));
            }
        }
    }
    validate_tree(doc, &create, 0)
}

fn value_type(value: &Value<'_>) -> &'static str {
    match value {
        Value::Object(ObjType::Map) => "map",
        Value::Object(ObjType::List) => "list",
        Value::Object(ObjType::Text) => "text",
        Value::Object(ObjType::Table) => "table",
        Value::Scalar(value) => match value.as_ref() {
            ScalarValue::Str(_) => "string",
            ScalarValue::Bytes(_) => "bytes",
            ScalarValue::Int(_) => "integer",
            ScalarValue::Uint(_) => "unsigned integer",
            ScalarValue::F64(_) => "number",
            ScalarValue::Counter(_) => "counter",
            ScalarValue::Timestamp(_) => "timestamp",
            ScalarValue::Boolean(_) => "boolean",
            ScalarValue::Null => "null",
            ScalarValue::Unknown { .. } => "unknown scalar",
        },
    }
}

fn describe_unknown(
    doc: &AutoCommit,
    object: &ObjId,
    prefix: &str,
    out: &mut Vec<String>,
) -> Result<(), String> {
    for key in doc.keys(object) {
        if prefix.is_empty() && CreateField::ALL.into_iter().any(|field| field.key() == key) {
            continue;
        }
        let (value, child) =
            single(doc, object, key.as_str())?.ok_or("Missing container draft descendant")?;
        let path = if prefix.is_empty() {
            key
        } else {
            format!("{prefix}.{key}")
        };
        out.push(format!("{path} ({})", value_type(&value)));
        if matches!(value, Value::Object(ObjType::Map | ObjType::Table)) {
            describe_unknown(doc, &child, &path, out)?;
        }
    }
    Ok(())
}

fn projection(doc: &AutoCommit) -> Result<DraftProjection, String> {
    let create = map_at(doc, &ROOT, "view")?
        .map(|view| map_at(doc, &view, "create"))
        .transpose()?
        .flatten();
    let fields = CreateField::ALL
        .into_iter()
        .map(|field| {
            let value = create.as_ref().and_then(|map| {
                single(doc, map, field.key())
                    .ok()
                    .flatten()
                    .and_then(|(value, object)| {
                        matches!(value, Value::Object(ObjType::Text))
                            .then(|| doc.text(&object).ok())
                            .flatten()
                    })
            });
            (field, value)
        })
        .collect();
    let mut unknown_descendants = Vec::new();
    if let Some(create) = create {
        describe_unknown(doc, &create, "", &mut unknown_descendants)?;
    }
    Ok(DraftProjection {
        fields,
        unknown_descendants,
    })
}

fn preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let doc = AutoCommit::load(input.snapshot.bytes()).map_err(|error| error.to_string())?;
    validate(&doc)?;
    let mut view = input.initial_view;
    view.page = Some("create".into());
    Ok(Box::new(CreateDraftPreview {
        projection: projection(&doc)?,
        view,
        bounds: Rect::ZERO,
    }))
}

struct CreateDraftPreview {
    projection: DraftProjection,
    view: PreviewView,
    bounds: Rect,
}

impl HistoryPreview for CreateDraftPreview {
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
        let scrolled = transform * Affine::translate(self.view.scroll);
        crate::draw::set_base(scrolled);
        text.draw_text(
            scene,
            LIMITATION,
            13.0,
            theme.text_dim,
            scrolled * Affine::translate((self.bounds.x0 + 16.0, self.bounds.y0 + 12.0)),
            Some((self.bounds.width() - 32.0).max(1.0) as f32),
        );
        self.projection.draw(
            scene,
            text,
            theme,
            Rect::new(
                self.bounds.x0,
                self.bounds.y0 + 72.0,
                self.bounds.x1,
                self.bounds.y0 + 640.0,
            ),
        );
        scene.pop_layer();
    }

    fn navigate(&mut self, command: PreviewNavigation) -> bool {
        navigate_pages(
            &mut self.view,
            command,
            &[("create".into(), "Container draft".into())],
        )
    }

    fn view(&self) -> PreviewView {
        self.view.clone()
    }

    fn accessibility(&self) -> PreviewAccessibility {
        PreviewAccessibility {
            description: format!("{}\n{LIMITATION}", self.projection.accessibility()),
            pages: vec![("create".into(), "Container draft".into())],
        }
    }
}
