//! Frozen domain History presentation and declared authored-content restoration.
use automerge::{AutoCommit, ObjType, ReadDoc, ScalarValue, Value};
use ui::history_mode::adapter::{
    FieldType, map_collection as collection, navigate_pages, validate_fields,
};
use ui::history_mode::restore::{HistoryRestoreRoot, HistoryRestoreSchema, RestoreValueType};
use ui::history_mode::{
    HistoryDefinition, HistoryPresentation, HistoryPreview, HistoryScopeDefinition,
    PreviewAccessibility, PreviewInput, PreviewNavigation, PreviewView,
};
use ui::paint::PaintCtx;
use ui::widget::{AvatarFace, PropertyListFace, PropertyRow};
use ui::{TextRenderer, TextRole, Theme};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
    peniko::{BlendMode, Fill},
};

use crate::detail::kind_label;
pub static HISTORY: HistoryDefinition = HistoryDefinition {
    app_id: "so.tilo.contacts",
    scopes: &[HistoryScopeDefinition {
        key: "contacts",
        title: "Address book",
        presentation: HistoryPresentation::Domain { create: preview },
        content_schema: Some(&SCHEMA),
    }],
};
pub static SCHEMA: HistoryRestoreSchema = HistoryRestoreSchema {
    id: "contacts.content",
    version: 1,
    roots: &[HistoryRestoreRoot {
        key: "contacts",
        required: false,
        expected_type: RestoreValueType::Object(ObjType::Map),
    }],
    selectors: &[],
    preserved_roots: &[],
    preserved: &[],
    validate,
    validate_result: validate,
};

fn validate(doc: &AutoCommit) -> Result<(), String> {
    let Some(map) = collection(doc, "contacts")? else {
        return Ok(());
    };
    for key in doc.keys(&map) {
        key.parse::<crate::model::ContactId>()
            .map_err(|_| "Invalid contact identity")?;
        let (Value::Object(ObjType::Map), row) = doc
            .get(&map, &key)
            .map_err(|e| e.to_string())?
            .ok_or("Missing contact")?
        else {
            return Err("Invalid contact row".into());
        };
        validate_fields(
            doc,
            &row,
            &[
                ("first", FieldType::Text),
                ("last", FieldType::Text),
                ("org", FieldType::Text),
                ("title", FieldType::Text),
                ("website", FieldType::Text),
                ("notes", FieldType::Text),
                ("birthday", FieldType::String),
                ("favorite", FieldType::Boolean),
                ("deleted", FieldType::Boolean),
                ("groups", FieldType::List),
                ("phones", FieldType::List),
                ("emails", FieldType::List),
                ("addresses", FieldType::List),
            ],
        )?;
        for field in ["groups", "phones", "emails", "addresses"] {
            if let Some((_, list)) = doc.get(&row, field).map_err(|e| e.to_string())? {
                let mut identities = std::collections::HashSet::new();
                for i in 0..doc.length(&list) {
                    match doc.get(&list, i).map_err(|e| e.to_string())? {
                        Some((Value::Scalar(v), _))
                            if field == "groups" && matches!(v.as_ref(), ScalarValue::Str(_)) => {}
                        Some((Value::Object(ObjType::Map), item)) => {
                            let allowed: &[(&str, FieldType)] = match field {
                                "groups" => {
                                    &[("id", FieldType::String), ("value", FieldType::String)]
                                }
                                "addresses" => &[
                                    ("id", FieldType::String),
                                    ("label", FieldType::String),
                                    ("street", FieldType::String),
                                    ("city", FieldType::String),
                                    ("region", FieldType::String),
                                    ("postal", FieldType::String),
                                    ("country", FieldType::String),
                                ],
                                _ => &[
                                    ("id", FieldType::String),
                                    ("label", FieldType::String),
                                    ("value", FieldType::Text),
                                ],
                            };
                            validate_fields(doc, &item, allowed)?;
                            if let Some((v, _)) = doc.get(&item, "id").map_err(|e| e.to_string())? {
                                let id = v
                                    .as_str()
                                    .ok_or("Invalid repeated-field identity")?
                                    .parse::<uuid::Uuid>()
                                    .map_err(|_| "Invalid repeated-field identity")?;
                                if id.is_nil() || !identities.insert(id) {
                                    return Err("Duplicate or nil repeated-field identity".into());
                                }
                            }
                        }
                        _ => return Err(format!("Invalid contact {field} row")),
                    }
                }
            }
        }
    }
    Ok(())
}
fn preview(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    let doc = AutoCommit::load(input.snapshot.bytes()).map_err(|e| e.to_string())?;
    validate(&doc)?;
    let mut contacts = crate::doc::ContactsDoc::list_contacts(&doc);
    contacts.sort_by_key(crate::model::Contact::display_name);
    let mut p = ContactsPreview {
        contacts,
        bounds: Rect::ZERO,
        view: input.initial_view,
    };
    if !p
        .contacts
        .iter()
        .any(|c| Some(c.id.to_string()) == p.view.page)
    {
        p.view.page = p.contacts.first().map(|c| c.id.to_string());
    }
    Ok(Box::new(p))
}
struct ContactsPreview {
    contacts: Vec<crate::model::Contact>,
    bounds: Rect,
    view: PreviewView,
}
impl HistoryPreview for ContactsPreview {
    fn layout(&mut self, bounds: Rect, _: &mut TextRenderer, _: &Theme) -> Result<(), String> {
        self.bounds = bounds;
        Ok(())
    }
    fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1., t, &self.bounds);
        // Page gutter `s6`, avatar `s16`, avatar ↔ first field `s3` (HIG §7b).
        let sp = &theme.tokens.spacing;
        let gutter = f64::from(sp.s6);
        let avatar = f64::from(sp.s16);
        let x = self.bounds.x0 + gutter;
        let x1 = self.bounds.x1 - gutter;
        let mut y = self.bounds.y0 + gutter - self.view.scroll.1;
        let ctx = &mut PaintCtx::new(scene, t, tr, theme);
        if let Some(c) = self
            .contacts
            .iter()
            .find(|c| Some(c.id.to_string()) == self.view.page)
        {
            let name = c.display_name();
            let identity = c.id.to_string();
            AvatarFace::new(&name, &identity).paint(ctx, Rect::new(x, y, x + avatar, y + avatar));
            y += avatar + f64::from(sp.s3);
            let favorite = if c.favorite { "Yes" } else { "No" };
            let mut owned: Vec<(String, String)> = vec![
                ("Name".into(), name.clone()),
                ("Job title".into(), c.title.clone()),
                ("Organization".into(), c.org.clone()),
                ("Favorite".into(), favorite.into()),
            ];
            owned.extend(c.groups.iter().map(|g| ("Group".into(), g.value.clone())));
            owned.extend(c.phones.iter().map(|row| {
                (
                    format!("{} phone", kind_label(&row.label)),
                    row.value.clone(),
                )
            }));
            owned.extend(c.emails.iter().map(|row| {
                (
                    format!("{} email", kind_label(&row.label)),
                    row.value.clone(),
                )
            }));
            owned.extend(c.addresses.iter().map(|a| {
                (
                    format!("{} address", kind_label(&a.label)),
                    crate::detail::address_lines(a),
                )
            }));
            if let Some(b) = c.birthday {
                owned.push(("Birthday".into(), b.pretty()));
            }
            owned.push(("Website".into(), c.website.clone()));
            owned.push(("Notes".into(), c.notes.clone()));
            let rows: Vec<PropertyRow<'_>> = owned
                .iter()
                .filter(|(_, value)| !value.trim().is_empty())
                .map(|(label, value)| PropertyRow::new(label, value))
                .collect();
            let face = PropertyListFace::new(&rows);
            let width = (x1 - x).max(0.0);
            let height = face.height(ctx.text, theme, width);
            face.paint(ctx, Rect::new(x, y, x1, y + height));
        } else {
            ctx.text_role(
                "No contacts in this version",
                TextRole::Body,
                theme.tokens.colors.text_dim,
                Point::new(x, y),
                Some((x1 - x) as f32),
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
        let description = self
            .contacts
            .iter()
            .find(|c| Some(c.id.to_string()) == self.view.page)
            .map_or_else(
                || "No contacts in this version".into(),
                |c| {
                    let mut rows = vec![
                        c.display_name(),
                        c.title.clone(),
                        c.org.clone(),
                        c.website.clone(),
                        c.notes.clone(),
                        format!("Favorite: {}", c.favorite),
                    ];
                    rows.extend(c.groups.iter().map(|g| g.value.clone()));
                    rows.extend(
                        c.phones
                            .iter()
                            .chain(&c.emails)
                            .map(|r| format!("{}: {}", r.label, r.value)),
                    );
                    rows.extend(c.addresses.iter().map(crate::model::Address::one_line));
                    if let Some(b) = c.birthday {
                        rows.push(b.pretty());
                    }
                    rows.join("\n")
                },
            );
        PreviewAccessibility {
            description,
            pages: self
                .contacts
                .iter()
                .map(|c| (c.id.to_string(), c.display_name()))
                .collect(),
        }
    }
}

// Closed, local navigation only. The host owns the accessible page controls at
// every width; no pointer/key events or live Widget actions enter this object.
