//! The signature a person signs with: the name they sign as and the strokes
//! they drew, composed once and reused on every document. It is theirs alone
//! — a record in their own room that follows them to their other devices and
//! never reaches a collaborator on a document they review together.

use ui::tilo_resource::{self, Model, ModelError, Refusal, resource};

/// The person's signature: whom they sign as and what they drew.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = SignatureProfile::valid)]
pub struct SignatureProfile {
    /// The name typed signatures carry; empty until the person sets one.
    #[model(atom)]
    pub signer: String,
    /// The drawn signature, polylines in normalised `[0,1]²` pad space;
    /// empty until the person draws. Replaced whole when redrawn.
    pub strokes: Vec<Stroke>,
}

/// One polyline of a drawn signature: its points as `x, y` pairs in one flat
/// list (`x₀, y₀, x₁, y₁, …`), so a stroke of hundreds of points is one list
/// of numbers rather than one object per point.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Stroke {
    pub xy: Vec<f64>,
}

impl Stroke {
    /// The stroke through `points`.
    #[must_use]
    pub fn through(points: &[(f64, f64)]) -> Self {
        Self {
            xy: points.iter().flat_map(|&(x, y)| [x, y]).collect(),
        }
    }

    /// Its points, in order.
    #[must_use]
    pub fn points(&self) -> Vec<(f64, f64)> {
        self.xy
            .chunks_exact(2)
            .map(|pair| (pair[0], pair[1]))
            .collect()
    }
}

/// Bring a signature of version 1 up to this one: its strokes were one JSON
/// string of point lists, and are strokes of flat coordinates now.
fn upgrade(doc: &mut tilo_resource::Doc, from: u32) -> Result<(), ModelError> {
    use ui::automerge::ROOT;
    if from != 1 {
        return Err(ModelError::invalid(format!(
            "no signature migration from {from}"
        )));
    }
    let Some(strokes) =
        tilo_resource::evolution::read_json::<Vec<Vec<(f64, f64)>>>(doc, &ROOT, "strokes")?
    else {
        return Ok(());
    };
    let strokes: Vec<Stroke> = strokes
        .unwrap_or_default()
        .iter()
        .map(|points| Stroke::through(points))
        .collect();
    tilo_resource::Slot::Key(&ROOT, "strokes").remove(doc)?;
    tilo_resource::write_key(doc, &ROOT, "strokes", &strokes)
}

/// The strokes of `profile` as point lists, as the pad draws them.
#[must_use]
pub fn point_lists(profile: &SignatureProfile) -> Vec<Vec<(f64, f64)>> {
    profile.strokes.iter().map(Stroke::points).collect()
}

/// The most points one drawn signature keeps.
const MAX_POINTS: usize = 20_000;

#[resource(
    kind = "pdf.signature-profile",
    label = "Signature",
    params = signature,
    version = 2,
    migrate = upgrade
)]
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
        self.strokes = strokes
            .iter()
            .map(|points| Stroke::through(points))
            .collect();
    }
}

impl SignatureProfile {
    fn valid(&self) -> Result<(), Refusal> {
        let coordinates: usize = self.strokes.iter().map(|stroke| stroke.xy.len()).sum();
        if coordinates > MAX_POINTS * 2
            || self
                .strokes
                .iter()
                .any(|stroke| !stroke.xy.len().is_multiple_of(2))
            || self
                .strokes
                .iter()
                .flat_map(|stroke| &stroke.xy)
                .any(|value| !value.is_finite())
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
        let state = replica.state().unwrap();
        assert_eq!(state.signer, "Ada Lovelace");
        assert_eq!(point_lists(&state), strokes);
        let too_long = vec![vec![(0.5, 0.5); MAX_POINTS + 1]];
        assert!(
            replica
                .apply(SignatureProfile::SET_STROKES.with(too_long))
                .is_err()
        );
    }

    /// A signature from before strokes were stored values: one JSON string
    /// of point lists. Opened now, it draws the same strokes.
    #[test]
    fn a_signature_kept_as_a_json_string_migrates_to_its_strokes() {
        use ui::tilo_resource::evolution;
        let strokes = vec![vec![(0.1, 0.5), (0.9, 0.4)], vec![(0.2, 0.2)]];
        let mut doc = ui::tilo_resource::Doc::new();
        evolution::fixture(
            &mut doc,
            &ui::automerge::ROOT,
            &ui::serde_json::json!({
                "signer": "Ada Lovelace",
                "strokes": ui::serde_json::to_string(&strokes).unwrap(),
            }),
        )
        .unwrap();
        assert!(
            evolution::read_checked::<SignatureProfile>(&doc).is_err(),
            "unmigrated"
        );
        assert!(evolution::migrate::<SignatureProfile>(&mut doc).unwrap());
        let profile = evolution::read_checked::<SignatureProfile>(&doc).unwrap();
        assert_eq!(profile.signer, "Ada Lovelace");
        assert_eq!(point_lists(&profile), strokes);
    }
}
