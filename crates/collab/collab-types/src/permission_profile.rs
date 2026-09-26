//! Named, reusable permission profiles for people and agents.
//!
//! A profile is authoring sugar, not a second enforcement model: assignment
//! expands to ordinary [`CapOverride`] values and every decision still goes
//! through `caps::decide_capability`. This is the stable object an agent or
//! permissions UI can create before assigning the same least-privilege bundle
//! to several collaborators.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::caps::{CapOverride, Effect, Expiry, Principal, Scope, Subject, Verb};

const MAX_NAME_CHARS: usize = 80;
const MAX_DESCRIPTION_CHARS: usize = 320;
const MAX_RULES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionProfileRule {
    pub scope: Scope,
    pub effect: Effect,
    pub verb: Verb,
}

impl PermissionProfileRule {
    pub fn grant(scope: Scope, verb: Verb) -> Self {
        Self {
            scope,
            effect: Effect::Grant,
            verb,
        }
    }

    pub fn restrict(scope: Scope, verb: Verb) -> Self {
        Self {
            scope,
            effect: Effect::Restrict,
            verb,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionProfile {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub rules: Vec<PermissionProfileRule>,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PermissionProfileError {
    #[error("profile name must contain 1 to {MAX_NAME_CHARS} printable characters")]
    InvalidName,
    #[error("profile description exceeds {MAX_DESCRIPTION_CHARS} characters")]
    DescriptionTooLong,
    #[error("profile must contain 1 to {MAX_RULES} rules")]
    InvalidRuleCount,
    #[error("profile contains the same scope and effect more than once")]
    DuplicateRule,
    #[error("profile contains both a grant and restriction for the same scope")]
    ContradictoryRule,
}

impl PermissionProfile {
    /// # Errors
    ///
    /// Returns the corresponding [`PermissionProfileError`] when profile metadata or rules are
    /// invalid or ambiguous.
    pub fn validate(&self) -> Result<(), PermissionProfileError> {
        if self.name.trim().is_empty()
            || self.name.chars().count() > MAX_NAME_CHARS
            || self.name.chars().any(char::is_control)
        {
            return Err(PermissionProfileError::InvalidName);
        }
        if self.description.chars().count() > MAX_DESCRIPTION_CHARS
            || self.description.chars().any(char::is_control)
        {
            return Err(PermissionProfileError::DescriptionTooLong);
        }
        if self.rules.is_empty() || self.rules.len() > MAX_RULES {
            return Err(PermissionProfileError::InvalidRuleCount);
        }
        let mut exact = HashSet::new();
        for rule in &self.rules {
            let scope = canonical_scope(&rule.scope);
            let grants = rule.effect == Effect::Grant;
            if !exact.insert((scope.clone(), grants)) {
                return Err(PermissionProfileError::DuplicateRule);
            }
            if exact.contains(&(scope, !grants)) {
                return Err(PermissionProfileError::ContradictoryRule);
            }
        }
        Ok(())
    }

    /// Compile one atomic assignment. Callers validate first, then append the
    /// returned vector as one governance transaction; a partial role can
    /// therefore never become effective.
    ///
    /// # Errors
    ///
    /// Returns a [`PermissionProfileError`] when the profile does not pass [`Self::validate`].
    pub fn assign_to(
        &self,
        subject: &Subject,
        granted_by: &Principal,
        expiry: Expiry,
    ) -> Result<Vec<CapOverride>, PermissionProfileError> {
        self.validate()?;
        // Capability ids are workspace-wide revocation handles. Bind the
        // subject so assigning one profile to several people cannot make one
        // person's revoke collide with another person's assignment.
        let profile_id = short_hash(format!("{}\0{subject:?}", self.name.trim()).as_bytes());
        Ok(self
            .rules
            .iter()
            .enumerate()
            .map(|(index, rule)| {
                CapOverride::new(subject.clone(), rule.scope.clone(), rule.effect, rule.verb)
                    .with_id(format!("profile:{profile_id}:{index}"))
                    .expiring(expiry)
                    .granted_by(granted_by.clone())
            })
            .collect())
    }
}

fn canonical_scope(scope: &Scope) -> String {
    // Scope's serde representation is stable and unambiguous; debug output is
    // used only as an in-memory validation key, never as a wire identity.
    format!("{scope:?}")
}

fn short_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest.iter().copied().take(12).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::{Actor, CapDecision, ResourceRef, decide_capability};

    /// An app and sub-action profile is real enforced authority, and ambiguous profiles are rejected
    /// before any assignment exists.
    #[test]
    fn permission_profiles_are_enforced_authority_and_unambiguous() {
        // Phase: app_and_sub_action_profile_is_real_enforced_authority.
        {
            let profile = PermissionProfile {
                name: "Release reviewer".into(),
                description: "Can inspect Builds and approve exactly one action".into(),
                rules: vec![
                    PermissionProfileRule::grant(Scope::App("org.tilo.Builds".into()), Verb::Read),
                    PermissionProfileRule::grant(
                        Scope::Action {
                            app: "org.tilo.Builds".into(),
                            action: "release.approve".into(),
                        },
                        Verb::Write,
                    ),
                ],
            };
            let caps = profile
                .assign_to(
                    &Subject::person("ada"),
                    &Principal::Person("owner".into()),
                    Expiry::Never,
                )
                .unwrap();
            let actor = Actor::person(None, "ada");
            assert_eq!(
                decide_capability(
                    &actor,
                    &ResourceRef::App("org.tilo.Builds".into()),
                    Verb::Read,
                    &caps,
                    None
                ),
                CapDecision::Allow
            );
            assert_eq!(
                decide_capability(
                    &actor,
                    &ResourceRef::Action {
                        app: "org.tilo.Builds".into(),
                        action: "release.approve".into()
                    },
                    Verb::Write,
                    &caps,
                    None
                ),
                CapDecision::Allow
            );
            assert_ne!(
                decide_capability(
                    &actor,
                    &ResourceRef::Action {
                        app: "org.tilo.Builds".into(),
                        action: "release.delete".into()
                    },
                    Verb::Write,
                    &caps,
                    None
                ),
                CapDecision::Allow
            );
            assert!(
                caps.iter()
                    .all(|cap| cap.id.starts_with("profile:") && cap.granted_by.is_some())
            );
        }

        // Phase: rejects_ambiguous_profiles_before_any_assignment_exists.
        {
            let profile = PermissionProfile {
                name: "Confused".into(),
                description: String::new(),
                rules: vec![
                    PermissionProfileRule::grant(Scope::App("notes".into()), Verb::Write),
                    PermissionProfileRule::restrict(Scope::App("notes".into()), Verb::Read),
                ],
            };
            assert_eq!(
                profile.assign_to(
                    &Subject::person("ada"),
                    &Principal::Person("owner".into()),
                    Expiry::Never
                ),
                Err(PermissionProfileError::ContradictoryRule)
            );
        }
    }
}
