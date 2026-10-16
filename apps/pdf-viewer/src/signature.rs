//! The signature a person signs with: the name they sign as and the strokes
//! they drew, composed once and reused on every document. It is theirs alone
//! — a record in their own room that follows them to their other devices and
//! never reaches a collaborator on a document they review together.

use ui::tilo_resource::{self, Json, Model, Refusal, resource};

/// The person's signature: whom they sign as and what they drew.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = SignatureProfile::valid)]
pub struct SignatureProfile {
    /// The name typed signatures carry; empty until the person sets one.
    #[model(atom)]
    pub signer: String,
    /// The drawn signature, polylines in normalised `[0,1]²` pad space;
    /// empty until the person draws. Replaced whole when redrawn.
    pub strokes: Json<Vec<Vec<(f64, f64)>>>,
}

/// The most points one drawn signature keeps.
const MAX_POINTS: usize = 20_000;

#[resource(kind = "pdf.signature-profile", label = "Signature", params = signature)]
impl SignatureProfile {
    /// Sign as
    ///
    /// Signs as `name` from now on.
    #[action]
    pub fn set_signer(&mut self, #[param("The name to sign as")] name: String) {
        name.trim().clone_into(&mut self.signer);
    }

    /// Draw signature
    ///
    /// Replaces the drawn signature; no strokes clears it.
    #[action]
    pub fn set_strokes(
        &mut self,
        #[param("Polylines in the unit square, each a list of x, y points")] strokes: Vec<
            Vec<(f64, f64)>,
        >,
    ) {
        self.strokes = Json(strokes);
    }
}

impl SignatureProfile {
    fn valid(&self) -> Result<(), Refusal> {
        let strokes = &self.strokes.0;
        let points: usize = strokes.iter().map(Vec::len).sum();
        if points > MAX_POINTS
            || strokes
                .iter()
                .flatten()
                .any(|(x, y)| !x.is_finite() || !y.is_finite())
        {
            return Err(Refusal::new("the drawn signature is not a signature"));
        }
        Ok(())
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
            .apply(SignatureProfile::SET_SIGNER.with(" Ada Lovelace ".to_owned()))
            .unwrap();
        let strokes = vec![vec![(0.1, 0.5), (0.9, 0.4)]];
        replica
            .apply(SignatureProfile::SET_STROKES.with(strokes.clone()))
            .unwrap();
        assert_eq!(
            replica.state().unwrap(),
            SignatureProfile {
                signer: "Ada Lovelace".into(),
                strokes: Json(strokes),
            }
        );
        let too_long = vec![vec![(0.5, 0.5); MAX_POINTS + 1]];
        assert!(
            replica
                .apply(SignatureProfile::SET_STROKES.with(too_long))
                .is_err()
        );
    }
}
