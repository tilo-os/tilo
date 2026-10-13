//! What the agent was asked to do, and the gate an answered operation id
//! passes before it becomes an instruction.
//!
//! What an instruction may ask for is exactly what the room's apps currently
//! publish, through
//! [`Instruction::Perform`]: surveyed through [`crate::surface`] and checked
//! against it again at dispatch, never against a remembered list. There are
//! no agent-only recipes: a table, a caption or a heading is whatever the
//! applications' own published operations do, the same ones a person reaches
//! from the palette.

use crate::surface::{self, Offer};

/// What the agent was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instruction {
    /// Perform one operation the room exports, named by its address. The
    /// address is the app's own `(resource, action)` pair; the argument is
    /// the text that operation collects, when it collects any. Neither is
    /// interpreted here — resolving it is done against the live surface at
    /// dispatch.
    Perform {
        /// `"<resource id>#<action id>"`.
        id: String,
        /// Canonical JSON argument object, keyed exactly as the live
        /// descriptor declares. Empty for a nullary operation.
        arguments: serde_json::Map<String, serde_json::Value>,
        /// Contract confirmation was explicitly and correctly supplied in
        /// the plan metadata. Never inferred from prose or risk level.
        confirmation: ConfirmationEvidence,
    },
    /// Nothing published answers it; the agent says so.
    Unknown(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ConfirmationEvidence {
    #[default]
    None,
    Confirm,
    Phrase(String),
}

/// Why an answer did not become an instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The id is not one the room exports right now. Nothing is dispatched.
    NotExported(String),
    /// The id is exported, but the content that came with it cannot be
    /// written as it stands.
    Unusable(String),
}

impl Instruction {
    /// The full gate over an answered id: checked against what the room
    /// exports **now**, not against what it exported when the id was
    /// offered.
    ///
    /// [`NOTHING_FITS`] is a legitimate answer, not a failure. Anything else
    /// must name a live [`Offer`]: an operation that
    /// collects a content hash is never asked to quote one — there is
    /// nowhere honest to get one from an answer — and an operation that
    /// collects text is [`Refused::Unusable`] when neither `supplied` nor a
    /// quoted literal in `rest` gives it any, rather than silently falling
    /// back to the deterministic floor while reporting success.
    ///
    /// # Errors
    ///
    /// [`Refused::NotExported`] when `id` names no live [`Offer`]. [`Refused::Unusable`] when `id` names an offer
    /// that collects text but neither `supplied` nor a quoted literal in
    /// `rest` gives it any.
    pub fn from_answer(
        id: &str,
        rest: &str,
        supplied: Option<&serde_json::Map<String, serde_json::Value>>,
        offers: &[Offer],
    ) -> Result<Instruction, Refused> {
        if id == NOTHING_FITS {
            return Ok(Instruction::Unknown(rest.to_string()));
        }
        let Some(offer) = surface::find(offers, id) else {
            return Err(Refused::NotExported(id.to_string()));
        };
        let mut arguments = if offer.wants_content() {
            supplied.cloned().unwrap_or_default()
        } else if offer.wants_arguments() {
            supplied
                .cloned()
                .or_else(|| offer.arguments_from_literal(rest))
                .ok_or_else(|| {
                    Refused::Unusable(format!(
                        "{} requires structured arguments and neither the answer nor the \
                     instruction supplied a valid object",
                        offer.descriptor.action_label
                    ))
                })?
        } else {
            supplied.cloned().unwrap_or_default()
        };
        let confirmation = match &offer.descriptor.contract.confirmation {
            tilo_actions::ConfirmationPolicy::None => ConfirmationEvidence::None,
            tilo_actions::ConfirmationPolicy::Confirm => {
                if arguments.remove("confirm") != Some(serde_json::Value::Bool(true)) {
                    return Err(Refused::Unusable(format!(
                        "{} requires `confirm: true`",
                        offer.descriptor.action_label
                    )));
                }
                ConfirmationEvidence::Confirm
            }
            tilo_actions::ConfirmationPolicy::Phrase { phrase } => {
                if arguments
                    .remove("confirmation_phrase")
                    .and_then(|v| v.as_str().map(str::to_owned))
                    != Some(phrase.clone())
                {
                    return Err(Refused::Unusable(format!(
                        "{} requires the exact confirmation phrase",
                        offer.descriptor.action_label
                    )));
                }
                ConfirmationEvidence::Phrase(phrase.clone())
            }
        };
        // Confirmation is invocation metadata, never an app argument. Strip
        // both reserved names even for actions that need no confirmation so
        // they cannot collide with or sneak through strict schema binding.
        arguments.remove("confirm");
        arguments.remove("confirmation_phrase");
        if !offer.wants_content() {
            tilo_actions::ActionArgs::bind_json(&offer.descriptor.params, &arguments)
                .map_err(|error| Refused::Unusable(error.to_string()))?;
        }
        Ok(Instruction::Perform {
            id: offer.id(),
            arguments,
            confirmation,
        })
    }
}

/// The answer that means "no exported action does what was asked".
pub const NOTHING_FITS: &str = "none";

#[cfg(test)]
mod tests {
    use super::*;

    // -- Instruction::Perform, and the floor that reaches it -------------

    /// An offer built from a real descriptor, so what these tests check is
    /// what a gate checks against a real survey.
    fn offer(
        app: &'static str,
        resource: &str,
        action: &str,
        label: &str,
        collects: Option<tilo_actions::Collects>,
    ) -> Offer {
        let params = if collects.is_some() {
            vec![tilo_actions::ActionParam::required(
                "value",
                tilo_actions::ParamType::Text,
                "value",
            )]
        } else {
            Vec::new()
        };
        Offer {
            app: app.to_string(),
            broker_peer_id: None,
            broker_provider: None,
            descriptor: tilo_actions::door::ActionDescriptor {
                semantic_identity: None,
                address: tilo_actions::door::ActionAddress::new(resource, action),
                resource_kind: format!("{app}.thing"),
                resource_label: resource.rsplit(':').next().unwrap_or(resource).to_string(),
                action_label: label.to_string(),
                description: None,
                locus: tilo_actions::ExecutionLocus::ClientLocal,
                kind: tilo_actions::OperationKind::Document,
                contract: tilo_actions::ActionContract::default(),
                params,
                input: match collects {
                    None => tilo_actions::InputSpec::None,
                    Some(c) => tilo_actions::InputSpec::Text {
                        prompt: "p".into(),
                        placeholder: String::new(),
                        collects: c,
                    },
                },
                view: None,
            },
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each case keeps its original fixture and assertions"
    )]
    fn an_instruction_resolves_only_to_a_surveyed_bound_and_confirmed_operation() {
        // Case `an_operation_that_collects_content_is_not_asked_for_words`.
        {
            let offers = vec![offer(
                "word-processor",
                "word-processor.section:b7",
                "place-image",
                "Place image below",
                Some(tilo_actions::Collects::Content),
            )];
            let answered = Instruction::from_answer(
                "word-processor.section:b7#place-image",
                "place image below b7",
                None,
                &offers,
            );
            assert_eq!(
                answered,
                Ok(Instruction::Perform {
                    id: "word-processor.section:b7#place-image".into(),
                    arguments: serde_json::Map::new(),
                    confirmation: ConfirmationEvidence::None,
                })
            );
        }

        // Case `text_collecting_operations_need_a_literal_or_are_refused`.
        {
            let offers = vec![offer(
                "files",
                "files.entry:/a",
                "rename",
                "Rename",
                Some(tilo_actions::Collects::Words),
            )];
            assert!(
                matches!(
                    Instruction::from_answer("files.entry:/a#rename", "rename it", None, &offers),
                    Err(Refused::Unusable(_))
                ),
                "no literal anywhere must be refused, not performed with a blank"
            );
            assert_eq!(
                Instruction::from_answer(
                    "files.entry:/a#rename",
                    r#"rename it to "b.md""#,
                    None,
                    &offers
                ),
                Ok(Instruction::Perform {
                    id: "files.entry:/a#rename".into(),
                    arguments: serde_json::Map::from_iter([(
                        "value".into(),
                        serde_json::Value::String("b.md".into())
                    )]),
                    confirmation: ConfirmationEvidence::None,
                }),
                "a literal quoted in the instruction is used"
            );
            assert_eq!(
                Instruction::from_answer(
                    "files.entry:/a#rename",
                    "rename it",
                    Some(&serde_json::Map::from_iter([(
                        "value".into(),
                        serde_json::Value::String("c.md".into())
                    )])),
                    &offers,
                ),
                Ok(Instruction::Perform {
                    id: "files.entry:/a#rename".into(),
                    arguments: serde_json::Map::from_iter([(
                        "value".into(),
                        serde_json::Value::String("c.md".into())
                    )]),
                    confirmation: ConfirmationEvidence::None,
                }),
                "a literal the answer itself supplied is used even with no quote in the instruction"
            );
        }

        // Case `structured_answers_preserve_types_and_are_bound_before_dispatch`.
        {
            let mut rich = offer("sheets", "sheets.range:r1", "fill", "Fill range", None);
            rich.descriptor.params = vec![
                tilo_actions::ActionParam::required(
                    "count",
                    tilo_actions::ParamType::Integer,
                    "Rows to fill",
                ),
                tilo_actions::ActionParam::required(
                    "enabled",
                    tilo_actions::ParamType::Boolean,
                    "Whether formulas are enabled",
                ),
            ];
            let offers = vec![rich];
            let valid = serde_json::Map::from_iter([
                ("count".into(), serde_json::json!(3)),
                ("enabled".into(), serde_json::json!(true)),
            ]);
            let instruction =
                Instruction::from_answer("sheets.range:r1#fill", "fill it", Some(&valid), &offers)
                    .expect("the descriptor accepts its typed object");
            assert_eq!(
                instruction,
                Instruction::Perform {
                    id: "sheets.range:r1#fill".into(),
                    arguments: valid,
                    confirmation: ConfirmationEvidence::None,
                }
            );

            let wrong = serde_json::Map::from_iter([
                ("count".into(), serde_json::json!("3")),
                ("enabled".into(), serde_json::json!(true)),
            ]);
            assert!(matches!(
                Instruction::from_answer("sheets.range:r1#fill", "fill it", Some(&wrong), &offers,),
                Err(Refused::Unusable(_))
            ));
        }

        // Case `consequential_actions_require_and_strip_explicit_confirmation_metadata`.
        {
            let mut destructive = offer(
                "files",
                "files.entry:/important",
                "delete",
                "Delete file",
                None,
            );
            destructive.descriptor.contract.confirmation =
                tilo_actions::ConfirmationPolicy::Confirm;
            destructive.descriptor.params = vec![tilo_actions::ActionParam::required(
                "recursive",
                tilo_actions::ParamType::Boolean,
                "Delete descendants",
            )];
            let offers = vec![destructive];

            for arguments in [
                serde_json::json!({"recursive": true}),
                serde_json::json!({"recursive": true, "confirm": false}),
                serde_json::json!({"recursive": true, "confirm": "true"}),
            ] {
                assert!(matches!(
                    Instruction::from_answer(
                        "files.entry:/important#delete",
                        "delete it",
                        arguments.as_object(),
                        &offers,
                    ),
                    Err(Refused::Unusable(_))
                ));
            }

            let confirmed = serde_json::json!({"recursive": true, "confirm": true});
            assert_eq!(
                Instruction::from_answer(
                    "files.entry:/important#delete",
                    "delete it",
                    confirmed.as_object(),
                    &offers,
                ),
                Ok(Instruction::Perform {
                    id: "files.entry:/important#delete".into(),
                    arguments: serde_json::Map::from_iter([(
                        "recursive".into(),
                        serde_json::Value::Bool(true),
                    )]),
                    confirmation: ConfirmationEvidence::Confirm,
                })
            );
        }

        // Case `phrase_confirmation_must_match_exactly_before_an_instruction_can_dispatch`.
        {
            let phrase = "delete all archived messages";
            let mut destructive =
                offer("chat", "chat.archive:2025", "purge", "Purge archive", None);
            destructive.descriptor.contract.confirmation =
                tilo_actions::ConfirmationPolicy::Phrase {
                    phrase: phrase.into(),
                };
            let offers = vec![destructive];

            for supplied in [
                serde_json::json!({}),
                serde_json::json!({"confirmation_phrase": "Delete all archived messages"}),
                serde_json::json!({"confirmation_phrase": "delete all archived messages "}),
            ] {
                assert!(matches!(
                    Instruction::from_answer(
                        "chat.archive:2025#purge",
                        "purge it",
                        supplied.as_object(),
                        &offers,
                    ),
                    Err(Refused::Unusable(_))
                ));
            }

            let exact = serde_json::json!({"confirmation_phrase": phrase});
            assert_eq!(
                Instruction::from_answer(
                    "chat.archive:2025#purge",
                    "purge it",
                    exact.as_object(),
                    &offers,
                ),
                Ok(Instruction::Perform {
                    id: "chat.archive:2025#purge".into(),
                    arguments: serde_json::Map::new(),
                    confirmation: ConfirmationEvidence::Phrase(phrase.into()),
                })
            );
        }
    }

    // -- Sequences: Plan, Step, clauses -----------------------------------
}
