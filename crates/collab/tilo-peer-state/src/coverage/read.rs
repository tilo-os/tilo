//! The governed-checkpoint coverage check.
use crate::workspace::read::{ReadError, Selection, WorkspaceRead, WorkspaceValue};
use std::collections::BTreeMap;

/// One per-route value (`super::FIELD_V2`).
fn decode_row(bytes: &[u8]) -> Result<super::RouteValue, postcard::Error> {
    match super::decode_value(bytes) {
        Ok(value) => Ok(value),
        Err(_) => tilo_failure::ignored!(
            "the fallback's error is the row's error, returned to the caller",
            postcard::from_bytes(bytes)
        ),
    }
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CoverageMatchError {
    Decode(postcard::Error),
    NoMatch,
}
/// How a stored checkpoint stands against its route's governed coverage.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Governed {
    /// No coverage or no checkpoint to compare, or the checkpoint is the
    /// covered one.
    Current,
    /// The coverage names a checkpoint the document has not admitted yet:
    /// coverage and checkpoint arrive as separate frames.
    Arriving,
}
/// Check a stored checkpoint against its route's governed coverage row, if
/// any. A checkpoint older than coverage whose body the document has not
/// admitted (`covered`, per hex author) is arriving; one that differs from
/// coverage it already admitted is refused.
pub(crate) fn governed_snapshot_matches(
    workspace: Option<&impl WorkspaceRead>,
    (app, record): (&str, &str),
    covered: &BTreeMap<String, u64>,
    snapshot: Option<&[u8]>,
) -> Result<Governed, CoverageMatchError> {
    let (Some(snapshot), Some(workspace)) = (snapshot, workspace) else {
        return Ok(Governed::Current);
    };
    let mut governed = Governed::Current;
    let mut resolve = |row: &super::RouteValue| -> Result<(), CoverageMatchError> {
        if crate::document::snapshot_hash(snapshot).is_ok_and(|hash| hash == row.payload_hash()) {
            Ok(())
        } else if row.admitted_in(covered) {
            Err(CoverageMatchError::NoMatch)
        } else {
            governed = Governed::Arriving;
            Ok(())
        }
    };
    // Matched against the stored keys without allocating the route key.
    let is_route = |key: &str| {
        key.as_bytes()
            .strip_prefix(app.as_bytes())
            .and_then(|tail| tail.strip_prefix(&[0]))
            == Some(record.as_bytes())
    };
    let mut routed = false;
    let v2 = workspace.visit_property(
        &automerge::ROOT,
        super::FIELD_V2,
        Selection::Winner,
        |value| {
            let WorkspaceValue::Object { object, .. } = value else {
                return Ok(());
            };
            let keys = workspace.visit_keys(&object, |key| {
                if routed || !is_route(key) {
                    return Ok(());
                }
                let inner = workspace.visit_property(&object, key, Selection::Winner, |entry| {
                    let WorkspaceValue::Bytes(bytes) = entry else {
                        return Ok(());
                    };
                    let row = decode_row(bytes).map_err(CoverageMatchError::Decode)?;
                    routed = true;
                    resolve(&row)
                });
                match inner {
                    Ok(_) | Err(ReadError::Malformed) => Ok(()),
                    Err(ReadError::Visitor(error)) => Err(error),
                }
            });
            match keys {
                Ok(_) | Err(ReadError::Malformed) => Ok(()),
                Err(ReadError::Visitor(error)) => Err(error),
            }
        },
    );
    match v2 {
        Ok(_) | Err(ReadError::Malformed) => Ok(governed),
        Err(ReadError::Visitor(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests;
