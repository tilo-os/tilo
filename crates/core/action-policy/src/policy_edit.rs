//! A workspace's members, grants and grant sets as one host keeps them: the
//! answers every reader of a [`WorkspacePolicy`] asks at its own clock, the
//! changes a host installs without a check, and what a checked change wrote.
//!
//! Every question here takes the caller's clock as `now`, Unix seconds:
//! grants expire, and the policy does not read a clock of its own.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    ActionKind, Answer, Authority, Change, Context, Grant, GrantId, Grantee, KindSet, Lifetime,
    Preset, PresetError, Refusal, Role, TreePath, WorkspacePolicy, roles,
};

/// What a change wrote into one workspace's policy. Empty means nothing
/// changed: the actor lacked authority, or there was nothing to do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicyEdit {
    /// People whose membership was set or ended.
    pub members: Vec<String>,
    pub granted: Vec<GrantId>,
    pub revoked: Vec<GrantId>,
    /// Ids of the app capabilities granted ([`crate::Grant`]s are in `granted`).
    pub sandbox_granted: Vec<String>,
    pub sandbox_revoked: Vec<String>,
    pub custom_roles: Vec<Change>,
}

impl PolicyEdit {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
            && self.granted.is_empty()
            && self.revoked.is_empty()
            && self.sandbox_granted.is_empty()
            && self.sandbox_revoked.is_empty()
            && self.custom_roles.is_empty()
    }

    #[must_use]
    pub fn and(mut self, other: Self) -> Self {
        self.members.extend(other.members);
        self.granted.extend(other.granted);
        self.revoked.extend(other.revoked);
        self.sandbox_granted.extend(other.sandbox_granted);
        self.sandbox_revoked.extend(other.sandbox_revoked);
        self.custom_roles.extend(other.custom_roles);
        self
    }

    /// What changed from `before` to `after`.
    #[must_use]
    pub fn between(before: &WorkspacePolicy, after: &WorkspacePolicy) -> Self {
        let mut members: Vec<String> = before
            .members
            .iter()
            .filter(|(person, role)| after.members.get(*person) != Some(role))
            .map(|(person, _)| person.clone())
            .collect();
        members.extend(
            after
                .members
                .keys()
                .filter(|person| !before.members.contains_key(*person))
                .cloned(),
        );
        let granted = after
            .grants
            .iter()
            .filter(|grant| !before.grants.contains(grant))
            .map(|grant| grant.id.clone())
            .collect();
        let revoked = before
            .grants
            .iter()
            .filter(|grant| after.grants.iter().all(|held| held.id != grant.id))
            .filter(|grant| {
                !matches!(&grant.grantee, Grantee::CustomRole(id) if !after.custom_roles.contains_key(id))
            })
            .map(|grant| grant.id.clone())
            .collect();
        let mut custom_roles = Vec::new();
        for (id, name) in &after.custom_roles {
            if before.custom_roles.get(id) != Some(name) {
                custom_roles.push(Change::DefineRole {
                    id: id.clone(),
                    name: name.clone(),
                });
            }
        }
        for id in before.custom_roles.keys() {
            if !after.custom_roles.contains_key(id) {
                custom_roles.push(Change::RemoveRole { id: id.clone() });
            }
        }
        let empty = BTreeSet::new();
        for person in after.members.keys() {
            let held = before.assigned_roles.get(person).unwrap_or(&empty);
            let wanted = after.assigned_roles.get(person).unwrap_or(&empty);
            custom_roles.extend(
                held.symmetric_difference(wanted)
                    .filter(|role| after.custom_roles.contains_key(*role))
                    .map(|role| Change::SetMemberCustomRole {
                        person: person.clone(),
                        role: role.clone(),
                        assigned: wanted.contains(role),
                    }),
            );
        }
        Self {
            members,
            granted,
            revoked,
            custom_roles,
            ..Self::default()
        }
    }
}

/// Why a grant set was not saved or assigned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresetMutationError {
    Refused(Refusal),
    Invalid(PresetError),
    NotFound,
}

impl std::fmt::Display for PresetMutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(f),
            Self::Invalid(error) => write!(f, "grant set is invalid: {error}"),
            Self::NotFound => f.write_str("grant set was not found"),
        }
    }
}

impl std::error::Error for PresetMutationError {}

impl From<Refusal> for PresetMutationError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<PresetError> for PresetMutationError {
    fn from(error: PresetError) -> Self {
        Self::Invalid(error)
    }
}

impl WorkspacePolicy {
    /// What `person`'s own authority answers for `kind` on `node` at `now`.
    #[must_use]
    pub fn answer_at(&self, person: &str, node: &TreePath, kind: ActionKind, now: u64) -> Answer {
        Authority::person(self.party(person))
            .decide(node, kind, &Context::at_secs(now))
            .answer
    }

    /// Whether `person` may take the workspace's own action `action` (one of
    /// [`roles::governance`]) at `now`.
    #[must_use]
    pub fn may_change_at(&self, person: &str, action: &str, now: u64) -> bool {
        self.answer_at(
            person,
            &roles::governance_action(&self.workspace, action),
            roles::GOVERNANCE_KIND,
            now,
        ) == Answer::Allow
    }

    /// [`Self::may_change_at`], as the refusal a caller returns.
    ///
    /// # Errors
    /// [`Refusal::NotPermitted`] when `person` may not.
    pub fn require_at(&self, person: &str, action: &str, now: u64) -> Result<(), Refusal> {
        if self.may_change_at(person, action, now) {
            Ok(())
        } else {
            Err(Refusal::NotPermitted(format!(
                "{person} may not {action} in this workspace"
            )))
        }
    }

    /// Apply `change` made by `actor` as themselves at `now`.
    ///
    /// # Errors
    /// See [`WorkspacePolicy::apply`].
    pub fn change_as_at(&mut self, actor: &str, change: Change, now: u64) -> Result<(), Refusal> {
        let policy = self.clone();
        let authority = Authority::person(policy.party(actor));
        self.apply(actor, &authority, change, &Context::at_secs(now), false)
    }

    /// Apply `changes` as `actor` at `now`, all or none: each is checked
    /// against the policy as the ones before it left it. What changed.
    ///
    /// # Errors
    /// The first change refused; nothing is applied then.
    pub fn change_all_as_at(
        &mut self,
        actor: &str,
        changes: Vec<Change>,
        now: u64,
    ) -> Result<PolicyEdit, Refusal> {
        let before = self.clone();
        for change in changes {
            if let Err(refusal) = self.change_as_at(actor, change, now) {
                *self = before;
                return Err(refusal);
            }
        }
        Ok(PolicyEdit::between(&before, self))
    }

    /// Record a grant without a check, for a host installing the workspace's
    /// own state (an accepted invite, a role preset, a consent the person
    /// gave on their device). Its id; a grant with the same id replaces the
    /// earlier one.
    pub fn grant(&mut self, grant: Grant) -> GrantId {
        let id = grant.id.clone();
        self.grants.retain(|held| held.id != id);
        self.grants.push(grant);
        id
    }

    /// Drop the grant with this id. Whether anything was removed.
    pub fn revoke_grant(&mut self, id: &GrantId) -> bool {
        let before = self.grants.len();
        self.grants.retain(|grant| &grant.id != id);
        self.grants.len() != before
    }

    /// Drop every grant matching `pred`. The ids removed.
    pub fn revoke_grants_matching(&mut self, pred: impl Fn(&Grant) -> bool) -> Vec<GrantId> {
        let mut removed = Vec::new();
        self.grants.retain(|grant| {
            if pred(grant) {
                removed.push(grant.id.clone());
                false
            } else {
                true
            }
        });
        removed
    }

    /// Admit `person` with `role`, without a check (the host installing an
    /// accepted invite or the workspace's first state). The owner stays owner.
    pub fn add_member(&mut self, person: impl Into<String>, role: Role) {
        let person = person.into();
        if !self.is_owner(&person) {
            self.members.insert(person, role);
        }
    }

    /// Remove `person`'s membership and personal grants, without a check.
    /// The owner stays. Whether they were a member.
    pub fn dismiss(&mut self, person: &str) -> bool {
        if self.is_owner(person) {
            return false;
        }
        let grantee = Grantee::person(person.to_owned());
        self.grants.retain(|grant| grant.grantee != grantee);
        self.joined.remove(person);
        self.members.remove(person).is_some()
    }

    /// Install `person` as owner; the previous owner, if any, holds nothing more.
    pub fn set_owner(&mut self, person: impl Into<String>) {
        let person = person.into();
        self.members.remove(&person);
        self.owner = Some(person);
    }

    /// Everyone in the workspace with their role: the owner first, then the
    /// members by id.
    #[must_use]
    pub fn people(&self) -> Vec<(String, Role)> {
        self.owner
            .iter()
            .map(|owner| (owner.clone(), Role::Owner))
            .chain(
                self.members
                    .iter()
                    .map(|(person, role)| (person.clone(), *role)),
            )
            .collect()
    }

    /// The people who may read the whole workspace at `now`, each with when
    /// that lapses (Unix seconds, exclusive): `None` for its members and
    /// anyone granted until revoked, the end of a time-limited grant
    /// otherwise. A grant that ends with the session is no reach and is left out.
    #[must_use]
    pub fn readers_at(&self, now: u64) -> Vec<(String, Option<u64>)> {
        let workspace_node = self.node();
        let cx = Context::at_secs(now);
        let mut readers: BTreeMap<String, Option<u64>> = self
            .people()
            .into_iter()
            .map(|(person, _)| (person, None))
            .collect();
        for grant in &self.grants {
            let Some(person) = grant.grantee.person_id() else {
                continue;
            };
            if grant.answer != Answer::Allow
                || !grant.kinds.contains(ActionKind::Read)
                || !grant.node.covers(&workspace_node)
            {
                continue;
            }
            let until = match grant.lifetime {
                Lifetime::Until(ms) => Some(ms / 1000),
                Lifetime::UntilRevoked => None,
                Lifetime::Once | Lifetime::Session => continue,
            };
            let held = readers.entry(person.to_owned()).or_insert(until);
            *held = match (*held, until) {
                (Some(a), Some(b)) => Some(a.max(b)),
                _ => None,
            };
        }
        readers
            .into_iter()
            .filter(|(person, _)| {
                Authority::person(self.party(person))
                    .decide(&workspace_node, ActionKind::Read, &cx)
                    .answer
                    == Answer::Allow
            })
            .collect()
    }

    /// The agents the workspace admits at `now`: those with a live grant on
    /// the workspace itself, with what it allows them.
    #[must_use]
    pub fn agent_members_at(&self, now: u64) -> Vec<(crate::MemberId, KindSet)> {
        let cx = Context::at_secs(now);
        let workspace_node = self.node();
        let mut agents: Vec<(crate::MemberId, KindSet)> = Vec::new();
        for grant in &self.grants {
            let Some(agent) = grant.grantee.as_member().filter(|member| member.is_agent()) else {
                continue;
            };
            if grant.node != workspace_node || grant.answer == Answer::Deny || !grant.is_live(&cx) {
                continue;
            }
            match agents.iter_mut().find(|(listed, _)| listed == agent) {
                Some((_, kinds)) => *kinds = kinds.union(grant.kinds),
                None => agents.push((agent.clone(), grant.kinds)),
            }
        }
        agents
    }

    /// The grants on one node below the workspace at `now`: the share list of a
    /// resource, apart from the workspace's members. A person's grant names them;
    /// an app's names the app and stands while the person who gave it still
    /// reaches the node ([`Grant::stands_in`]), so the file's sharing shows what
    /// each person opened in an app and whose it is to take back.
    #[must_use]
    pub fn shares_at(&self, node: &TreePath, now: u64) -> Vec<&Grant> {
        let cx = Context::at_secs(now);
        let workspace_node = self.node();
        self.grants
            .iter()
            .filter(|grant| {
                (grant.grantee.person_id().is_some() || grant.grantee.app_id().is_some())
                    && grant.node != workspace_node
                    && grant.answer != Answer::Deny
                    && grant.is_live(&cx)
                    && grant.node.covers(node)
            })
            .filter(|grant| grant.grantee.app_id().is_none() || grant.stands_in(self, &cx))
            .collect()
    }

    /// The first instant after `now` (Unix seconds) at which one of the
    /// grants stops being live. `None` while none expires later.
    #[must_use]
    pub fn next_grant_expiry_after(&self, now: u64) -> Option<u64> {
        self.grants
            .iter()
            .filter_map(|grant| until_secs(grant.lifetime))
            .filter(|deadline| *deadline > now)
            .min()
    }

    /// Create or replace a named grant set in `presets`, as `authority`. The
    /// set itself grants nothing; assigning it does.
    ///
    /// # Errors
    /// [`PresetMutationError::Refused`] when `authority` may not change the
    /// workspace's settings, [`PresetMutationError::Invalid`] when `preset`
    /// does not validate.
    pub fn save_preset_at(
        &self,
        presets: &mut Vec<Preset>,
        authority: &Authority<'_>,
        mut preset: Preset,
        now: u64,
    ) -> Result<(), PresetMutationError> {
        self.check_action(
            authority,
            roles::governance::SETTINGS,
            &Context::at_secs(now),
            false,
        )?;
        preset.validate()?;
        // A grant set is kept below no workspace in particular: assigning it
        // places it in this one.
        for rule in &mut preset.rules {
            rule.node = rule.node.within_workspace();
        }
        if let Some(existing) = presets
            .iter_mut()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(preset.name.trim()))
        {
            *existing = preset;
        } else {
            presets.push(preset);
            sort_presets(presets);
        }
        Ok(())
    }

    /// Assign the saved grant set `name` of `presets` to `subject`, as
    /// `actor` acting through `authority`. Assigning is granting, so the
    /// workspace's grant action covers every set; a grant on one set's
    /// assign node covers that set only. No grant written is above the
    /// actor's own. The ids written.
    ///
    /// # Errors
    /// [`PresetMutationError::NotFound`] for an unknown set, or
    /// [`PresetMutationError::Refused`] when the assignment is refused.
    pub fn assign_preset_at(
        &mut self,
        presets: &[Preset],
        actor: &str,
        authority: &Authority<'_>,
        name: &str,
        subject: &Grantee,
        lifetime: &Lifetime,
        now: u64,
    ) -> Result<Vec<GrantId>, PresetMutationError> {
        let cx = Context::at_secs(now);
        let preset = presets
            .iter()
            .find(|preset| preset.name.eq_ignore_ascii_case(name.trim()));
        let workspace = self.workspace.clone();
        let verdict = preset.map_or_else(
            || {
                authority.decide(
                    &roles::governance_action(&workspace, roles::governance::GRANT),
                    roles::GOVERNANCE_KIND,
                    &cx,
                )
            },
            |preset| roles::assign_preset_verdict(authority, &workspace, &preset.name, &cx),
        );
        if verdict.answer != Answer::Allow {
            return Err(Refusal::NotPermitted(verdict.explain()).into());
        }
        let preset = preset.ok_or(PresetMutationError::NotFound)?;
        let prefix = format!(
            "preset:{}:{}",
            uuid::Uuid::new_v4().simple(),
            preset.name.trim().to_lowercase()
        );
        let workspace_node = self.node();
        let grants: Vec<Grant> = preset
            .assign(
                &prefix,
                subject,
                &Grantee::person(actor.to_owned()),
                lifetime,
            )
            .into_iter()
            .map(|grant| Grant {
                node: workspace_node.join(&grant.node),
                ..grant
            })
            .collect();
        for grant in &grants {
            if grant.node.workspace_id() != Some(workspace.as_str()) {
                return Err(Refusal::OutsideWorkspace(grant.node.clone()).into());
            }
            Self::check_within(authority, grant, &cx)?;
        }
        let ids = grants.iter().map(|grant| grant.id.clone()).collect();
        self.grants.extend(grants);
        Ok(ids)
    }
}

/// Order a workspace's grant sets by name, ignoring case: the one order every
/// reader and writer of them keeps.
pub fn sort_presets(presets: &mut [Preset]) {
    presets.sort_by_cached_key(|preset| preset.name.to_lowercase());
}

/// When a grant of `lifetime` stops being live, Unix seconds: `None` for one
/// that does not lapse with time.
#[must_use]
pub fn until_secs(lifetime: Lifetime) -> Option<u64> {
    match lifetime {
        // Live while the clock is before `at` milliseconds.
        Lifetime::Until(at) => Some(at.div_ceil(1000)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Level, Party, Rule};

    fn policy() -> WorkspacePolicy {
        let mut policy = WorkspacePolicy::unowned("ws".to_owned(), &Grantee::role(Role::Owner));
        policy.set_owner("owner");
        policy
    }

    /// A grant on one set's assign node lets its holder assign that set and
    /// no other, and a delegation hands out no more than it holds itself.
    #[test]
    fn a_grant_to_assign_one_set_assigns_that_set_only() {
        let mut policy = policy();
        let workspace_id = policy.workspace.clone();
        let mut presets = Vec::new();
        for name in ["Reviewer", "Admin helper"] {
            let preset = Preset::new(
                name,
                "",
                vec![Rule::new(
                    TreePath::workspace(&workspace_id).app("so.tilo.notes"),
                    KindSet::READ,
                    Answer::Allow,
                )],
            );
            let owner = policy.clone();
            policy
                .save_preset_at(
                    &mut presets,
                    &Authority::person(owner.party("owner")),
                    preset,
                    0,
                )
                .unwrap();
        }
        policy.grant(Grant::new(
            "scribe-assigns-reviewer",
            Grantee::agent("scribe", "owner"),
            roles::preset_assign_node(&workspace_id, "Reviewer"),
            KindSet::OUTSIDE,
            Answer::Allow,
            Grantee::person("owner"),
        ));
        policy.grant(Grant::new(
            "scribe-reads-notes",
            Grantee::agent("scribe", "owner"),
            TreePath::workspace(&workspace_id).app("so.tilo.notes"),
            KindSet::READ,
            Answer::Allow,
            Grantee::person("owner"),
        ));
        let ada = Grantee::person("ada");
        let assign = |policy: &mut WorkspacePolicy, name: &str| {
            let snapshot = policy.clone();
            let scribe = Authority::person(snapshot.party("owner")).bounded_by(Party::new(
                vec![Grantee::agent("scribe", "owner")],
                &snapshot.grants,
            ));
            policy.assign_preset_at(
                &presets,
                "owner",
                &scribe,
                name,
                &ada,
                &Lifetime::UntilRevoked,
                0,
            )
        };
        assert!(
            assign(&mut policy, "Reviewer").is_ok(),
            "the granted set is assigned"
        );
        assert!(
            assign(&mut policy, "Admin helper").is_err(),
            "another set is not"
        );
        let reading = TreePath::root()
            .push(Level::Workspace, workspace_id.as_str())
            .app("so.tilo.notes");
        assert_eq!(
            policy.answer_at("ada", &reading, ActionKind::Read, 0),
            Answer::Allow
        );
    }

    /// A workspace with `owner` and a member `ada` in `role`.
    fn workspace_with(role: Role) -> WorkspacePolicy {
        let mut policy = WorkspacePolicy::new("ws", "owner");
        policy.add_member("ada", role);
        policy
    }

    /// One file of the workspace's tree, as a chosen file's node is named.
    fn file_node() -> TreePath {
        TreePath::workspace("ws")
            .app(crate::roles::FILES_APP)
            .kind("file")
            .resource("record")
    }

    /// The grant an app is given on that file, by `person`, for reading and, with `write`,
    /// changing it.
    fn app_grant(person: &str, write: bool) -> Grant {
        Grant::new(
            format!(
                "chosen:so.tilo.settings:record:{}",
                if write { "write" } else { "read" }
            ),
            Grantee::app("so.tilo.settings"),
            file_node(),
            if write {
                KindSet::READ_UNDOABLE
            } else {
                KindSet::READ
            },
            Answer::Allow,
            Grantee::person(person),
        )
    }

    #[test]
    fn a_member_grants_an_app_on_a_node_they_reach() {
        let mut policy = workspace_with(Role::Editor);
        policy
            .change_as_at("ada", Change::Grant(app_grant("ada", false)), 0)
            .expect("an editor opens a file they can read in an app");
        let held = policy
            .grants
            .iter()
            .find(|grant| grant.grantee == Grantee::app("so.tilo.settings"))
            .expect("the workspace keeps the grant");
        assert_eq!(
            held.granted_by,
            Grantee::person("ada"),
            "history names them"
        );
        assert!(
            held.stands_in(&policy, &Context::at(0)),
            "the app holds what its person still reaches"
        );
    }

    #[test]
    fn a_member_cannot_grant_on_a_node_they_do_not_reach() {
        let mut policy = workspace_with(Role::Editor);
        policy.grant(Grant::new(
            "denied-to-ada",
            Grantee::person("ada"),
            file_node(),
            KindSet::ALL,
            Answer::Deny,
            Grantee::person("owner"),
        ));
        assert!(
            policy
                .change_as_at("ada", Change::Grant(app_grant("ada", false)), 0)
                .is_err(),
            "a node they are refused is no node to open in an app"
        );
    }

    #[test]
    fn a_member_cannot_grant_more_than_they_hold_and_a_reader_grants_no_write() {
        // A viewer reads and does not change: reading is theirs to give, changing is not.
        let mut policy = workspace_with(Role::Viewer);
        policy
            .change_as_at("ada", Change::Grant(app_grant("ada", false)), 0)
            .expect("a reader opens a file they can read");
        assert!(
            policy
                .change_as_at("ada", Change::Grant(app_grant("ada", true)), 0)
                .is_err(),
            "a reader cannot give what they do not hold"
        );
    }

    #[test]
    fn an_app_grant_ends_when_its_person_loses_the_workspace() {
        let mut policy = workspace_with(Role::Editor);
        policy
            .change_as_at("ada", Change::Grant(app_grant("ada", false)), 0)
            .expect("the grant stands");
        policy
            .change_as_at(
                "owner",
                Change::RemoveMember {
                    person: "ada".to_owned(),
                },
                0,
            )
            .expect("the owner removes them");
        let held = policy
            .grants
            .iter()
            .find(|grant| grant.grantee == Grantee::app("so.tilo.settings"))
            .expect("the grant is still recorded");
        assert!(
            !held.stands_in(&policy, &Context::at(0)),
            "what they gave ends with their reach, with nothing to revoke"
        );
    }

    #[test]
    fn the_person_who_gave_an_app_grant_takes_it_back_and_another_member_does_not() {
        let mut policy = workspace_with(Role::Editor);
        policy.add_member("bob", Role::Editor);
        policy
            .change_as_at("ada", Change::Grant(app_grant("ada", false)), 0)
            .expect("the grant stands");
        let id = GrantId::new("chosen:so.tilo.settings:record:read");
        assert!(
            policy
                .change_as_at("bob", Change::Revoke(id.clone()), 0)
                .is_err(),
            "another member can neither reach nor take back what they gave"
        );
        policy
            .change_as_at("ada", Change::Revoke(id.clone()), 0)
            .expect("the person who gave it takes it back");
        assert!(
            !policy.grants.iter().any(|grant| grant.id == id),
            "the grant is gone"
        );
    }

    #[test]
    fn a_sharing_manager_takes_an_app_grant_back() {
        let mut policy = workspace_with(Role::Editor);
        policy
            .change_as_at("ada", Change::Grant(app_grant("ada", false)), 0)
            .expect("the grant stands");
        policy
            .change_as_at(
                "owner",
                Change::Revoke(GrantId::new("chosen:so.tilo.settings:record:read")),
                0,
            )
            .expect("whoever manages the file's sharing takes it back");
    }

    #[test]
    fn a_files_sharing_names_the_app_it_was_opened_in_and_who_gave_it() {
        let mut policy = workspace_with(Role::Editor);
        policy
            .change_as_at("ada", Change::Grant(app_grant("ada", false)), 0)
            .expect("the grant stands");
        let shares = policy.shares_at(&file_node(), 0);
        assert_eq!(shares.len(), 1, "the app is listed beside the people");
        assert_eq!(
            shares[0].grantee,
            Grantee::app("so.tilo.settings"),
            "the app the file was opened in"
        );
        assert_eq!(
            shares[0].granted_by,
            Grantee::person("ada"),
            "the person whose it is to take back"
        );
    }

    #[test]
    fn a_files_sharing_leaves_out_an_app_grant_whose_person_lost_the_file() {
        let mut policy = workspace_with(Role::Editor);
        policy
            .change_as_at("ada", Change::Grant(app_grant("ada", false)), 0)
            .expect("the grant stands");
        policy
            .change_as_at(
                "owner",
                Change::RemoveMember {
                    person: "ada".to_owned(),
                },
                0,
            )
            .expect("the owner removes them");
        assert!(
            policy.shares_at(&file_node(), 0).is_empty(),
            "what ended is not listed for anyone to take back"
        );
    }
}
