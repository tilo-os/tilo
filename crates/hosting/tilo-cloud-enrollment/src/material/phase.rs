//! Decode the saved tagged representation without Serde Content buffering.
use super::Phase;
use serde::{Deserialize, Deserializer};

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Step {
    Initial,
    Apply,
    MembershipChallenge,
    MembershipComplete,
}

// Option<T> would collapse a present null into an absent field. The original
// tagged schema rejects null and fields that belong to a different variant.
#[derive(Default)]
enum Present<T> {
    #[default]
    Absent,
    Value(T),
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Present<T> {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        T::deserialize(decoder).map(Self::Value)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fields {
    step: Step,
    #[serde(default)]
    enrollment: Present<Vec<u8>>,
    #[serde(default)]
    signed_at_ns: Present<u64>,
    #[serde(default)]
    request: Present<Vec<u8>>,
    #[serde(default)]
    proof: Present<Vec<u8>>,
    #[serde(default)]
    proof_at_ns: Present<u64>,
}
impl<'de> Deserialize<'de> for Phase {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        use Present::{Absent, Value};
        let fields = Fields::deserialize(decoder)?;
        match (
            fields.step,
            fields.enrollment,
            fields.signed_at_ns,
            fields.request,
            fields.proof,
            fields.proof_at_ns,
        ) {
            (Step::Initial, Absent, Absent, Absent, Absent, Absent) => Ok(Self::Initial),
            (Step::Apply, Value(enrollment), Value(signed_at_ns), Absent, Absent, Absent) => {
                Ok(Self::Apply {
                    enrollment,
                    signed_at_ns,
                })
            }
            (
                Step::MembershipChallenge,
                Value(enrollment),
                Value(signed_at_ns),
                Value(request),
                Absent,
                Absent,
            ) => Ok(Self::MembershipChallenge {
                enrollment,
                signed_at_ns,
                request,
            }),
            (
                Step::MembershipComplete,
                Value(enrollment),
                Value(signed_at_ns),
                Value(request),
                Value(proof),
                Value(proof_at_ns),
            ) => Ok(Self::MembershipComplete {
                enrollment,
                signed_at_ns,
                request,
                proof,
                proof_at_ns,
            }),
            _ => Err(serde::de::Error::custom("invalid hosted phase fields")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::PhaseView;
    use super::*;

    #[test]
    fn hosted_phase_fixed_fields_preserve_tagged_bytes_and_order_independence() {
        let examples = [
            (
                Phase::Apply {
                    enrollment: vec![0, 1],
                    signed_at_ns: 7,
                },
                PhaseView::Apply {
                    enrollment: &[0, 1],
                    signed_at_ns: 7,
                },
                r#"{"step":"apply","enrollment":[0,1],"signed_at_ns":7}"#,
            ),
            (
                Phase::MembershipChallenge {
                    enrollment: vec![0, 1],
                    signed_at_ns: 7,
                    request: vec![3, 4],
                },
                PhaseView::MembershipChallenge {
                    enrollment: &[0, 1],
                    signed_at_ns: 7,
                    request: &[3, 4],
                },
                r#"{"step":"membership_challenge","enrollment":[0,1],"signed_at_ns":7,"request":[3,4]}"#,
            ),
            (
                Phase::MembershipComplete {
                    enrollment: vec![0, 1],
                    signed_at_ns: 7,
                    request: vec![3, 4],
                    proof: vec![5, 6],
                    proof_at_ns: 8,
                },
                PhaseView::MembershipComplete {
                    enrollment: &[0, 1],
                    signed_at_ns: 7,
                    request: &[3, 4],
                    proof: &[5, 6],
                    proof_at_ns: 8,
                },
                r#"{"step":"membership_complete","enrollment":[0,1],"signed_at_ns":7,"request":[3,4],"proof":[5,6],"proof_at_ns":8}"#,
            ),
        ];
        for (owned, borrowed, literal) in examples {
            assert_eq!(serde_json::to_string(&owned).unwrap(), literal);
            assert_eq!(serde_json::to_string(&borrowed).unwrap(), literal);
            let restored: Phase = serde_json::from_str(literal).unwrap();
            assert_eq!(serde_json::to_string(&restored).unwrap(), literal);
        }
        let restored: Phase =
            serde_json::from_str(r#"{"signed_at_ns":7,"enrollment":[0,1],"step":"apply"}"#)
                .unwrap();
        assert_eq!(
            serde_json::to_string(&restored).unwrap(),
            r#"{"step":"apply","enrollment":[0,1],"signed_at_ns":7}"#
        );
    }

    #[test]
    fn hosted_phase_rejects_null_duplicate_unknown_and_other_variant_fields() {
        for invalid in [
            r#"{"step":"initial","enrollment":null}"#,
            r#"{"step":"initial","enrollment":[]}"#,
            r#"{"step":"apply","enrollment":null,"signed_at_ns":7}"#,
            r#"{"step":"apply","enrollment":[],"signed_at_ns":null}"#,
            r#"{"step":"apply","enrollment":[],"signed_at_ns":7,"request":[]}"#,
            r#"{"step":"initial","step":"initial"}"#,
            r#"{"step":"initial","extension":0}"#,
            r#"{"step":"membership_complete","enrollment":[],"signed_at_ns":7,"request":[],"proof":[]}"#,
        ] {
            assert!(serde_json::from_str::<Phase>(invalid).is_err(), "{invalid}");
        }
    }
}
