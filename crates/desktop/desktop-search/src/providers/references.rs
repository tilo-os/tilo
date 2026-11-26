//! The records of other workspaces this one holds a reference to, found by name.
//!
//! Bringing a record here ([`bring`]) puts a reference to it in this workspace's store. The
//! reference grants nothing: a row the person may read is found by the record's name and
//! opens it where it lives, as a link to it there does; a row they may not read says so, names
//! nothing of the record, and opens nothing.

use ui::search::system::{MAX_PROVIDER_SCORE, ResourceKind};

use crate::model::{DESKTOP_APP, Hit, InvokeTarget, Source, Target};

/// The session's references: [`BRING`] puts one in a workspace and [`REMOVE`] takes one out,
/// and choosing one the person may not read asks [`UNAVAILABLE`] of it, which it refuses saying
/// why.
pub const REFERENCES_RESOURCE: &str = "desktop:references";
pub const BRING: &str = "bring";
pub const REMOVE: &str = "remove";
pub const UNAVAILABLE: &str = "unavailable";
/// What [`BRING`] is told of the record it brings: where it lives, its app and its id.
pub const WORKSPACE_PARAM: &str = "workspace";
pub const APP_PARAM: &str = "app";
pub const RECORD_PARAM: &str = "record";
/// What [`REMOVE`] is told: the reference's id.
pub const REFERENCE_PARAM: &str = "reference";

/// The modifier that chooses a row's second action: bringing a record here, or taking a
/// reference out again.
pub const BRING_MODIFIER: ui::search::ActivationModifier = ui::search::ActivationModifier::Alt;

/// What brings the record `record` that the app `app` keeps in the workspace `from` into the
/// workspace `here`, without moving it.
#[must_use]
pub fn bring(here: &str, from: &str, app: &str, record: &str) -> InvokeTarget {
    InvokeTarget::action(DESKTOP_APP, here, REFERENCES_RESOURCE, BRING)
        .with_argument(WORKSPACE_PARAM, from)
        .with_argument(APP_PARAM, app)
        .with_argument(RECORD_PARAM, record)
}

/// Rows the empty palette lists.
const LISTED: usize = 6;

/// A reference this workspace holds, as the person reading it may see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    /// The reference's id in this workspace.
    pub id: String,
    /// The record it names, where the person may read it there; `None` where they may not.
    pub opens: Option<Opens>,
}

/// A record a reference names, for a person who may read it where it lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opens {
    pub workspace: String,
    pub app: String,
    pub record: String,
    /// What it is called: its name, else its kind.
    pub title: String,
    /// The name of the workspace it lives in, where this device shows it.
    pub from: Option<String>,
}

/// The references of the workspace `workspace`.
pub struct References {
    workspace: String,
    held: Vec<Reference>,
}

impl References {
    #[must_use]
    pub fn new(workspace: impl Into<String>, held: Vec<Reference>) -> Self {
        Self {
            workspace: workspace.into(),
            held,
        }
    }

    fn hit(&self, reference: &Reference, score: i32) -> Hit {
        let t = crate::i18n::t;
        let (title, facts, target) = match &reference.opens {
            Some(opens) => (
                opens.title.clone(),
                vec![opens.from.as_ref().map_or_else(
                    || t("search-reference-from-another-workspace"),
                    |workspace| {
                        crate::i18n::t_args(
                            "search-reference-from",
                            &[("workspace", workspace.as_str().into())],
                        )
                    },
                )],
                InvokeTarget::place(
                    &opens.workspace,
                    &tilo_service::notice::Place {
                        app: opens.app.clone(),
                        record: opens.record.clone(),
                        ..Default::default()
                    },
                ),
            ),
            None => (
                t("search-reference-unavailable"),
                vec![t("search-reference-unavailable-why")],
                InvokeTarget::action(
                    DESKTOP_APP,
                    &self.workspace,
                    REFERENCES_RESOURCE,
                    UNAVAILABLE,
                ),
            ),
        };
        let target = target.with_alternate(
            BRING_MODIFIER,
            t("search-reference-remove"),
            InvokeTarget::action(DESKTOP_APP, &self.workspace, REFERENCES_RESOURCE, REMOVE)
                .with_argument(REFERENCE_PARAM, reference.id.as_str()),
        );
        let readable = reference.opens.is_some();
        Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: (!readable).then(|| t("search-reference-unavailable-outcome")),
            kind: ResourceKind::Record,
            provider: self.id().into(),
            id: reference.id.clone(),
            title,
            subtitle: Some(facts.join(", ")),
            facts,
            score,
            icon: (!readable).then_some(tilo_ui_base::icons::Icon::Lock),
            target: Target::Invoke(Box::new(target)),
        }
    }
}

impl Source for References {
    fn id(&self) -> &'static str {
        "references"
    }

    fn browse_rank(&self) -> u8 {
        crate::model::BROWSE_RANK_REFERENCES
    }

    fn name(&self) -> String {
        crate::i18n::t("search-source-references")
    }

    /// Nothing typed lists the references; typed, a row is found by the record's name, which
    /// only one the person may read has.
    fn query(&self, needle: &str) -> Vec<Hit> {
        if needle.trim().is_empty() {
            return self
                .held
                .iter()
                .take(LISTED)
                .zip(0..)
                .map(|(reference, at)| self.hit(reference, MAX_PROVIDER_SCORE.saturating_sub(at)))
                .collect();
        }
        let mut hits: Vec<Hit> = self
            .held
            .iter()
            .filter_map(|reference| {
                let opens = reference.opens.as_ref()?;
                let score = crate::rank::record_match_score(needle, &opens.title, &opens.record)?;
                Some(self.hit(reference, score))
            })
            .collect();
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.title.cmp(&b.title)));
        hits
    }
}
