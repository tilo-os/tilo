//! The signature a person signs with: the name they sign as and the strokes
//! they drew, composed once and reused on every document. It is theirs alone
//! — a record in their own room that follows them to their other devices and
//! never reaches a collaborator on a document they review together.

use automerge::transaction::Transactable as _;
use automerge::{ROOT, ReadDoc as _, Value};
use serde::{Deserialize, Serialize};
use ui::tilo_actions::{Action, ActionError, Doc, ExecutionLocus};
use ui::tilo_resource::{ModelError, Resource};

/// The person's signature: whom they sign as and what they drew.
pub struct SignatureProfile;

/// The signature as the composer shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SignatureState {
    /// The name typed signatures carry; empty until the person sets one.
    pub signer: String,
    /// The drawn signature, polylines in normalised `[0,1]²` pad space;
    /// empty until the person draws.
    pub strokes: Vec<Vec<(f64, f64)>>,
}

/// An edit of the person's signature.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SignatureAction {
    /// Sign as `name` from now on.
    SetSigner { name: String },
    /// Replace the drawn signature; no strokes clears it.
    SetStrokes { strokes: Vec<Vec<(f64, f64)>> },
}

/// The most points one drawn signature keeps.
const MAX_POINTS: usize = 20_000;

impl Action for SignatureAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            SignatureAction::SetSigner { name } => {
                doc.put(ROOT, "signer", name.trim())
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
            }
            SignatureAction::SetStrokes { strokes } => {
                let points: usize = strokes.iter().map(Vec::len).sum();
                if points > MAX_POINTS
                    || strokes
                        .iter()
                        .flatten()
                        .any(|(x, y)| !x.is_finite() || !y.is_finite())
                {
                    return Err(ActionError::Refused(
                        "the drawn signature is not a signature".into(),
                    ));
                }
                // One value: a redrawn signature replaces the last one whole.
                let encoded = serde_json::to_string(strokes)
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
                doc.put(ROOT, "strokes", encoded)
                    .map_err(|error| ActionError::Refused(error.to_string()))?;
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

fn read_text(doc: &Doc, key: &str) -> Option<String> {
    match doc.get(ROOT, key) {
        Ok(Some((Value::Scalar(scalar), _))) => scalar.as_str().map(str::to_owned),
        _ => None,
    }
}

impl Resource for SignatureProfile {
    const KIND: &'static str = "pdf.signature-profile";
    const LABEL: &'static str = "Signature";
    const ACTION_TYPE: &'static str = "pdf_viewer.signature";

    type State = SignatureState;
    type Action = SignatureAction;

    fn read(doc: &Doc) -> Result<SignatureState, ModelError> {
        Ok(SignatureState {
            signer: read_text(doc, "signer").unwrap_or_default(),
            strokes: read_text(doc, "strokes")
                .and_then(|encoded| serde_json::from_str(&encoded).ok())
                .unwrap_or_default(),
        })
    }

    fn register(registry: &mut ui::tilo_actions::ActionRegistry) {
        registry.register::<SignatureAction>(Self::ACTION_TYPE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::Replica;

    #[test]
    fn the_signature_keeps_its_signer_and_strokes_and_refuses_garbage() {
        let mut replica = Replica::<SignatureProfile>::new();
        replica
            .apply_action(&SignatureAction::SetSigner {
                name: " Ada Lovelace ".into(),
            })
            .unwrap();
        let strokes = vec![vec![(0.1, 0.5), (0.9, 0.4)]];
        replica
            .apply_action(&SignatureAction::SetStrokes {
                strokes: strokes.clone(),
            })
            .unwrap();
        assert_eq!(
            replica.state().unwrap(),
            SignatureState {
                signer: "Ada Lovelace".into(),
                strokes,
            }
        );
        assert!(
            replica
                .apply_action(&SignatureAction::SetStrokes {
                    strokes: vec![vec![(f64::NAN, 0.0)]],
                })
                .is_err()
        );
    }
}
