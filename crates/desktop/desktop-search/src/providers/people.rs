//! The people in this workspace, found by name.
//!
//! Typing a member's name offers them. Choosing one opens this person's
//! conversation with them: Chat's own `message_person`, which keeps one
//! conversation with each person and starts Chat when it is closed. The people are the session's one list of who can be
//! found, the list a box that sends a message reads too. Who is working where
//! with nothing typed is [`super::happening`]'s.

use tilo_ui_base::text_match::FieldWeight;
use ui::floating_surface::{Recipient, RecipientKind};
use ui::search::system::{ResourceKind, rank_fields};
use ui::search::{CommandRow, RowFace, SearchRow};

use crate::model::{Hit, InvokeTarget, Source, Target};

/// The kind people are grouped under, in the desktop's language.
#[must_use]
pub fn people_kind() -> ResourceKind {
    ResourceKind::Custom(crate::i18n::t("search-kind-people"))
}

pub struct People {
    /// The workspace they are in.
    workspace: String,
    people: Vec<Recipient>,
}

impl People {
    /// The people among `found`, whoever else it lists.
    #[must_use]
    pub fn new(workspace: impl Into<String>, mut found: Vec<Recipient>) -> Self {
        found.retain(|recipient| recipient.kind == RecipientKind::Person);
        Self {
            workspace: workspace.into(),
            people: found,
        }
    }
}

impl Source for People {
    fn id(&self) -> &'static str {
        "people"
    }

    fn name(&self) -> String {
        crate::i18n::t("search-kind-people")
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        if needle.trim().is_empty() {
            return Vec::new();
        }
        self.people
            .iter()
            .filter_map(|person| {
                let score = rank_fields(needle, &[(&person.name, FieldWeight::TITLE)])?;
                Some(Hit {
                    // The person's own face, as the bar's stack and every chip draw it.
                    row: Some(SearchRow::Command(Box::new(CommandRow {
                        avatar: Some(RowFace::of_member(
                            person.name.clone(),
                            person.id.clone(),
                            false,
                        )),
                        ..CommandRow::default()
                    }))),
                    preview: None,
                    faces: Vec::new(),
                    outcome: None,
                    kind: people_kind(),
                    provider: <Self as Source>::id(self).into(),
                    id: person.id.clone(),
                    title: person.name.clone(),
                    subtitle: None,
                    facts: Vec::new(),
                    score,
                    icon: None,
                    target: Target::Invoke(Box::new(
                        InvokeTarget::action(
                            ui::apps::CHAT,
                            &self.workspace,
                            ui::chat::CONVERSATIONS,
                            ui::chat::MESSAGE_PERSON,
                        )
                        .with_argument("who", serde_json::json!({ "person": person.id })),
                    )),
                })
            })
            .collect()
    }
}
