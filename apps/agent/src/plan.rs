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

/// The one content hash `previous` produced: the argument a step that
/// carries is given.
///
/// # Errors
///
/// Why `previous` cannot feed a content input: it produced nothing, more
/// than one resource, or a resource that is not `tilo.content`.
pub fn carried_content(previous: &tilo_actions::ActionOutput) -> Result<&str, String> {
    match previous.resources.as_slice() {
        [] => Err("the previous action produced no content resource".into()),
        [resource] => tilo_actions::content_hash(resource).ok_or_else(|| {
            "the previous action produced a resource that is not tilo.content".into()
        }),
        _ => Err("the previous action must produce exactly one content resource".into()),
    }
}

/// The arguments a step is invoked with: `planned` as written, or — when
/// the operation carries content under the parameter `carries`
/// ([`crate::surface::Offer::content_param`]) — with that input bound to
/// what the immediately preceding action produced. With nothing before it,
/// a content address the plan names outright (an image a step of an
/// earlier task exported) is taken as it is.
///
/// # Errors
///
/// Why a content-consuming step cannot run: there is no preceding action
/// result, or it does not name exactly one piece of content.
pub fn carried_arguments(
    planned: &serde_json::Map<String, serde_json::Value>,
    carries: Option<&str>,
    previous: Option<&tilo_actions::ActionOutput>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let Some(param) = carries else {
        return Ok(planned.clone());
    };
    let hash = match previous.map(carried_content) {
        Some(Ok(hash)) => hash.to_owned(),
        unfed => named_content(planned, param).ok_or_else(|| match unfed {
            Some(Err(error)) => error,
            _ => "a content-consuming action has no preceding action result".to_string(),
        })?,
    };
    let mut arguments = planned.clone();
    // A model-authored value is replaced: the descriptor says the value is
    // carried, so only the preceding application may supply it.
    arguments.insert(param.to_owned(), serde_json::Value::String(hash));
    Ok(arguments)
}

/// The content hash `planned` names outright under `param`: a
/// `tilo.content:` address of a well-formed hash, not a placeholder.
#[must_use]
pub fn named_content(
    planned: &serde_json::Map<String, serde_json::Value>,
    param: &str,
) -> Option<String> {
    planned
        .get(param)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !is_content_placeholder(text))
        .and_then(tilo_actions::content_hash)
        .map(str::to_owned)
}

/// What a plan writes for content a step before it in the same task
/// produces (a picture an export makes), whose address no plan can know
/// before that step has run.
pub const LATEST_CONTENT: &str = "tilo.content:latest";

/// The length of a content hash: BLAKE3 in lower-case hex, as the content
/// store derives every address.
const CONTENT_HASH_CHARS: usize = 64;

/// Bind every content placeholder among `arguments` to `latest`, the
/// address of the content the task's latest content-producing step made:
/// [`LATEST_CONTENT`], or any `tilo.content:` address whose hash is not one
/// (`tilo.content:<export>`, a name the model made up). An address naming
/// real content is kept. Returns how many it bound.
pub fn bind_latest_content(
    arguments: &mut serde_json::Map<String, serde_json::Value>,
    latest: &str,
) -> usize {
    arguments
        .values_mut()
        .map(|value| bind_placeholders(value, latest))
        .sum()
}

/// [`bind_latest_content`] within one value, however deep.
fn bind_placeholders(value: &mut serde_json::Value, latest: &str) -> usize {
    match value {
        serde_json::Value::String(text) if is_content_placeholder(text) => {
            latest.clone_into(text);
            1
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .map(|item| bind_placeholders(item, latest))
            .sum(),
        serde_json::Value::Object(fields) => fields
            .values_mut()
            .map(|field| bind_placeholders(field, latest))
            .sum(),
        _ => 0,
    }
}

/// Whether `text` stands for content rather than naming it: a
/// `tilo.content:` address without a well-formed hash.
fn is_content_placeholder(text: &str) -> bool {
    text.trim()
        .strip_prefix("tilo.content:")
        .is_some_and(|hash| {
            !(hash.len() == CONTENT_HASH_CHARS
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plan names content an earlier step makes by placeholder; the
    /// placeholder, however the model wrote it, becomes that content, and
    /// an address that names content outright is kept.
    #[test]
    fn content_placeholders_bind_to_the_latest_content() {
        let latest = format!("tilo.content:{}", "a".repeat(64));
        let kept = format!("tilo.content:{}", "b".repeat(64));
        let mut arguments = serde_json::json!({
            "src": LATEST_CONTENT,
            "images": ["tilo.content:<export-result>", kept],
            "style": {"background": " tilo.content:hero "},
            "upper": format!("tilo.content:{}", "C".repeat(64)),
            "title": "Meet Lumo",
            "count": 3,
        })
        .as_object()
        .cloned()
        .unwrap();
        assert_eq!(bind_latest_content(&mut arguments, &latest), 4);
        assert_eq!(arguments["src"], latest.as_str());
        assert_eq!(arguments["images"][0], latest.as_str());
        assert_eq!(arguments["images"][1], kept.as_str());
        assert_eq!(arguments["style"]["background"], latest.as_str());
        assert_eq!(
            arguments["upper"],
            latest.as_str(),
            "upper-case hex is no address the store derives"
        );
        assert_eq!(arguments["title"], "Meet Lumo");
        assert_eq!(arguments["count"], 3);
    }

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
}
