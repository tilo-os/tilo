//! Pure adapter utilities. Domain allowlists, schemas and presentation remain local.
use super::{PreviewNavigation, PreviewView};
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

/// Explicit persisted field shape accepted by a domain's local allowlist.
#[derive(Clone, Copy, Debug)]
pub enum FieldType {
    Text,
    String,
    List,
    Boolean,
    Integer,
}

/// Validate only the fields explicitly declared by the caller.
/// # Errors
/// Returns an error for unknown fields, incompatible values or unreadable content.
pub fn validate_fields(
    doc: &AutoCommit,
    obj: &ObjId,
    allowed: &[(&str, FieldType)],
) -> Result<(), String> {
    for key in doc.keys(obj) {
        let kind = allowed
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, kind)| *kind)
            .ok_or_else(|| format!("Unsupported content field: {key}"))?;
        let (value, _) = doc
            .get(obj, &key)
            .map_err(|e| e.to_string())?
            .ok_or("Missing field")?;
        let valid = match (&value, kind) {
            (Value::Object(ObjType::Text), FieldType::Text)
            | (Value::Object(ObjType::List), FieldType::List) => true,
            (Value::Scalar(v), FieldType::String | FieldType::Text) => {
                matches!(v.as_ref(), ScalarValue::Str(_))
            }
            (Value::Scalar(v), FieldType::Boolean) => matches!(v.as_ref(), ScalarValue::Boolean(_)),
            (Value::Scalar(v), FieldType::Integer) => {
                matches!(v.as_ref(), ScalarValue::Int(_) | ScalarValue::Uint(_))
            }
            _ => false,
        };
        if !valid {
            return Err(format!("Unsupported type for {key}"));
        }
    }
    Ok(())
}

/// Read an optional keyed collection without normalizing its persisted shape.
/// # Errors
/// Returns an error if the root exists with a different shape or cannot be read.
pub fn map_collection(doc: &AutoCommit, key: &str) -> Result<Option<ObjId>, String> {
    match doc.get(ROOT, key).map_err(|e| e.to_string())? {
        None => Ok(None),
        Some((Value::Object(ObjType::Map), id)) => Ok(Some(id)),
        _ => Err(format!("{key}: expected a keyed map")),
    }
}

/// Apply closed local navigation; never accesses a document or live capability.
pub fn navigate_pages(
    view: &mut PreviewView,
    command: PreviewNavigation,
    pages: &[(String, String)],
) -> bool {
    let page = match command {
        PreviewNavigation::Scroll { dx, dy } if dx.is_finite() && dy.is_finite() => {
            view.scroll.0 = (view.scroll.0 - dx).clamp(0., 100_000.);
            view.scroll.1 = (view.scroll.1 - dy).clamp(0., 100_000.);
            return true;
        }
        PreviewNavigation::SelectPage(id) | PreviewNavigation::Reveal(id) => Some(id),
        PreviewNavigation::NextPage => pages
            .iter()
            .position(|(id, _)| Some(id) == view.page.as_ref())
            .and_then(|i| i.checked_add(1))
            .and_then(|i| pages.get(i))
            .map(|(id, _)| id.clone()),
        PreviewNavigation::PreviousPage => pages
            .iter()
            .position(|(id, _)| Some(id) == view.page.as_ref())
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| pages.get(i))
            .map(|(id, _)| id.clone()),
        _ => None,
    };
    if let Some(id) = page.filter(|id| pages.iter().any(|(key, _)| key == id)) {
        view.page = Some(id);
        view.scroll = (0., 0.);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable;

    #[test]
    fn adapter_validation_and_navigation_accept_only_declared_input() {
        // phase: typed_validation_accepts_only_declared_shapes_without_writes
        {
            let mut doc = AutoCommit::new();
            let row = doc.put_object(ROOT, "rows", ObjType::Map).unwrap();
            doc.put(&row, "text", "legacy text").unwrap();
            doc.put(&row, "flag", true).unwrap();
            let heads = doc.get_heads();
            assert!(
                validate_fields(
                    &doc,
                    &row,
                    &[("text", FieldType::Text), ("flag", FieldType::Boolean)]
                )
                .is_ok()
            );
            assert!(
                validate_fields(
                    &doc,
                    &row,
                    &[("text", FieldType::Integer), ("flag", FieldType::Boolean)]
                )
                .is_err()
            );
            assert!(validate_fields(&doc, &row, &[("text", FieldType::Text)]).is_err());
            assert_eq!(map_collection(&doc, "rows").unwrap(), Some(row));
            assert!(map_collection(&doc, "missing").unwrap().is_none());
            assert_eq!(doc.base_heads(), heads);
            assert_eq!(doc.pending_ops(), 0);
            doc.put_object(ROOT, "old", ObjType::List).unwrap();
            assert!(map_collection(&doc, "old").is_err());
        }
        // phase: closed_navigation_rejects_unknown_pages_and_nonfinite_scroll
        {
            let pages = vec![("a".into(), "First".into()), ("b".into(), "Second".into())];
            let mut view = PreviewView {
                page: Some("a".into()),
                ..Default::default()
            };
            assert!(!navigate_pages(
                &mut view,
                PreviewNavigation::PreviousPage,
                &pages
            ));
            assert!(!navigate_pages(
                &mut view,
                PreviewNavigation::Reveal("unknown".into()),
                &pages
            ));
            assert!(!navigate_pages(
                &mut view,
                PreviewNavigation::Scroll {
                    dx: f64::NAN,
                    dy: 0.
                },
                &pages
            ));
            assert!(navigate_pages(
                &mut view,
                PreviewNavigation::Scroll { dx: -12., dy: -30. },
                &pages
            ));
            assert_eq!(view.scroll, (12., 30.));
            assert!(navigate_pages(
                &mut view,
                PreviewNavigation::NextPage,
                &pages
            ));
            assert_eq!(view.page.as_deref(), Some("b"));
            assert_eq!(view.scroll, (0., 0.));
            assert!(!navigate_pages(
                &mut view,
                PreviewNavigation::NextPage,
                &pages
            ));
            assert!(navigate_pages(
                &mut view,
                PreviewNavigation::PreviousPage,
                &pages
            ));
            assert_eq!(view.page.as_deref(), Some("a"));
        }
    }
}
