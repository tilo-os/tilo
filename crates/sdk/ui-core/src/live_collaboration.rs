//! Workspace-level collaboration experience shared by every application.
//!
//! Pointer/caret presence answers *where*. This module answers the two human
//! questions that should not be inferred independently by every app: what a
//! participant is doing, and what they are trying to achieve. Inference is
//! deliberately local and transient; only the resulting short status is fit
//! for publication, and a manual status always wins.

use crate::time::{Duration, SystemTime};
use std::collections::HashMap;

use crate::permissions::UserId;

/// Automatic summaries go stale quickly. A stale intention is worse than no
/// status because it makes collaborators coordinate around work that stopped.
pub const INFERRED_STATUS_TTL: Duration = Duration::from_secs(5 * 60);
/// Manual statuses are explicit communication and therefore remain until the
/// author clears them.
pub const MAX_STATUS_CHARS: usize = 96;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusSharing {
    /// No inferred status is produced or published. Manual status remains
    /// available so privacy never removes the useful explicit path.
    Off,
    /// Only a status typed by the participant is visible.
    ManualOnly,
    /// A local model may summarize recent semantic actions.
    #[default]
    Automatic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusOrigin {
    Manual,
    /// Name of the local generator, surfaced in details rather than disguised
    /// as something the participant literally wrote.
    Inferred {
        generator: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveStatus {
    pub text: String,
    pub origin: StatusOrigin,
    pub updated_at: SystemTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticActivity {
    /// Human verb phrase supplied by an app's semantic action layer, e.g.
    /// "Editing assumptions". Never derived from raw keys or pointer motion.
    pub action: String,
    pub app: String,
    pub resource: Option<String>,
    pub occurred_at: SystemTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonPresence {
    pub id: UserId,
    pub name: String,
    pub is_self: bool,
    pub is_agent: bool,
    pub current: Option<SemanticActivity>,
    pub status: Option<LiveStatus>,
}

impl PersonPresence {
    pub fn visible_status(&self, now: SystemTime) -> Option<&LiveStatus> {
        self.status.as_ref().filter(|status| match status.origin {
            StatusOrigin::Manual => true,
            StatusOrigin::Inferred { .. } => now
                .duration_since(status.updated_at)
                .is_ok_and(|age| age <= INFERRED_STATUS_TTL),
        })
    }
}

/// State behind the universal "People" surface. It contains no model runner
/// and no transport: hosts feed it semantic activity and a grounded candidate
/// returned by their on-device summarizer, then publish `shareable_status`.
#[derive(Clone, Debug, Default)]
pub struct CollaborationExperience {
    people: HashMap<UserId, PersonPresence>,
    sharing: StatusSharing,
}

impl CollaborationExperience {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sharing(&self) -> StatusSharing {
        self.sharing
    }

    pub fn set_sharing(&mut self, sharing: StatusSharing) {
        self.sharing = sharing;
        if sharing != StatusSharing::Automatic {
            for person in self.people.values_mut().filter(|person| person.is_self) {
                if person
                    .status
                    .as_ref()
                    .is_some_and(|status| matches!(status.origin, StatusOrigin::Inferred { .. }))
                {
                    person.status = None;
                }
            }
        }
    }

    pub fn upsert_person(&mut self, person: PersonPresence) {
        self.people.insert(person.id.clone(), person);
    }

    /// Stable UI order: self first, then people, then agents; names break ties.
    pub fn people(&self) -> Vec<&PersonPresence> {
        let mut people: Vec<_> = self.people.values().collect();
        people.sort_by_key(|person| {
            (
                !person.is_self,
                person.is_agent,
                person.name.to_lowercase(),
                person.id.0.clone(),
            )
        });
        people
    }

    pub fn note_activity(&mut self, id: &UserId, activity: SemanticActivity) {
        if let Some(person) = self.people.get_mut(id) {
            person.current = Some(activity);
        }
    }

    /// Install a local-model candidate for the local participant. Candidates
    /// are bounded and scrubbed before they can become shareable. Manual text
    /// is never overwritten by inference.
    pub fn accept_inferred_status(
        &mut self,
        id: &UserId,
        candidate: &str,
        generator: impl Into<String>,
        now: SystemTime,
    ) -> bool {
        if self.sharing != StatusSharing::Automatic {
            return false;
        }
        let Some(person) = self.people.get_mut(id).filter(|person| person.is_self) else {
            return false;
        };
        if person
            .status
            .as_ref()
            .is_some_and(|status| matches!(status.origin, StatusOrigin::Manual))
        {
            return false;
        }
        let Some(text) = sanitize_status(candidate) else {
            return false;
        };
        person.status = Some(LiveStatus {
            text,
            origin: StatusOrigin::Inferred {
                generator: generator.into(),
            },
            updated_at: now,
        });
        true
    }

    /// Empty text clears the override and allows automatic inference to resume.
    pub fn set_manual_status(&mut self, id: &UserId, text: &str, now: SystemTime) -> bool {
        let Some(person) = self.people.get_mut(id).filter(|person| person.is_self) else {
            return false;
        };
        if text.trim().is_empty() {
            person.status = None;
            return true;
        }
        let Some(text) = sanitize_status(text) else {
            return false;
        };
        person.status = Some(LiveStatus {
            text,
            origin: StatusOrigin::Manual,
            updated_at: now,
        });
        true
    }

    /// The only projection suitable for the presence transport. Keeping this
    /// accessor narrow prevents raw action trails from becoming presence data.
    pub fn shareable_status(&self, id: &UserId, now: SystemTime) -> Option<&LiveStatus> {
        let person = self.people.get(id)?;
        let status = person.visible_status(now)?;
        match (&status.origin, self.sharing) {
            (StatusOrigin::Manual, _)
            | (StatusOrigin::Inferred { .. }, StatusSharing::Automatic) => Some(status),
            _ => None,
        }
    }
}

fn sanitize_status(value: &str) -> Option<String> {
    let one_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.is_empty() || one_line.chars().any(char::is_control) {
        return None;
    }
    Some(one_line.chars().take(MAX_STATUS_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local() -> PersonPresence {
        PersonPresence {
            id: UserId("me".into()),
            name: "ada".into(),
            is_self: true,
            is_agent: false,
            current: None,
            status: None,
        }
    }

    #[test]
    fn live_status_prefers_manual_over_inference_and_respects_privacy() {
        // Case `manual_status_wins_and_clearing_it_resumes_inference`.
        {
            let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
            let mut experience = CollaborationExperience::new();
            experience.upsert_person(local());
            let me = UserId("me".into());
            assert!(experience.set_manual_status(&me, "Reviewing totals", now));
            assert!(!experience.accept_inferred_status(&me, "Editing sheet", "local", now));
            assert_eq!(
                experience.shareable_status(&me, now).unwrap().text,
                "Reviewing totals"
            );
            assert!(experience.set_manual_status(&me, "", now));
            assert!(experience.accept_inferred_status(&me, "Editing sheet", "local", now));
        }

        // Case `privacy_switch_removes_inference_but_preserves_explicit_status`.
        {
            let now = SystemTime::UNIX_EPOCH;
            let mut experience = CollaborationExperience::new();
            experience.upsert_person(local());
            let me = UserId("me".into());
            assert!(experience.accept_inferred_status(&me, "Drafting Q2 report", "local", now));
            experience.set_sharing(StatusSharing::Off);
            assert!(experience.shareable_status(&me, now).is_none());
            assert!(experience.set_manual_status(&me, "Heads-down until 3", now));
            assert_eq!(
                experience.shareable_status(&me, now).unwrap().text,
                "Heads-down until 3"
            );
        }

        // Case `inferred_intent_expires_and_status_is_safe_for_a_single_line`.
        {
            let now = SystemTime::UNIX_EPOCH;
            let mut experience = CollaborationExperience::new();
            experience.upsert_person(local());
            let me = UserId("me".into());
            assert!(experience.accept_inferred_status(
                &me,
                "  Reconciling\n Q2   finance numbers  ",
                "local",
                now,
            ));
            assert_eq!(
                experience.shareable_status(&me, now).unwrap().text,
                "Reconciling Q2 finance numbers"
            );
            let later = now + INFERRED_STATUS_TTL + Duration::from_secs(1);
            assert!(experience.shareable_status(&me, later).is_none());
        }

        // Case `roster_order_is_predictable_and_agents_are_visually_separable`.
        {
            let mut experience = CollaborationExperience::new();
            experience.upsert_person(PersonPresence {
                id: UserId("agent".into()),
                name: "Research agent".into(),
                is_self: false,
                is_agent: true,
                current: None,
                status: None,
            });
            experience.upsert_person(PersonPresence {
                id: UserId("ada".into()),
                name: "Ada".into(),
                is_self: false,
                is_agent: false,
                current: None,
                status: None,
            });
            experience.upsert_person(local());
            let names: Vec<_> = experience
                .people()
                .iter()
                .map(|person| person.name.as_str())
                .collect();
            assert_eq!(names, ["ada", "Ada", "Research agent"]);
        }
    }
}
