//! Quality cases for Tilo's AI interactions, and the run that asks them.
//!
//! An AI feature — one-shot text actions, routing, tasks, skills, a
//! Shortcut's step — is a promise that *this* instruction on *that* input
//! produces *that* answer. Changing a prompt, a skill or a default model
//! changes the promise, and nothing about "it looked fine when I tried it"
//! catches the cases that broke. This crate holds them: a suite per
//! interaction kind, each case declaring what a right answer is in terms a
//! program can check, and a runner that asks every case of every endpoint a
//! run names.
//!
//! ## What scores, and what judges
//!
//! A case is scored by its **typed checks**: the shape of its answer, the
//! value it must equal, the text it must or must not contain, the reasons it
//! must give (`crates/dev/ai-bench/src/case.rs`). Nothing scores by reading
//! the answer, because a reading is neither reproducible nor cheap enough to
//! run on every commit. A model judge is allowed to add a *named check* — one
//! more `CheckResult` in the same report, with its own name and reason — and
//! never to replace a typed one, so a run that a judge observed says so.
//!
//! ## Captured cases
//!
//! A case may record where it came from: a run a person marked good or bad,
//! keeping the wrong answer when they said bad. That is how a suite grows
//! from what actually happens rather than from what a prompt author imagined,
//! and it is why a suite's cases carry a `why`.
//!
//! ## No silent fallbacks
//!
//! An endpoint that cannot be reached, refuses the request or answers in the
//! wrong shape fails its cases; the run records why and asks nothing else.
//! The report names the endpoint that answered, the model it said it served
//! and the digest of each request, so a comparison between two models is a
//! comparison of what happened, not of what was configured. Tokens and cost
//! are printed beside the score.
//!
//! ## Running one
//!
//! ```text
//! tilo-ai-bench --suite crates/dev/ai-bench/cases/one-shot --endpoints endpoints.json \
//!     --endpoint local --key-env LOCAL_MODEL_KEY --out report.json
//! ```
//!
//! `--endpoints` names every endpoint a `--endpoint` may select; the key is
//! read from the variable `--key-env` names, in the process that runs the
//! benchmark, so the endpoints file holds no secret and the harness never
//! falls back to a credential it found lying around.

#![deny(missing_docs)]

pub mod case;
pub mod endpoint;
pub mod report;
pub mod run;
pub mod schema;
pub mod score;

pub use case::{Case, CaseSource, Check, InteractionKind, Suite, Verdict};
pub use endpoint::{Endpoint, Endpoints, Price};
pub use report::{CaseRun, ModelRun, Report};
pub use score::{ScoredCase, score};
