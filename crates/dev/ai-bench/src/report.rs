//! A run's report: what each endpoint answered, which checks refused it, and
//! what the run cost — written as data so two runs can be compared by a
//! program rather than by eye.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::case::InteractionKind;
use crate::endpoint::{Cost, Endpoint, Usage};
use crate::score::ScoredCase;

/// One endpoint's run over one suite.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelRun {
    /// The endpoint this run asked.
    pub endpoint: Endpoint,
    /// Every case, in suite order.
    pub cases: Vec<CaseRun>,
    /// What the endpoint reported spending, summed.
    pub usage: Usage,
    /// What those tokens cost, at the endpoint's stated price.
    pub cost: Cost,
}

/// One case, as one endpoint answered it.
#[derive(Clone, Debug, PartialEq)]
pub struct CaseRun {
    /// The case's id in its suite.
    pub case: String,
    /// The answer, or why there was none.
    pub answer: Result<Answered, String>,
    /// Every check's result, when the case was scored.
    pub score: Option<ScoredCase>,
}

/// An answer worth reporting.
#[derive(Clone, Debug, PartialEq)]
pub struct Answered {
    /// What the model answered.
    pub text: String,
    /// The model the endpoint said served this answer.
    pub served_model: Option<String>,
    /// The digest of the request this answered.
    pub request: String,
}

impl CaseRun {
    /// Whether the case passed: an answer arrived and every check took it.
    ///
    /// A case with no answer never passes, however few checks it declared —
    /// the failure a harness that counted empty check lists as green would
    /// hide.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.answer.is_ok() && self.score.as_ref().is_some_and(ScoredCase::passed)
    }

    /// The row this case is in a report's JSON.
    fn json(&self) -> CaseJson {
        let (answered, served_model, request, no_answer) = match &self.answer {
            Ok(answer) => (
                Some(answer.text.clone()),
                answer.served_model.clone(),
                Some(answer.request.clone()),
                None,
            ),
            Err(why) => (None, None, None, Some(why.clone())),
        };
        let refused = self
            .score
            .iter()
            .flat_map(|score| &score.checks)
            .filter_map(|check| {
                check.refused.as_ref().map(|reason| RefusedJson {
                    check: check.check.to_string(),
                    reason: reason.clone(),
                })
            })
            .collect();
        CaseJson {
            id: self.case.clone(),
            kind: self
                .score
                .as_ref()
                .map(|score| score.kind.to_string())
                .unwrap_or_default(),
            passed: self.passed(),
            answered,
            served_model,
            request,
            no_answer,
            refused,
        }
    }
}

/// A case's row read back out of a report's JSON.
#[derive(Serialize, Deserialize)]
struct CaseJson {
    id: String,
    kind: String,
    passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    answered: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    served_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    no_answer: Option<String>,
    /// Every check that refused the answer, with its reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    refused: Vec<RefusedJson>,
}

impl CaseJson {
    /// The case this row describes, with the verdict it recorded.
    fn run(self) -> CaseRun {
        let passed = self.passed;
        let kind = match self.kind.as_str() {
            "one-shot" => InteractionKind::OneShot,
            "routing" => InteractionKind::Routing,
            "task" => InteractionKind::Task,
            "skill" => InteractionKind::Skill,
            _ => InteractionKind::ShortcutStep,
        };
        let answer = self.answered.map_or_else(
            || Err(self.no_answer.unwrap_or_default()),
            |text| {
                Ok(Answered {
                    text,
                    served_model: self.served_model,
                    request: self.request.unwrap_or_default(),
                })
            },
        );
        // What a read-back report keeps is the verdict and the reasons, which
        // is what comparing two runs needs; it holds no per-check detail
        // beyond that. A refusal outlives the report it was read out of, so
        // its kind does too.
        let checks = if passed {
            Vec::new()
        } else {
            self.refused
                .into_iter()
                .map(|refused| crate::score::CheckResult {
                    check: Box::leak(refused.check.into_boxed_str()),
                    refused: Some(refused.reason),
                })
                .collect()
        };
        CaseRun {
            case: self.id.clone(),
            answer,
            score: Some(ScoredCase {
                case: self.id,
                kind,
                checks,
            }),
        }
    }
}

impl ModelRun {
    /// This run over one interaction kind.
    #[must_use]
    pub fn only(&self, kind: InteractionKind) -> Vec<&CaseRun> {
        self.cases
            .iter()
            .filter(|run| run.score.as_ref().is_some_and(|score| score.kind == kind))
            .collect()
    }

    /// How many cases passed.
    #[must_use]
    pub fn passed(&self) -> usize {
        self.cases.iter().filter(|run| run.passed()).count()
    }

    /// This run as the report's JSON row.
    fn json(&self) -> EndpointJson {
        let mut suites: BTreeMap<String, SuiteJson> = BTreeMap::new();
        for case in &self.cases {
            let Some(score) = &case.score else {
                continue;
            };
            let suite = suites.entry(score.kind.to_string()).or_insert(SuiteJson {
                passed: 0,
                cases: 0,
            });
            suite.cases = suite.cases.saturating_add(1);
            if case.passed() {
                suite.passed = suite.passed.saturating_add(1);
            }
        }
        // The first case whose answer came from a model other than the one
        // asked for: a run that silently served another model says so once,
        // in the header, rather than only inside each case.
        let served_model = self.cases.iter().find_map(|case| match &case.answer {
            Ok(answered) => answered
                .served_model
                .clone()
                .filter(|served| *served != self.endpoint.model),
            Err(_) => None,
        });
        EndpointJson {
            label: self.endpoint.label.clone(),
            url: self.endpoint.url.clone(),
            model: self.endpoint.model.clone(),
            served_model,
            passed: self.passed(),
            cases: self.cases.len(),
            input_tokens: self.usage.input_tokens,
            output_tokens: self.usage.output_tokens,
            cost: self.cost.amount(),
            suites,
            case: self.cases.iter().map(CaseRun::json).collect(),
        }
    }
}

/// Every endpoint's run, as one report.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    /// One entry per endpoint, in the order the run named them.
    pub runs: Vec<ModelRun>,
}

/// The report's JSON: the one shape a comparison reads, and the one a CI job
/// keeps as its artifact.
#[derive(Serialize, Deserialize)]
struct ReportJson {
    endpoints: Vec<EndpointJson>,
}

#[derive(Serialize, Deserialize)]
struct EndpointJson {
    label: String,
    url: String,
    model: String,
    /// What the endpoint said it served, when that is not what was asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    served_model: Option<String>,
    passed: usize,
    cases: usize,
    input_tokens: u64,
    output_tokens: u64,
    cost: String,
    suites: BTreeMap<String, SuiteJson>,
    case: Vec<CaseJson>,
}

#[derive(Serialize, Deserialize)]
struct SuiteJson {
    passed: usize,
    cases: usize,
}

#[derive(Serialize, Deserialize)]
struct RefusedJson {
    check: String,
    reason: String,
}

impl Report {
    /// The report as JSON, pretty-printed with a trailing newline.
    ///
    /// # Errors
    ///
    /// When the report cannot be written.
    pub fn to_json(&self) -> Result<String, String> {
        let mut text = serde_json::to_string_pretty(&self.json())
            .map_err(|error| format!("the report cannot be written: {error}"))?;
        text.push('\n');
        Ok(text)
    }

    /// The report read back, for comparing two runs by a program.
    ///
    /// # Errors
    ///
    /// When `text` is not a report.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let parsed: ReportJson = serde_json::from_str(text)
            .map_err(|error| format!("this is not a benchmark report: {error}"))?;
        Ok(Self {
            runs: parsed
                .endpoints
                .into_iter()
                .map(|endpoint| {
                    let cases = endpoint.case.into_iter().map(CaseJson::run).collect();
                    ModelRun {
                        endpoint: Endpoint {
                            url: endpoint.url,
                            model: endpoint.model,
                            label: endpoint.label,
                            price: crate::endpoint::Price::default(),
                        },
                        cases,
                        usage: Usage {
                            input_tokens: endpoint.input_tokens,
                            output_tokens: endpoint.output_tokens,
                        },
                        cost: Cost::default(),
                    }
                })
                .collect(),
        })
    }

    fn json(&self) -> ReportJson {
        ReportJson {
            endpoints: self.runs.iter().map(ModelRun::json).collect(),
        }
    }

    /// What a person reads: one block per endpoint, a line per failing case,
    /// and the cost beside the score.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for run in &self.runs {
            let model = run.endpoint.model.trim();
            let shown = match run.cases.iter().find_map(|case| match &case.answer {
                Ok(answered) => answered
                    .served_model
                    .as_deref()
                    .filter(|served| !served.is_empty() && *served != model),
                Err(_) => None,
            }) {
                Some(served) => format!("{model}, served as {served}"),
                None => model.to_string(),
            };
            let _ = writeln!(
                text,
                "{}\n  {}/{} cases passed on {shown}",
                run.endpoint.label,
                run.passed(),
                run.cases.len()
            );
            for kind in InteractionKind::ALL {
                let cases = run.only(*kind);
                if cases.is_empty() {
                    continue;
                }
                let passed = cases.iter().filter(|case| case.passed()).count();
                let _ = writeln!(text, "  {kind}: {passed}/{} cases passed", cases.len());
            }
            for case in &run.cases {
                if case.passed() {
                    continue;
                }
                match &case.answer {
                    Err(why) => {
                        let _ = writeln!(text, "  failed {}: {why}", case.case);
                    }
                    Ok(answered) => {
                        let empty = String::from("no answer");
                        let shown = answered.text.trim();
                        let shown = if shown.is_empty() { &empty } else { shown };
                        let answer = if shown.chars().count() <= 120 {
                            shown.to_string()
                        } else {
                            format!("{}...", shown.chars().take(117).collect::<String>())
                        };
                        let _ = writeln!(text, "  failed {}: answered {answer:?}", case.case);
                    }
                }
                for check in case.score.iter().flat_map(|score| &score.checks) {
                    if let Some(reason) = &check.refused {
                        let _ = writeln!(text, "    {} {reason}", check.check);
                    }
                }
            }
            let _ = writeln!(
                text,
                "  cost {} on {} input and {} output tokens",
                run.cost.amount(),
                run.usage.input_tokens,
                run.usage.output_tokens
            );
        }
        text
    }

    /// Whether every case passed on every endpoint.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.runs.iter().all(|run| run.passed() == run.cases.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::CheckResult;

    fn run(answered: &str, passed: bool) -> ModelRun {
        let checks = vec![CheckResult {
            check: "text",
            refused: (!passed).then(|| format!("wanted {answered:?}, no")),
        }];
        ModelRun {
            endpoint: Endpoint {
                url: "http://127.0.0.1:1/v1/chat/completions".into(),
                model: "small".into(),
                label: "local".into(),
                price: crate::endpoint::Price::default(),
            },
            cases: vec![CaseRun {
                case: "greet".into(),
                answer: Ok(Answered {
                    text: answered.into(),
                    served_model: Some("small-2024".into()),
                    request: "0123456789abcdef".into(),
                }),
                score: Some(ScoredCase {
                    case: "greet".into(),
                    kind: InteractionKind::OneShot,
                    checks,
                }),
            }],
            usage: Usage {
                input_tokens: 12,
                output_tokens: 3,
            },
            cost: Cost::default(),
        }
    }

    #[test]
    fn a_failing_case_says_what_it_answered_and_which_check_refused_it() {
        let text = Report {
            runs: vec![run("hey", false)],
        }
        .to_text();
        assert!(text.contains("local\n"), "{text}");
        assert!(
            text.contains("0/1 cases passed on small, served as small-2024"),
            "{text}"
        );
        assert!(text.contains("failed greet: answered \"hey\""), "{text}");
        assert!(text.contains("    text wanted \"hey\", no"), "{text}");
        assert!(text.contains("cost 0.000000 on 12 input"), "{text}");
    }

    #[test]
    fn the_json_names_the_failing_case_and_its_refusal() {
        let report = Report {
            runs: vec![run("hey", false)],
        };
        let json = report.to_json().unwrap();
        assert!(json.contains("\"refused\""), "{json}");
        assert!(json.contains("\"reason\": \"wanted"), "{json}");
        let read = Report::from_json(&json).unwrap();
        assert_eq!(read.runs.len(), 1);
        assert_eq!(read.runs[0].cases[0].case, "greet");
        assert!(!read.runs[0].cases[0].passed());
    }

    #[test]
    fn a_silent_failure_is_a_failure_and_never_a_skip() {
        let mut quiet = run("", false);
        quiet.cases[0].answer = Err("no answer: connection refused".into());
        quiet.cases[0].score = Some(ScoredCase {
            case: "greet".into(),
            kind: InteractionKind::OneShot,
            checks: Vec::new(),
        });
        let report = Report { runs: vec![quiet] };
        assert!(!report.passed(), "a case with no answer never passes");
        assert!(
            report
                .to_text()
                .contains("failed greet: no answer: connection refused")
        );
    }
}
