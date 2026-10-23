//! The scripted planning model, and the words a task names apps and
//! records by.

use tilo_agent::surface::{Catalogue, Offer};
use tilo_agent::tools::Answer;

use super::{PlanModel, requested};

/// Words that say nothing about which operation is meant.
const STOPWORDS: &[&str] = &[
    "the", "and", "then", "with", "into", "for", "that", "this", "please", "called", "named",
    "titled", "title", "name", "tilo",
];

/// Lowercase words of three letters or more, without a plural `s`.
pub(super) fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.len() > 2 && !STOPWORDS.contains(word))
        .map(|word| {
            word.strip_suffix('s')
                .filter(|stem| stem.len() > 2)
                .unwrap_or(word)
                .to_owned()
        })
        .collect()
}

/// The quoted phrases of `text`, in order.
pub(super) fn quoted(text: &str) -> Vec<String> {
    text.split(['"', '\u{201c}', '\u{201d}'])
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

/// The clauses of a task: `;`, new lines and "then" separate them.
pub(super) fn clauses(task: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in task.split([';', '\n']) {
        let mut rest = part.trim();
        loop {
            let lower = rest.to_lowercase();
            let Some(at) = lower.find(" then ") else {
                break;
            };
            let (head, tail) = rest.split_at(at);
            if !head.trim().is_empty() {
                out.push(head.trim().to_owned());
            }
            rest = tail.get(" then ".len()..).unwrap_or_default().trim();
        }
        let rest = rest
            .strip_prefix("then ")
            .or_else(|| rest.strip_prefix("and then "))
            .unwrap_or(rest)
            .trim();
        if !rest.is_empty() {
            out.push(rest.to_owned());
        }
    }
    out
}

/// An app's short name: `so.tilo.notes` → `notes`.
fn app_name(app: &str) -> Vec<String> {
    words(app.rsplit('.').next().unwrap_or(app))
}

/// The everyday words for what an app's records are, as [`words`] stems
/// them: a task says "the spreadsheet" or "the document", rarely the app.
fn record_words(app: &str) -> &'static [&'static str] {
    match app.rsplit('.').next().unwrap_or(app) {
        "sheets" => &["sheet", "spreadsheet", "workbook", "xlsx"],
        "word-processor" => &["document", "doc", "docx", "letter", "report", "memo"],
        "notes" => &["note", "notebook"],
        // Not "image" or "picture": a document's image is not an artwork.
        "canvas" => &["canva", "artwork", "poster"],
        // "slide" is the app's own name already, stemmed.
        "slides" => &["deck", "slide", "presentation", "pptx"],
        // One word each: "calendar entry" is named by "calendar", the
        // app's own name; "entry" alone names too much else.
        "calendar" => &["event"],
        _ => &[],
    }
}

/// Whether a clause's words name `app` itself: every word of its name.
fn names_app_itself(clause: &[String], app: &str) -> bool {
    let name = app_name(app);
    !name.is_empty() && name.iter().all(|word| clause.contains(word))
}

/// Whether a clause's words name `app`: its name, or a word for its
/// records.
pub(super) fn names_app(clause: &[String], app: &str) -> bool {
    names_app_itself(clause, app)
        || record_words(app)
            .iter()
            .any(|word| clause.iter().any(|said| said == word))
}

/// Whether `task` names `app` (Word Processor, Sheets…) by its short name.
pub(crate) fn task_names_app(task: &str, app: &str) -> bool {
    names_app(&words(task), app)
}

/// Whether `task` names `app` itself (Sheets, Word Processor), not only its
/// records: what a new record is made ahead of the plan for. "From the
/// campaign spreadsheet" reads a workbook; it does not ask for one.
pub(crate) fn task_names_app_itself(task: &str, app: &str) -> bool {
    names_app_itself(&words(task), app)
}

/// A deterministic [`PlanModel`] for tests and local development: no
/// network, the same plan for the same task and surface every time.
///
/// It reads the task clause by clause (`;`, new lines and "then" separate
/// them). For the first clause it picks the offer sharing the most words
/// with it, weighing the app the clause names and the action's own verb,
/// and fills text parameters (required ones first, then optional ones) from
/// the clause's quoted phrases and number
/// parameters from its numbers. When that app is not open yet it opens it
/// and asks again with the same clause; otherwise the remaining clauses are
/// the plan's `then`. It reveals what it names before planning, bound by
/// the same rule a model is.
#[derive(Default)]
pub struct ScriptedModel;

impl ScriptedModel {
    fn score(clause: &[String], has_quote: bool, offer: &Offer) -> usize {
        let descriptor = &offer.descriptor;
        // An open's label says how it opens ("a new, empty document"),
        // words a task uses for what it wants done: it is matched by what it
        // opens alone, and the room's own document comes before a new one.
        let opens = tilo_agent::tools::opened_app(offer).is_some();
        let own: std::collections::BTreeSet<String> = words(&if opens {
            descriptor.resource_label.clone()
        } else {
            format!(
                "{} {} {} {}",
                descriptor.resource_label,
                descriptor.address.resource,
                descriptor.action_label,
                descriptor.address.action
            )
        })
        .into_iter()
        .collect();
        let shared = clause.iter().filter(|word| own.contains(*word)).count();
        let verb = words(&descriptor.address.action)
            .iter()
            .any(|word| clause.contains(word));
        let names_app = names_app(clause, &offer.app);
        let wants_text = descriptor
            .params
            .iter()
            .any(|param| param.ty == tilo_actions::ParamType::Text);
        let mut score = shared;
        if verb {
            score = score.saturating_add(2);
        }
        if names_app {
            score = score.saturating_add(5);
        }
        if tilo_agent::surface::opens_record(&descriptor.address).is_some() {
            score = score.saturating_add(1);
        }
        if has_quote && wants_text {
            score = score.saturating_add(1);
        }
        if descriptor.contract.confirmation != tilo_actions::ConfirmationPolicy::None
            || descriptor.contract.availability != tilo_actions::Availability::Available
        {
            score = 0;
        }
        if shared == 0 && !names_app {
            score = 0;
        }
        score
    }
}

impl PlanModel for ScriptedModel {
    #[allow(
        clippy::too_many_lines,
        reason = "the scripted planner's keyword table reads as one match"
    )]
    fn choose(
        &mut self,
        task: &str,
        catalogue: &Catalogue,
        most_steps: usize,
    ) -> Result<Answer, String> {
        let mut clauses = clauses(requested(task));
        if clauses.is_empty() {
            return Err("the task says nothing to do".into());
        }
        let clause = clauses.remove(0);
        let clause_words = words(&clause);
        let phrases = quoted(&clause);
        let offer = catalogue
            .offers
            .iter()
            .map(|offer| {
                (
                    Self::score(&clause_words, !phrases.is_empty(), offer),
                    offer,
                )
            })
            .filter(|(score, _)| *score > 0)
            // The first of the best, in catalogue order.
            .fold(None::<(usize, &Offer)>, |best, (score, offer)| match best {
                Some((top, _)) if top >= score => best,
                _ => Some((score, offer)),
            })
            .map(|(_, offer)| offer)
            .ok_or_else(|| format!("nothing offered fits {clause:?}"))?;
        let mut revealed = tilo_agent::tools::Revealed::new();
        tilo_agent::tools::answer(
            tilo_agent::tools::LIST_OPERATIONS,
            &serde_json::json!({ "resource": offer.address().resource }),
            catalogue,
            &mut revealed,
        );
        let opens = offer.address().action == tilo_agent::surface::OPEN_ACTION
            && offer.descriptor.resource_kind == tilo_agent::surface::APPLICATION_KIND;
        let mut arguments = serde_json::Map::new();
        if !opens {
            let mut texts = phrases.iter();
            let numbers: Vec<f64> = clause
                .split(|c: char| !(c.is_ascii_digit() || c == '.'))
                .filter_map(|part| part.parse().ok())
                .collect();
            for param in offer
                .descriptor
                .params
                .iter()
                .filter(|param| param.required)
            {
                let value = if let Some(choice) = param
                    .choices
                    .iter()
                    .find(|choice| clause_words.contains(&choice.to_lowercase()))
                    .or_else(|| param.choices.first())
                {
                    serde_json::Value::from(choice.as_ref())
                } else {
                    match param.ty {
                        tilo_actions::ParamType::Integer => {
                            #[allow(
                                clippy::cast_possible_truncation,
                                reason = "a number a person wrote in a task"
                            )]
                            let number = numbers.first().copied().unwrap_or(1.0) as i64;
                            number.into()
                        }
                        tilo_actions::ParamType::Number => {
                            numbers.first().copied().unwrap_or(1.0).into()
                        }
                        tilo_actions::ParamType::Boolean => true.into(),
                        _ => texts
                            .next()
                            .cloned()
                            .ok_or_else(|| {
                                format!(
                                    "{} needs {}: put it in quotes",
                                    offer.descriptor.action_label, param.name
                                )
                            })?
                            .into(),
                    }
                };
                arguments.insert(param.name.to_string(), value);
            }
            // A quoted phrase left over names an optional text parameter
            // (the new sheet's name), in the order they are declared.
            for param in offer.descriptor.params.iter().filter(|param| {
                !param.required
                    && param.ty == tilo_actions::ParamType::Text
                    && param.choices.is_empty()
            }) {
                let Some(text) = texts.next() else { break };
                arguments.insert(param.name.to_string(), text.clone().into());
            }
            if offer.descriptor.params.is_empty()
                && let tilo_actions::InputSpec::Text { .. } = offer.descriptor.input
                && let Some(text) = texts.next()
            {
                arguments.insert(tilo_actions::PROMPTED_PARAM.into(), text.clone().into());
            }
        }
        // Opening an app puts its own operations on the list: the same
        // clause is asked again against them.
        let then = if opens {
            Some(
                std::iter::once(clause)
                    .chain(clauses)
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        } else {
            (!clauses.is_empty()).then(|| clauses.join("; "))
        };
        tilo_agent::tools::read_plan(
            &serde_json::json!({
                "steps": [{"operation": offer.id(), "arguments": arguments}],
                "why": "scripted",
                "then": then,
            }),
            &revealed,
            most_steps,
        )
    }
}
