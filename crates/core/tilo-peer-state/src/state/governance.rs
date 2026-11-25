//! The membership governance this peer authors and reads: members, roles,
//! grants, delegations and revocations as the workspace document records them.
use super::facades::{PeerGovernance, PeerGovernanceMut};
use super::{
    BTreeMap, BTreeSet, DeviceCert, DeviceRow, Grant, GrantId, Grantee, Member, MemberAccess,
    MemberId, MemberList, MemberListAccess, MembershipError, Person, ROOT, ReadDoc, RevocationList,
    Role, SystemTime, Transactable, err, keeping_device_keys_of, now_ns, person_of, workspace,
    workspace_map,
};
use crate::join::WorkspaceId;
use collab_types::signer::DeviceSigner;
use tilo_action_policy::Change;

/// A state of a workspace's governance, kept to ask which heads it holds
/// ([`PeerGovernance::held_heads`]). Cheap to keep: it shares the state.
#[derive(Clone, Default)]
pub struct GovernanceHeads(Option<crate::cached_document::CachedDocument>);

impl GovernanceHeads {
    /// Whether this state holds every head of `heads`: a few hash lookups,
    /// whatever it is asked.
    #[must_use]
    pub fn holds(&self, heads: &[[u8; 32]]) -> bool {
        heads.iter().all(|head| {
            self.0
                .as_ref()
                .is_some_and(|doc| doc.has_change(&automerge::ChangeHash(*head)))
        })
    }

    /// Whether this state holds a change for every head an op names
    /// ([`collab_types::SignedOp::governance`]). Asked only of a change that
    /// would be refused: a head that is one of the governance's own is found
    /// at once, an older one by a pass over its change hashes.
    #[must_use]
    pub fn holds_named(&self, heads: &[[u8; 4]]) -> bool {
        let Some(doc) = self.0.as_ref() else {
            return heads.is_empty();
        };
        let current: Vec<_> = automerge::transaction::Transactable::base_heads(&**doc)
            .iter()
            .map(|head| collab_types::governance_head(&head.0))
            .collect();
        heads.iter().all(|head| {
            current.contains(head)
                || doc
                    .committed_document()
                    .change_hashes()
                    .iter()
                    .any(|hash| collab_types::governance_head(&hash.0) == *head)
        })
    }
}

impl PeerGovernance<'_> {
    /// The governance as it stands now, to ask which heads it holds: what a
    /// server that decides changes against an access read from this state
    /// keeps beside it, so a change refused while the governance is past it
    /// waits for the next.
    #[must_use]
    pub fn held_heads(self) -> GovernanceHeads {
        GovernanceHeads(self.peer.core.workspace_doc.clone())
    }

    /// The name the workspace keeps beside each device row and each person's root, as a test
    /// reads who a device was admitted under.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn kept_names(self) -> std::collections::BTreeMap<String, String> {
        self.peer
            .core
            .workspace_doc
            .as_ref()
            .map(|doc| doc.policy().names.clone())
            .unwrap_or_default()
    }
    /// [`Self::access_of`] `devices` at `now`, with the governance it was read
    /// from ([`Self::held_heads`]).
    ///
    /// # Errors
    ///
    /// As [`Self::access_of`].
    #[allow(
        clippy::type_complexity,
        reason = "the access and the governance state it was read from, as one projection"
    )]
    pub fn projection(
        self,
        devices: &[[u8; 32]],
        now: SystemTime,
    ) -> Result<
        (
            std::collections::BTreeMap<[u8; 32], tilo_action_policy::MemberAccess>,
            GovernanceHeads,
        ),
        String,
    > {
        Ok((self.access_of(devices, now)?, self.held_heads()))
    }
    /// The governance's heads now.
    #[must_use]
    pub fn heads(self) -> Vec<[u8; 32]> {
        self.peer.governance_heads().unwrap_or_default()
    }
    /// How far the workspace's member list has come: the number of signed
    /// operations it holds that prove their writer. Every device holding the
    /// same operations answers the same, and each change to the list counts
    /// one more.
    #[must_use]
    pub fn generation(self) -> u64 {
        self.peer
            .core
            .workspace_doc
            .as_deref()
            .map_or(0, |doc| crate::members::ops(doc).len() as u64)
    }
    /// When the workspace was made, ms since the epoch: the time of the
    /// earliest change its governance holds, its founder's seed. A later
    /// change to it does not move it.
    ///
    /// `None` when the governance holds no change, or none that names a time.
    #[must_use]
    pub fn made_at_ms(self) -> Option<i64> {
        let doc = self.peer.core.workspace_doc.as_ref()?;
        let at = doc
            .committed_document()
            .get_changes_meta(&[])
            .iter()
            .map(|change| change.timestamp)
            .min()?
            .checked_mul(1000)?;
        (at > 0).then_some(at)
    }
    /// Whether the governance carries a device's removal without its cut
    /// ([`crate::removals`]): one recorded before cuts were kept. Until the
    /// device that hosts the workspace completes it
    /// ([`PeerGovernanceMut::publish_member_revocations`]), nobody decides
    /// what that device wrote.
    #[must_use]
    pub fn lacks_removal_cuts(self) -> bool {
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return false;
        };
        let Ok(recorded) = crate::removals::recorded(doc) else {
            return false;
        };
        doc.identities()
            .values()
            .filter_map(|identity| identity.as_ref().ok())
            .flat_map(|identity| &identity.revocations)
            .any(|entry| !recorded.contains(&entry.revoked_device_public_key))
    }
    /// The sandbox capabilities the workspace's governance grants its apps.
    pub fn member_sandbox(self) -> Result<Vec<collab_types::caps::CapOverride>, String> {
        let doc = self
            .peer
            .core
            .workspace_doc
            .as_ref()
            .ok_or("workspace governance missing")?;
        workspace::governance_of(Some(doc))
            .sandbox
            .values()
            .map(|bytes| serde_json::from_slice(bytes).map_err(err))
            .collect()
    }
    /// The access of every member device at `now`: what a server
    /// that does not hold the workspace's governance decides their changes
    /// with, each held to its foreign-device bound.
    pub fn member_device_access(
        self,
        now: SystemTime,
    ) -> Result<std::collections::BTreeMap<[u8; 32], tilo_action_policy::MemberAccess>, String>
    {
        let access = self.peer.governance().member_list_access(now)?;
        self.access_of(&access.member_devices, now)
    }
    /// [`Self::member_device_access`] of `devices`, the member devices
    /// [`Self::member_list_access`] at `now` answered: a host that just listed
    /// them for its access does not list them again.
    pub fn access_of(
        self,
        devices: &[[u8; 32]],
        now: SystemTime,
    ) -> Result<std::collections::BTreeMap<[u8; 32], tilo_action_policy::MemberAccess>, String>
    {
        let doc = self
            .peer
            .core
            .workspace_doc
            .as_ref()
            .ok_or("workspace governance missing")?;
        // One policy, shared by every device's access, and the grants its
        // person approved, walked once for all of them: walking the
        // workspace's certificates per device made admitting one cost more
        // the more devices the workspace had.
        let policy = std::sync::Arc::new(doc.workspace_policy(&self.peer.core.workspace).clone());
        let approved = self.peer.approved_keys(now);
        Ok(devices
            .iter()
            .copied()
            .filter_map(|device| {
                self.peer
                    .member_with(device, std::sync::Arc::clone(&policy), &approved, now)
                    .ok()
                    .map(|member| (device, member))
            })
            .collect())
    }
    /// Current member access, derived from the workspace's member set: its
    /// members by their roots, and the devices of theirs it lists now.
    ///
    /// [`Self::access_of`], and how many times deriving it walked the
    /// workspace's certificates and grants: the tests hold that to one walk
    /// however many devices the workspace names, a count that does not pass by
    /// waiting longer.
    ///
    /// # Errors
    ///
    /// As [`Self::access_of`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn access_of_with_walks(
        self,
        devices: &[[u8; 32]],
        now: SystemTime,
    ) -> Result<
        (
            std::collections::BTreeMap<[u8; 32], tilo_action_policy::MemberAccess>,
            u64,
        ),
        String,
    > {
        let (access, walks) = super::authority::walks::taken(|| self.access_of(devices, now));
        Ok((access?, walks))
    }

    /// A person is a member by their root, whatever devices of theirs the
    /// workspace lists now: a page's certificate lasts an hour and its row
    /// goes with it, and a person whose last page expired, or whose browser
    /// forgot the site, is still a member when they come back on a device
    /// their root certifies anew.
    ///
    /// Each identity row is decoded and its certificate's signature verified
    /// once per document state: a page in a workspace of hundreds of people
    /// lists its member devices for every frame its channels take in.
    pub fn member_list_access(self, now: SystemTime) -> Result<MemberListAccess, String> {
        let people = self.listed_members(now)?;
        let mut member_devices = Vec::new();
        let mut member_user_roots = Vec::new();
        for (root, member) in &people {
            if member.removed {
                continue;
            }
            member_user_roots.push(*root);
            member_devices.extend(member.devices.iter().copied());
        }
        // A key its person revoked is let in nowhere: its device leaves the workspace's access,
        // and a session of it that is open is told so and let go.
        member_devices.retain(|device| !self.revoked_key(device));
        member_devices.sort_unstable();
        member_devices.dedup();
        member_user_roots.sort_unstable();
        member_user_roots.dedup();
        Ok(MemberListAccess {
            member_devices,
            member_user_roots,
        })
    }

    /// Whether the workspace's governance names `device` as a device of a
    /// person it no longer has as a member: one it removed, or whose part
    /// ended. A device it never named is not one; neither is any while no
    /// governance is held here.
    pub fn removed_device(self, device: [u8; 32]) -> bool {
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return false;
        };
        let policy = doc.policy();
        let workspace_policy = doc.workspace_policy(&self.peer.core.workspace);
        let mut named = false;
        for bytes in policy.identities.values() {
            let Ok(identity) = DeviceRow::decode(bytes) else {
                continue;
            };
            if identity.device_cert.device_public_key != device {
                continue;
            }
            let person = hex::encode(identity.device_cert.user_public_key);
            if workspace_policy.active_role(&person).is_some() {
                return false;
            }
            named = true;
        }
        named
    }

    /// Whether the workspace has met `device` and no longer lists it: its row
    /// is marked removed, or the member list carries none while this copy
    /// holds its certificate. A device the workspace took off its member list
    /// ([`PeerGovernanceMut::forget_device`]) stays out until a deliberate
    /// admission writes its row again, never by presenting the certificate
    /// the workspace already has. A device the workspace never met is not
    /// one; neither is any while no governance is held here.
    ///
    /// The row a removal marked stays for the admission, delegation and
    /// revocation it carries, which history and verification name; this is
    /// the one place that reads its mark.
    pub fn removed_from_member_list(self, device: &DeviceCert) -> bool {
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return false;
        };
        match doc.identities().get(&hex::encode(device.device_public_key)) {
            Some(Ok(row)) => return row.removed,
            Some(Err(_)) => return false,
            None => {}
        }
        self.peer
            .core
            .certificates
            .get(&device.device_public_key)
            .is_some_and(|held| held.as_ref().user_public_key == device.user_public_key)
    }

    /// Whether the member list holds `device`'s own row, under this certificate and not marked
    /// removed: what a device that signed its way in finds once its row arrived. A server that
    /// keeps the workspace has its row as any device of its person does, though the workspace
    /// counts it among its servers rather than its members' devices ([`Self::member_list_access`]).
    pub fn lists_device(self, device: &DeviceCert) -> bool {
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return false;
        };
        matches!(
            doc.identities().get(&hex::encode(device.device_public_key)),
            Some(Ok(row)) if !row.removed && row.device_cert == *device
        )
    }

    /// Whether `key`'s device may issue an invitation into the workspace at
    /// `now`: a current member whose part there grants inviting. What the
    /// surfaces that offer inviting show; an invitation's write path asks it
    /// again, with the part its link hands out.
    pub fn may_invite(self, key: &dyn DeviceSigner, now: SystemTime) -> bool {
        self.peer.require_inviter(key, now).is_ok()
    }

    /// Whether `key`'s device may hand out `role` in an invitation into the
    /// workspace at `now`: what the surfaces offering a part to grant list.
    pub fn may_invite_role(self, key: &dyn DeviceSigner, role: Role, now: SystemTime) -> bool {
        self.peer
            .require_invitation_authority(key, role, now)
            .is_ok()
    }

    /// Whether `key`'s device may invite someone as an administrator at
    /// `now`: whether the surfaces offering a part to grant offer it.
    pub fn may_invite_administrator(self, key: &dyn DeviceSigner, now: SystemTime) -> bool {
        self.peer.require_administrator_invitation(key, now).is_ok()
    }

    /// The device the workspace was created on, which founds its MLS group;
    /// every other device takes the group it hands over.
    pub fn founder(self) -> Option<[u8; 32]> {
        let doc = self.peer.core.workspace_doc.as_ref()?;
        let (value, _) = doc.get(ROOT, super::seed::FOUNDER).ok()??;
        let hex = value.into_string().ok()?;
        <[u8; 32]>::try_from(hex::decode(hex).ok()?).ok()
    }

    /// The devices that hold the workspace's keys at `now`: every device of
    /// its members and every holder it names as holding them, and no holder
    /// it names as a store of its ciphertext, whoever's device that is. The
    /// workspace's one MLS group follows this set, so a holder named a store
    /// leaves the group, and the commit that removes it rotates the keys.
    ///
    /// A device its person's root confined to one workspace (a workspace's
    /// VM) holds no workspace's keys, this one's included: what it reads
    /// reaches it as its person's records do on a session, never as the
    /// group's keys, so a page that escapes it on that machine opens nothing
    /// sealed to the group.
    pub fn key_holders(self, now: SystemTime) -> Result<BTreeSet<[u8; 32]>, String> {
        let access = self.peer.governance().member_list_access(now)?;
        let mut holders: BTreeSet<[u8; 32]> = BTreeSet::new();
        let mut stores: BTreeSet<[u8; 32]> = BTreeSet::new();
        // A holder the member list names keeps the workspace: a key holder among them takes the
        // keys beside the members' devices, while it stays certified and unrevoked. Being named
        // a holder lets it keep and carry the workspace; it makes nobody a member.
        for hint in self.keeping_devices() {
            let cert = &hint.host;
            if super::holder_role(&hint) == super::CopyRole::Store {
                stores.insert(cert.device_public_key);
            } else if tilo_user_identity::verify_device_cert_at(cert, now_ns(now)).is_ok()
                && cert.capabilities.workspace.is_none()
                && !self.peer.core.revocations.refuses(cert)
                && !self.revoked_key(&cert.device_public_key)
            {
                holders.insert(cert.device_public_key);
            }
        }
        holders.extend(access.member_devices);
        let confined = |device: &[u8; 32]| {
            self.peer
                .core
                .certificates
                .get(device)
                .is_some_and(|cert| cert.capabilities.workspace.is_some())
        };
        Ok(holders
            .into_iter()
            .filter(|device| !stores.contains(device) && !confined(device))
            .collect())
    }

    /// The devices of the readers `named` names among the workspace's key
    /// holders at `now` ([`Self::key_holders`]): who holds the keys of a group
    /// only they read — its people, and every member whose role includes the
    /// role it names, as the governance has them now. A person it no longer
    /// has as a member, one whose role no longer includes the set's, and a
    /// revoked key hold none, so the commit that follows any of them rotates
    /// that group's keys; one given the role joins it by the next commit.
    pub fn reader_key_holders(
        self,
        named: &collab_types::NamedReaders,
        now: SystemTime,
    ) -> Result<BTreeSet<[u8; 32]>, String> {
        let members = self.listed_members(now)?;
        let reads = |root: &[u8; 32]| {
            named.includes(
                root,
                members
                    .get(root)
                    .filter(|member| !member.removed)
                    .map(|member| member.role),
            )
        };
        let mut theirs: BTreeSet<[u8; 32]> = members
            .iter()
            .filter(|(root, member)| !member.removed && reads(root))
            .flat_map(|(_, member)| member.devices.iter().copied())
            .collect();
        // A device keeping the workspace for a person reads as that person does, by name or
        // by the role they hold.
        theirs.extend(
            self.keeping_devices()
                .iter()
                .filter(|hint| reads(&hint.host.user_public_key))
                .map(|hint| hint.host.device_public_key),
        );
        let mut holders = self.key_holders(now)?;
        holders.retain(|device| theirs.contains(device));
        Ok(holders)
    }

    /// Whether the person of root key `person` reads the set `named` at
    /// `now`: one of its people, or a member the workspace has not removed
    /// whose role includes the set's ([`collab_types::NamedReaders::includes`]).
    pub fn reads_named(
        self,
        named: &collab_types::NamedReaders,
        person: &[u8; 32],
        now: SystemTime,
    ) -> bool {
        if named.people.contains(person) {
            return true;
        }
        named.role.is_some()
            && self.listed_members(now).is_ok_and(|members| {
                members.get(person).is_some_and(|member| {
                    !member.removed && named.includes(person, Some(member.role))
                })
            })
    }

    /// The workspace's people and grants as its governance records them: its
    /// owner, its members with the part each takes, when each was admitted,
    /// and the grants on its nodes. This is the one source every list of its
    /// people shows.
    pub fn member_policy(self) -> Result<tilo_action_policy::WorkspacePolicy, String> {
        let doc = self
            .peer
            .core
            .workspace_doc
            .as_ref()
            .ok_or("workspace governance missing")?;
        Ok(doc.workspace_policy(&self.peer.core.workspace).clone())
    }

    /// The issuance time of each live grant, from the signed change that wrote it.
    #[must_use]
    pub fn member_grant_times(&self, grants: &[Grant]) -> std::collections::BTreeMap<GrantId, u64> {
        use crate::members::{self, MembershipOpCommand};
        let mut times = std::collections::BTreeMap::<GrantId, u64>::new();
        if grants.is_empty() {
            return times;
        }
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return times;
        };
        for op in members::ops(doc).values() {
            let MembershipOpCommand::Project { changes } = &op.command else {
                continue;
            };
            for change in changes {
                if let Change::Grant(grant) = change
                    && grants.contains(grant)
                {
                    times
                        .entry(grant.id.clone())
                        .and_modify(|at| *at = (*at).max(op.at_ns))
                        .or_insert(op.at_ns);
                }
            }
        }
        times
    }
    /// How the workspace's membership log stands against its rows, for an
    /// operator: how many operations it holds, whether it has started, and
    /// each way the membership it replays to differs from the one the rows
    /// hold (log first). Where they differ, every membership change this
    /// host writes on top is refused.
    pub fn membership_log(self) -> Result<Vec<String>, String> {
        let doc = self
            .peer
            .core
            .workspace_doc
            .as_ref()
            .ok_or("workspace governance missing")?;
        let workspace = &self.peer.core.workspace;
        let held = crate::members::ops(doc);
        let mut said = vec![format!(
            "{} membership operations; owner {:?}",
            held.len(),
            crate::members::replay(doc, workspace).owner
        )];
        for op in held.values() {
            let command = match &op.command {
                crate::members::MembershipOpCommand::Admit { person, .. } => {
                    format!("admission of {}", hex::encode(person))
                }
                crate::members::MembershipOpCommand::Enter { person, .. } => {
                    format!("entry admission of {}", hex::encode(person))
                }
                crate::members::MembershipOpCommand::Governance { rows } => {
                    format!("governance of {} rows", rows.len())
                }
                crate::members::MembershipOpCommand::Readmit { device, .. } => {
                    format!("readmission of {}", hex::encode(device))
                }
                crate::members::MembershipOpCommand::Project { changes } => {
                    format!("{changes:?}").chars().take(400).collect()
                }
                crate::members::MembershipOpCommand::Compact { .. } => "compaction".into(),
            };
            said.push(format!(
                "  op by {} on device {} at {}: {command}",
                op.person(),
                hex::encode(op.author.device_public_key),
                op.at_ns
            ));
        }
        Ok(said)
    }
    /// The workspace's members as its signed governance records them, and the
    /// devices of theirs it lists now: every person its policy gives a role,
    /// its owner among them, by their root, whether or not a device of theirs
    /// has joined; and whoever it removed, kept so a change of theirs still
    /// reads as theirs. A device the workspace names among its keepers is
    /// listed as its person's too, though its access comes from the keepers.
    /// It is the workspace's member set, named.
    pub fn member_list(self, now: SystemTime) -> Result<MemberList, String> {
        let doc = self
            .peer
            .core
            .workspace_doc
            .as_ref()
            .ok_or("workspace governance missing")?;
        let people = self.listed_members(now)?;
        // A device the workspace names among its keepers holds it by that
        // name and is no member's device for its access ([`Self::listed_members`]).
        // A page still shows it as the device of the person who keeps it
        // there, so a window it serves or what it narrates reads as theirs.
        let keeping: Vec<([u8; 32], [u8; 32])> = self
            .keeping_devices()
            .into_iter()
            .filter(|hint| {
                tilo_user_identity::verify_device_cert_at(&hint.host, now_ns(now)).is_ok()
                    && !self.peer.core.revocations.refuses(&hint.host)
            })
            .map(|hint| (hint.host.user_public_key, hint.host.device_public_key))
            .collect();
        // Read once per document state ([`crate::cached_document`]), so
        // listing the people costs no decoding or signature check for a row
        // a change left as it was.
        let names = &doc.policy().names;
        // The name each person goes by here, which one of their own devices
        // says and no key of theirs does ([`Naming`]).
        let mut naming: BTreeMap<[u8; 32], Naming> = BTreeMap::new();
        for (id, identity) in doc.identities() {
            let identity = identity.as_ref().map_err(err)?;
            naming
                .entry(identity.device_cert.user_public_key)
                .or_default()
                .learn(
                    &identity.device_cert,
                    identity.delegate.is_none(),
                    names.get(id).map(String::as_str),
                );
        }
        let mut members = Vec::with_capacity(people.len());
        let mut devices: BTreeMap<MemberId, Vec<[u8; 32]>> = BTreeMap::new();
        for (root, held) in people {
            let mut row = Member::of_person(&Person::new(hex::encode(root), ""), Some(held.role));
            row.removed = held.removed;
            row.administrator = held.administrator;
            row.grants = u32::try_from(held.grants).unwrap_or(u32::MAX);
            // The name the person goes by here: the one the workspace keeps
            // under their root, which names them before any device of theirs
            // arrived and wherever they last renamed themselves, else the one
            // their own oldest device says.
            let kept = names
                .get(&hex::encode(root))
                .filter(|name| !name.trim().is_empty())
                .map(String::as_str);
            if let Some(name) = kept.or_else(|| naming.get(&root).and_then(Naming::name)) {
                name.clone_into(&mut row.name);
            }
            if !held.devices.is_empty() {
                devices.insert(row.id.clone(), held.devices);
            }
            members.push(row);
        }
        for (root, device) in keeping {
            let Some(member) = members
                .iter()
                .find(|member| member.root() == Some(root) && !member.removed)
            else {
                continue;
            };
            let devices = devices.entry(member.id.clone()).or_default();
            if !devices.contains(&device) {
                devices.push(device);
            }
        }
        Ok(MemberList { members, devices })
    }

    /// The workspace's member set as its signed governance gives it, by the
    /// root each person's devices are certified by: their part, whether the
    /// workspace removed them, and the devices of theirs it lists now — the
    /// one membership its member list ([`Self::member_list`]) and its
    /// access ([`Self::member_list_access`]) are both read from.
    fn listed_members(self, now: SystemTime) -> Result<BTreeMap<[u8; 32], ListedMember>, String> {
        let doc = self
            .peer
            .core
            .workspace_doc
            .as_ref()
            .ok_or("workspace governance missing")?;
        // A device the workspace names among its servers holds it by that
        // name: it is no member's device, though it founded the workspace, so
        // no member list names it as one.
        let serving: BTreeSet<[u8; 32]> = keeping_device_keys_of(doc);
        let workspace_policy = doc.workspace_policy(&self.peer.core.workspace);
        let certificates = doc.identity_certificates();
        // Who the grants make administrators, and how many each gives a person beyond their
        // role, read in one walk of them: asked person by person, a workspace of hundreds walked
        // every grant for every member.
        let grants = workspace_policy.grants_by_person();
        crate::work::grants_walked();
        let administrator = |root: &[u8; 32]| grants.administers(&hex::encode(root));
        let mut people: BTreeMap<[u8; 32], ListedMember> = BTreeMap::new();
        // A person is a member by their root, whatever devices of theirs the
        // workspace has rows for: its owner from the workspace's beginning,
        // and someone declared before any device of theirs arrived, or whose
        // last page's row expired, holds their role all the same, so a key of
        // theirs finds the workspace and enters it at that role.
        for person in workspace_policy
            .owner
            .iter()
            .chain(workspace_policy.members.keys())
        {
            let Some(role) = workspace_policy.active_role(person) else {
                continue;
            };
            let mut root = [0; 32];
            // A person named by anything but a root key has no root a device
            // could be certified by.
            if tilo_failure::ignored!(
                "a person named by something other than a root key has no root, so no device of \
                 theirs is admitted by one",
                hex::decode_to_slice(person, &mut root).is_err()
            ) {
                continue;
            }
            let member = ListedMember {
                role,
                // Its owner keeps the part they were given first, whatever
                // else the policy says of the same person.
                administrator: role == Role::Owner || administrator(&root),
                grants: grants.extra(person),
                removed: false,
                devices: Vec::new(),
            };
            people.entry(root).or_insert(member);
        }
        for (id, identity) in doc.identities() {
            let identity = identity.as_ref().map_err(err)?;
            let root = identity.device_cert.user_public_key;
            // A person the workspace removed keeps the part their admission
            // gave them, so what they did here still reads as theirs.
            if let Some(receipt) = identity
                .admission
                .as_ref()
                .or_else(|| identity.admissions.last())
                && let std::collections::btree_map::Entry::Vacant(held) = people.entry(root)
            {
                held.insert(ListedMember {
                    role: receipt.role,
                    administrator: administrator(&root),
                    grants: 0,
                    removed: true,
                    devices: Vec::new(),
                });
            }
            let Some(member) = people.get_mut(&root).filter(|member| !member.removed) else {
                continue;
            };
            // An agent outside its delegation reads and writes nothing.
            if identity
                .delegate_acting(&self.peer.core.workspace, now_ns(now))
                .is_err()
            {
                continue;
            }
            let cert = &identity.device_cert;
            if certificates
                .get(id)
                .is_some_and(|verified| verified.current_typed(now_ns(now)).is_ok())
                && !self.peer.core.revocations.refuses(cert)
                && !serving.contains(&cert.device_public_key)
            {
                member.devices.push(cert.device_public_key);
            }
        }
        for member in people.values_mut() {
            member.devices.sort_unstable();
            member.devices.dedup();
        }
        Ok(people)
    }

    pub fn owner_user_public_key(self) -> [u8; 32] {
        self.peer.core.owner
    }
    /// Issue a membership refresh challenge for a member device.
    ///
    /// # Errors
    ///
    /// Returns an error if the device is not a current member at `now` or the
    /// challenge cannot be signed.
    pub fn membership_challenge(
        self,
        workspace_id: WorkspaceId,
        host: &DeviceCert,
        device_cert: &DeviceCert,
        request_id: [u8; 32],
        nonce: [u8; 32],
        expires_at_ns: u64,
        now: SystemTime,
    ) -> Result<crate::join::MembershipChallenge, String> {
        // A renewed certificate is challenged as the device it renews; its
        // membership is decided once the host has it
        // ([`PeerGovernanceMut::renew_member_certificate`]). A device the
        // workspace holds no certificate for renews nothing: it is a stranger
        // or a further device of a member, told apart below.
        let held = self
            .peer
            .core
            .certificates
            .contains_key(&device_cert.device_public_key);
        if !held || self.renewal_of(device_cert, now)?.is_none() {
            if let Err(error) = self.peer.certificate(&device_cert.device_public_key, now) {
                // A further device of a person the workspace lists is
                // challenged as theirs, and sends its own row with its proof
                // ([`crate::own_admission::sign_further_device_row`]).
                tilo_user_identity::verify_device_cert_at(device_cert, now_ns(now)).map_err(err)?;
                let further = !self.peer.core.revocations.refuses(device_cert)
                    && self.peer.core.workspace_doc.as_ref().is_some_and(|doc| {
                        doc.workspace_policy(&self.peer.core.workspace)
                            .active_role(&hex::encode(device_cert.user_public_key))
                            .is_some()
                    });
                if !further {
                    return Err(error.to_string());
                }
                return crate::join::MembershipChallenge::new(
                    workspace_id,
                    host,
                    device_cert,
                    request_id,
                    nonce,
                    expires_at_ns,
                    now_ns(now),
                );
            }
            // A device the workspace admitted and then removed is refused as
            // removed, not as a stranger.
            match self.peer.permissions(device_cert.device_public_key, now) {
                Ok(_) => {}
                Err(MembershipError::NoMembership)
                    if self.peer.has_listed_admission(device_cert) =>
                {
                    return Err("member has been removed".into());
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        crate::join::MembershipChallenge::new(
            workspace_id,
            host,
            device_cert,
            request_id,
            nonce,
            expires_at_ns,
            now_ns(now),
        )
    }
    /// The role a member's device that proved `membership` holds now, as the
    /// answer to it names it: the person's role in the workspace, which is
    /// what the device takes part as. An answer never names the owner's
    /// authority, which governance alone decides; a device of the owner hears
    /// the strongest member role.
    pub fn member_role(
        self,
        membership: &crate::join::VerifiedMembership,
        now: SystemTime,
    ) -> Result<crate::join::Role, String> {
        Ok(match self.peer.current_membership_role(membership, now)? {
            Role::Owner => Role::Moderator,
            role => role,
        })
    }
}

/// One person a workspace has, as its signed governance records them: the part
/// they hold, whether it removed them, and the devices of theirs it lists now.
struct ListedMember {
    role: Role,
    administrator: bool,
    /// The permissions the workspace gives them beyond their role.
    grants: usize,
    removed: bool,
    devices: Vec<[u8; 32]>,
}

/// How one person is named in a workspace, from the identities of theirs it
/// recorded: the name their own oldest device goes by, or none.
///
/// A person holds a workspace through devices of their own — a laptop, a
/// phone, a page — and through keys made to act for them: an agent's grant, a
/// server key a backend holds, a key minted for one workspace. Every one of
/// them is an identity of the same person, each named in the workspace by its
/// own label or by the invitation that admitted it, so the person's row takes
/// its name from a device of theirs and never from a key's label: "Jane Doe's
/// agent" names the agent, not Jane. A person who names themself
/// ([`Self::name_person`]'s caller) has that name kept under their root too,
/// which is what their row says. A person the workspace knows only through
/// keys made for them, and whom nobody named, has no name here, and the row
/// says so rather than borrowing a key's.
#[derive(Default)]
pub(super) struct Naming {
    /// The name of their own device whose certificate was issued earliest,
    /// with when it was issued and that device's key, the tie-break.
    own: Option<(u64, [u8; 32], String)>,
}

impl Naming {
    /// Take the name `identity` goes by when it is a device of the person's
    /// own: one they hold and act with, not one made to act for them. `theirs`
    /// is false for a delegate of theirs, an agent, which
    /// goes by a name of its own whatever its certificate says.
    pub(super) fn learn(&mut self, identity: &DeviceCert, theirs: bool, name: Option<&str>) {
        let capabilities = &identity.capabilities;
        if !theirs || capabilities.agent.is_some() || capabilities.stand_in {
            return;
        }
        // The stand-in a device nobody named carries ("Primary device") is no name of the person's.
        let Some(name) = name.and_then(tilo_user_identity::device_name) else {
            return;
        };
        let issued = identity.issued_at_ns;
        let device = identity.device_public_key;
        if self
            .own
            .as_ref()
            .is_some_and(|(at, key, _)| (issued, device) >= (*at, *key))
        {
            return;
        }
        self.own = Some((issued, device, name.to_owned()));
    }

    /// The person's name here, when the workspace recorded one.
    pub(super) fn name(&self) -> Option<&str> {
        self.own.as_ref().map(|(_, _, name)| name.as_str())
    }
}
/// What the workspace's own record of a device decides about admitting it.
enum Decision {
    /// The member list holds the device already.
    Held,
    /// A later certificate of the same device replaces the one the workspace
    /// keeps.
    Renewed,
    /// The workspace never met the device.
    Fresh,
}

impl PeerGovernanceMut<'_> {
    /// Apply an already authorized document rule as one membership op.
    pub fn change_document_access(
        self,
        changes: Vec<Change>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_member_writer(key, now)?;
        let document = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        self.peer.write_changes(document, changes, key, now)
    }

    /// Sign one custom-role change through the workspace's governance: it
    /// holds as far as the signer's grants reach, on every device that
    /// replays it.
    pub fn change_member_list(
        self,
        change: Change,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        if !change.is_custom_role_change() {
            return Err("expected a custom role change".into());
        }
        self.peer.require_member_writer(key, now)?;
        let document = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        // Giving a member a role of the workspace's own, or taking it, changes that member;
        // defining a role changes nobody.
        let stamp = matches!(change, Change::SetMemberCustomRole { .. })
            .then(|| {
                self.peer
                    .own_action(tilo_action_policy::roles::governance::SET_ROLE, key, now)
            })
            .flatten();
        self.peer.naming(stamp, |peer| {
            peer.write_changes(document, vec![change], key, now)
        })
    }

    /// Publish root-signed revocations into pinned governance, so every
    /// replica refuses the revoked devices. The authority's root revokes any
    /// of its keys, kept on the authority's own row too so a device revoked
    /// before it was ever admitted stays refused; any other root revokes only
    /// a device the workspace certifies to it. With each removal goes its cut
    /// ([`crate::removals`]): where each document's history stood for the
    /// removed device as this replica holds it, written once, also for a
    /// removal the governance carried before cuts were kept. Returns whether
    /// governance changed.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make, an entry's root
    /// may not revoke its device here, or governance cannot be updated.
    pub fn publish_member_revocations(
        self,
        entries: &[tilo_user_identity::RevocationEntry],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let owner = self.peer.core.owner;
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let rows = super::identity_rows::identity_rows(&doc).map_err(err)?;
        let mut verified = self.peer.core.revocations.clone();
        for entry in entries {
            let certified = rows.iter().any(|(_, _, identity)| {
                identity.device_cert.device_public_key == entry.revoked_device_public_key
                    && identity.device_cert.user_public_key == entry.user_public_key
            });
            if entry.user_public_key != owner && !certified {
                return Err("revocation root is not the member list's".into());
            }
            verified.insert(entry.clone()).map_err(err)?;
        }
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        let mut projected = rows;
        for (_, _, identity) in &mut projected {
            let root = identity.device_cert.user_public_key;
            let own_row = root == owner;
            let mut list = RevocationList::new();
            for entry in &identity.revocations {
                list.insert(entry.clone()).map_err(err)?;
            }
            for entry in verified
                .iter()
                .filter(|entry| entry.user_public_key == root)
            {
                if own_row
                    || entry.revoked_device_public_key == identity.device_cert.device_public_key
                {
                    list.insert(entry.clone()).map_err(err)?;
                }
            }
            let mut written = list.iter().cloned().collect::<Vec<_>>();
            written.sort_by_key(|entry| entry.revoked_device_public_key);
            identity.revocations = written;
        }
        let mut changed =
            super::identity_rows::put_identity_rows(&mut doc, &projected).map_err(err)?;
        // With every removal the governance carries goes its cut, written
        // once: where each document's history stood for the removed device, as
        // this replica holds it now. A removal the governance carried before
        // cuts were kept gets its cut here too: this replica refused
        // everything of the device since, so what it holds of it is what the
        // workspace had taken.
        let removed: BTreeSet<[u8; 32]> = projected
            .iter()
            .flat_map(|(_, _, identity)| &identity.revocations)
            .map(|entry| entry.revoked_device_public_key)
            .collect();
        for device in removed {
            changed |= crate::removals::write(&mut doc, &device, &self.peer.taken_from(&device))
                .map_err(err)?;
        }
        if changed {
            self.peer.write_governance(doc, &before, key, now)?;
        } else {
            self.peer.core.revocations = verified;
        }
        Ok(changed)
    }
    /// Atomically records a verified single-use invitation redemption, its
    /// independent root-certified device, and its workspace role, written by
    /// `key`: the redeeming device itself. The invitation is checked on its
    /// own ([`crate::workspace::membership::invitation_admits`]), as every
    /// receiver checks it.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the redeeming device's own, the
    /// invitation does not admit the redemption, it conflicts with
    /// recorded governance, or governance cannot be updated.
    pub fn admit_invited_member(
        self,
        redemption: &crate::join::VerifiedRedemption,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.admit_named_member(redemption, None, key, now)
    }
    /// [`Self::admit_invited_member`], naming the person `name` in the same
    /// change, as [`Self::name_person`] would after it: the name every member
    /// shows for them on each of their admitted devices' rows. `None` keeps
    /// the name their invitation and certificate give.
    ///
    /// # Errors
    ///
    /// As [`Self::admit_invited_member`], and when `name` is not a display
    /// name ([`tilo_user_identity::display_name`]).
    pub fn admit_named_member(
        self,
        redemption: &crate::join::VerifiedRedemption,
        name: Option<&str>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let name = name
            .map(|name| {
                tilo_user_identity::display_name(name)
                    .map_err(tilo_user_identity::DisplayNameError::reason)
            })
            .transpose()?;
        if redemption.join().grant().workspace_id.to_string() != self.peer.core.workspace {
            return Err("invitation is not for this workspace".into());
        }
        // The redeeming device writes its own redemption, the invitation its
        // authority: every receiver checks the invitation itself.
        let cert = redemption.certificate();
        if self.peer.core.revocations.refuses(cert) {
            return Err("device revoked".into());
        }
        if let Some(old) = self.peer.core.certificates.get(&cert.device_public_key)
            && old.as_ref() != cert
            && !super::supersedes(old.as_ref(), cert)
        {
            return Err("certificate conflict".into());
        }
        let cached = self
            .peer
            .core
            .workspace_doc
            .as_ref()
            .ok_or("workspace governance missing")?;
        let mut document = (**cached).clone();
        document.set_actor(self.peer.workspace_actor_of(key));
        let before = document.get_heads();
        match crate::governance::admit_invited_member(&mut document, cached, redemption, name, now)?
        {
            crate::governance::AdmissionOutcome::AlreadyApplied => Ok(false),
            crate::governance::AdmissionOutcome::Applied { .. } => {
                // The workspace's owner is not one of its members: they hold
                // it from its beginning, and their device's certificate is
                // what admits it.
                let admit = (cert.user_public_key != self.peer.core.owner).then(|| {
                    crate::members::MembershipOpCommand::Admit {
                        invite: redemption.join().invite_id(),
                        person: cert.user_public_key,
                    }
                });
                // A person joining names the change for whoever reads the workspace's
                // members: they joined, at the invitation of whoever sent it.
                let joined = admit.as_ref().map(|_| joined(redemption));
                self.peer.naming(joined, |peer| {
                    peer.write_membership(document.into(), &before, key, now, admit)
                })
            }
        }
    }
    /// Admit `key`'s device through the workspace's entry: it names the entry
    /// `entry` it came in under, and the entry's open role is the role its
    /// person takes. No invitation is behind it and no host signs it; the
    /// device writes its own row and its own membership change, and the
    /// entry the workspace records now is what admits it. Whether the
    /// workspace changed.
    ///
    /// A workspace whose entry closed, turned invitation-only, or opened at
    /// another role since the device looked refuses it, and so does one that
    /// banned the person; replay judges an admission already in the history
    /// by the entry its own place in the log held, not by the entry now.
    ///
    /// # Errors
    ///
    /// The device is not current or is revoked, the entry is not the
    /// workspace's now or takes nobody, the person is banned, an agent tries
    /// to come in by the entry, or governance cannot be updated.
    pub fn admit_entered_member(
        self,
        own: &DeviceCert,
        entry: [u8; 32],
        request_id: [u8; 32],
        name: Option<&str>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let name = name
            .map(|name| {
                tilo_user_identity::display_name(name)
                    .map_err(tilo_user_identity::DisplayNameError::reason)
            })
            .transpose()?;
        if key.device_public_key() != own.device_public_key {
            return Err("a device signs only its own admission".into());
        }
        let cert = own.clone();
        if cert.capabilities.agent.is_some() || cert.capabilities.stand_in {
            return Err(
                "an agent's key comes into a workspace by an invitation, within its certificate's scope, not through the entry".into(),
            );
        }
        if self.peer.core.revocations.refuses(&cert) {
            return Err("device revoked".into());
        }
        let cached = self
            .peer
            .core
            .workspace_doc
            .as_ref()
            .ok_or("workspace governance missing")?;
        let mut document = (**cached).clone();
        document.set_actor(self.peer.workspace_actor_of(key));
        let before = document.get_heads();
        match crate::governance::admit_entered_member(
            &mut document,
            cached,
            &cert,
            entry,
            request_id,
            name,
            now,
        )? {
            crate::governance::AdmissionOutcome::AlreadyApplied => Ok(false),
            crate::governance::AdmissionOutcome::Applied { .. } => {
                // The workspace's owner is not one of its members: they hold
                // it from its beginning, and their device's certificate is
                // what admits it.
                let enter = (cert.user_public_key != self.peer.core.owner).then_some(
                    crate::members::MembershipOpCommand::Enter {
                        entry,
                        person: cert.user_public_key,
                    },
                );
                // A person joining names the change for whoever reads the
                // workspace's members: they came in under its entry.
                let joined = enter.as_ref().map(|_| joined_entry(&cert, request_id));
                self.peer.naming(joined, |peer| {
                    peer.write_membership(document.into(), &before, key, now, enter)
                })
            }
        }
    }
    /// Admit `device`, a device certified by the root of someone already a
    /// member, as that member's: it needs no invitation, and `key` is that
    /// very device, writing its own row. No device writes another's row.
    /// Whether the workspace changed.
    ///
    /// A device the workspace already holds is admitted again under the same
    /// certificate, and a later certificate of it replaces the held one
    /// ([`PeerGovernanceMut::renew_member_certificate`]): a device is its key,
    /// whatever certificate of its person's root it comes back with. A device
    /// the workspace took off its member list stays off it: only an
    /// invitation it redeems brings it back
    /// ([`PeerGovernance::removed_from_member_list`]).
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not `device`'s own, `device` does not
    /// verify, is revoked, was taken off the member list or names another
    /// device than the one the workspace holds, its person is not a member,
    /// or governance cannot be updated.
    pub fn admit_member_device(
        self,
        device: &DeviceCert,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        tilo_user_identity::verify_device_cert_at(device, now_ns(now)).map_err(err)?;
        if self.peer.core.revocations.refuses(device) {
            return Err("device revoked".into());
        }
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let known = hex::encode(device.device_public_key);
        let held = doc
            .identities()
            .get(&known)
            .and_then(|row| row.as_ref().ok().cloned());
        if held.as_ref().is_some_and(|row| row.removed) {
            return Err("the workspace took this device off its member list".into());
        }
        // What the workspace's own record of the device decides, before the
        // fresh row an admission of a device it never met writes.
        let decided = match self.peer.core.certificates.get(&device.device_public_key) {
            Some(old) if old.as_ref() == device => match held {
                // The member list holds the device already: nothing changes.
                Some(_) => Decision::Held,
                None => Decision::Fresh,
            },
            Some(old) => {
                if !super::supersedes(old.as_ref(), device) {
                    return Err("certificate conflict".into());
                }
                match held {
                    // A later certificate of the same device replaces the one
                    // the workspace keeps, and keeps the rest of that device's
                    // row.
                    Some(_) => Decision::Renewed,
                    None => Decision::Fresh,
                }
            }
            None => Decision::Fresh,
        };
        match decided {
            Decision::Held => return Ok(false),
            Decision::Renewed => return self.renew_member_certificate(device, key, now),
            Decision::Fresh => {}
        }
        let person = hex::encode(device.user_public_key);
        let owner = self.peer.core.owner == device.user_public_key;
        if !owner
            && doc
                .workspace_policy(&self.peer.core.workspace)
                .active_role(&person)
                .is_none()
        {
            return Err("the device's person is not a member".into());
        }
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("member identities missing")?
            .1;
        let identity = DeviceRow {
            device_cert: device.clone(),
            revocations: Vec::new(),
            admission: None,
            admissions: Vec::new(),
            delegate: None,
            workspace_delegations: Vec::new(),
            removed: false,
            entry: None,
        };
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        doc.put(
            &identities,
            hex::encode(device.device_public_key),
            automerge::ScalarValue::Bytes(identity.encode().map_err(err)?),
        )
        .map_err(err)?;
        self.peer.write_governance(doc, &before, key, now)
    }
    /// Take `device`, which the workspace removed, back onto its member list, as `key`'s device:
    /// the row the removal marked loses its mark and keeps the admission, delegation and
    /// revocations it carries; a row the removal took out is written again bare, under
    /// `device`'s own certificate. `root` is the root of `key`'s person marking `key`'s device as
    /// the one keeping it ([`tilo_user_identity::root_holder`]), which the change carries for
    /// every receiver to check: only a device that keeps its person's root takes a removed device
    /// back, and a removed device never writes itself back in. Whether the member list changed:
    /// a device it lists already changes nothing.
    ///
    /// # Errors
    ///
    /// `device` does not verify, or governance cannot be updated; receivers, and this copy taking
    /// the change in as they do, refuse it where `root` does not mark `key`'s device, its person
    /// may not add members, or `device`'s person is no member.
    pub fn readmit_device(
        self,
        device: &DeviceCert,
        root: [u8; 64],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        tilo_user_identity::verify_device_cert_at(device, now_ns(now)).map_err(err)?;
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let id = hex::encode(device.device_public_key);
        let row = match doc
            .identities()
            .get(&id)
            .and_then(|row| row.as_ref().ok().cloned())
        {
            Some(row) if !row.removed => return Ok(false),
            Some(mut row) => {
                row.removed = false;
                row
            }
            None => DeviceRow {
                device_cert: device.clone(),
                revocations: Vec::new(),
                admission: None,
                admissions: Vec::new(),
                delegate: None,
                workspace_delegations: Vec::new(),
                removed: false,
                entry: None,
            },
        };
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("the workspace's identities are missing")?
            .1;
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        doc.put(
            &identities,
            id,
            automerge::ScalarValue::Bytes(row.encode().map_err(err)?),
        )
        .map_err(err)?;
        self.peer.write_membership(
            doc,
            &before,
            key,
            now,
            Some(crate::members::MembershipOpCommand::Readmit {
                device: device.device_public_key,
                root,
            }),
        )?;
        Ok(true)
    }

    /// Take out of the workspace's governance every device row whose
    /// certificate has expired: a device whose certificate no longer verifies
    /// is no member device any more ([`Self::member_list_access`] leaves it out),
    /// so the row it left keeps nobody reading. A page's device certificate
    /// lasts an hour and a page opens a fresh one on every load, so this is
    /// what keeps a workspace that many people visit from keeping a row per
    /// visit. Rows that record an admission, a delegation or a revocation
    /// stay: a person's member devices are the workspace's for as long as the
    /// person is. Whether the workspace changed.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make, a row does not
    /// read back, or governance cannot be updated.
    pub fn forget_expired_devices(
        self,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return Ok(false);
        };
        // The rows as read once per governance state: a settle after every
        // change looks for expired rows, and decoding every member's row each
        // time made each change cost the workspace.
        let expired: Vec<[u8; 32]> = doc
            .identities()
            .values()
            .map(|row| row.as_ref().map_err(err))
            .collect::<Result<Vec<_>, String>>()?
            .into_iter()
            .filter(|row| row.revocations.is_empty())
            .filter(|row| row.admission.is_none() && row.admissions.is_empty())
            .filter(|row| row.delegate.is_none())
            .filter(|row| {
                row.device_cert
                    .expires_at_ns
                    .is_some_and(|expiry| expiry <= now_ns(now))
            })
            .map(|row| row.device_cert.device_public_key)
            .collect();
        if expired.is_empty() {
            return Ok(false);
        }
        self.forget_identity_rows(&expired, key, now)
    }

    /// Take `device`'s row out of the workspace's governance before its
    /// certificate expires, as [`Self::forget_expired_devices`] does after:
    /// what the workspace does with a device that stopped being one of its
    /// own, so no member's device admits it any longer. A row that records an
    /// admission, a delegation or a revocation is kept, for the history that
    /// names it and for verification, and marked removed
    /// ([`PeerGovernance::removed_from_member_list`]). Whether the workspace
    /// changed.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make, the row
    /// does not read back, or governance cannot be updated.
    pub fn forget_device(
        self,
        device: [u8; 32],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let bare = {
            let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
                return Ok(false);
            };
            let identities = doc.identities();
            let Some(row) = identities.get(&hex::encode(device)) else {
                return Ok(false);
            };
            let row = row.as_ref().map_err(err)?;
            row.revocations.is_empty()
                && row.admission.is_none()
                && row.admissions.is_empty()
                && row.delegate.is_none()
        };
        if bare {
            return self.forget_identity_rows(&[device], key, now);
        }
        self.mark_identity_removed(device, key, now)
    }

    /// Mark `device`'s row removed, keeping the admission, delegation and
    /// revocation it carries for the history that names them. Whether the
    /// workspace changed.
    fn mark_identity_removed(
        self,
        device: [u8; 32],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let id = hex::encode(device);
        let bytes = workspace::governance_of(Some(&doc))
            .identities
            .get(&id)
            .cloned()
            .ok_or("member identity missing")?;
        let mut row = DeviceRow::decode(&bytes).map_err(err)?;
        if row.removed {
            return Ok(false);
        }
        row.removed = true;
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("member identities missing")?
            .1;
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        doc.put(
            &identities,
            &id,
            automerge::ScalarValue::Bytes(row.encode().map_err(err)?),
        )
        .map_err(err)?;
        self.peer.write_governance(doc, &before, key, now)
    }

    /// Take `devices`' rows out of the workspace's governance signed by
    /// `key`, in one change, and take it in as every member will. Whether the
    /// workspace changed.
    fn forget_identity_rows(
        self,
        devices: &[[u8; 32]],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let identities = doc
            .get(ROOT, "peer_identities_v1")
            .map_err(err)?
            .ok_or("member identities missing")?
            .1;
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        for device in devices {
            doc.delete(&identities, hex::encode(device)).map_err(err)?;
        }
        self.peer.write_governance(doc, &before, key, now)
    }

    /// Change a member's workspace role.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make, the user is the owner or
    /// absent from the admission history, or governance cannot be updated.
    pub fn set_member_role(
        self,
        user_public_key: [u8; 32],
        role: crate::join::Role,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.set_member(user_public_key, role, false, key, now)
    }
    /// Hold the person `user_public_key` as a member of the workspace at
    /// `role`, an administrator or not, before any device of theirs is
    /// admitted here: someone a deployment's operator names as an owner while
    /// the workspace waits for them to join. Their device is admitted when it
    /// joins, and they hold what this record says from then on. `name`, when
    /// the caller carries one, is how the workspace names them from then on
    /// ([`Self::name_person`]).
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make, the
    /// person is the workspace's own owner, `name` is not a display name, or
    /// governance cannot be updated.
    pub fn declare_member(
        self,
        user_public_key: [u8; 32],
        role: crate::join::Role,
        administrator: bool,
        name: Option<&str>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer
            .declare_member(user_public_key, role, administrator, name, key, now)
    }

    /// Make a person an administrator of the workspace: a member as an Editor,
    /// holding every governance action
    /// ([`tilo_action_policy::roles::administrator_grant`]), whether one of
    /// their devices has joined yet or not. A declaration made before they
    /// arrive is governance like any other, so it is here when they do and
    /// after a restart. `name`, when the caller carries one, is how the
    /// workspace names them until they rename themselves.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make, the
    /// person is the workspace's own owner, `name` is not a display name, or
    /// governance cannot be updated.
    pub fn set_member_administrator(
        self,
        user_public_key: [u8; 32],
        name: Option<&str>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.declare_member(
            user_public_key,
            crate::join::Role::Editor,
            true,
            name,
            key,
            now,
        )
    }
    /// Hand the workspace to the person `user_public_key`, as its owner's
    /// device `key`: they become its owner from this change on, on every
    /// device that replays it, and the owner this replaces stays as one of
    /// its administrators ([`tilo_action_policy::Change::TransferOwnership`]).
    /// Whether the workspace changed: handing it to its owner changes nothing.
    ///
    /// # Errors
    ///
    /// Returns an error if `key`'s device is not the owner's, the person is
    /// banned, or governance cannot be updated.
    pub fn transfer_ownership(
        self,
        user_public_key: [u8; 32],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        if user_public_key == self.peer.core.owner {
            return Ok(false);
        }
        self.peer
            .owner_device(&key.device_public_key(), now)
            .map_err(|_| {
                "only the workspace's owner hands the workspace to someone else".to_owned()
            })?;
        let doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let stamp = self.peer.own_action(
            tilo_action_policy::roles::governance::TRANSFER_OWNERSHIP,
            key,
            now,
        );
        self.peer.naming(stamp, |peer| {
            peer.write_changes(
                doc,
                vec![Change::TransferOwnership {
                    person: hex::encode(user_public_key),
                }],
                key,
                now,
            )
        })
    }

    /// Hold the person `user_public_key` in the workspace at `place`, as
    /// `key`'s device: a role or one of its administrators, whether or not a
    /// device of theirs has joined ([`Self::declare_member`]), or its owner,
    /// whom the workspace is handed to ([`Self::transfer_ownership`]). `name`,
    /// when given, is how the workspace names them until they name themselves;
    /// a new owner is no member beside it, and a name they bring is still theirs.
    /// The same change whichever device writes it, the host's or a member's own,
    /// and every device that replays it decides it by its writer's grants
    /// ([`crate::workspace_membership::WorkspaceMembershipCommand::Place`]).
    ///
    /// # Errors
    ///
    /// The change is not `key`'s device's to make, the person is the workspace's
    /// own owner and the place a role, `name` is not a display name, or
    /// governance cannot be updated.
    pub fn place_member(
        self,
        user_public_key: [u8; 32],
        place: crate::workspace_membership::MemberPlace,
        name: Option<&str>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        use crate::workspace_membership::MemberPlace;
        let (role, administrator) = match place {
            MemberPlace::Owner => {
                let handed = PeerGovernanceMut {
                    peer: &mut *self.peer,
                }
                .transfer_ownership(user_public_key, key, now)?;
                let named = match name {
                    Some(name) => self.name_person(user_public_key, name, key, now)?,
                    None => false,
                };
                return Ok(handed || named);
            }
            MemberPlace::Administrator => (crate::join::Role::Editor, true),
            MemberPlace::Role(role) => (role, false),
        };
        self.declare_member(user_public_key, role, administrator, name, key, now)
    }

    /// Name the person `user_public_key` `name` in the workspace: the name
    /// every reader of [`PeerGovernance::member_list`] shows for them.
    /// The name is kept under the person's root, where the workspace holds
    /// them whether or not a device of theirs has been admitted yet — someone
    /// its operator declared an owner names themselves from a key before any
    /// device of theirs arrives — and on each of their admitted devices' rows,
    /// which everything else listing people reads. A device's own label stays
    /// on its certificate. Whether the workspace changed: naming someone what
    /// they are already called writes nothing.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make, `name` is not
    /// a display name ([`tilo_user_identity::display_name`]), the person is no
    /// member of the workspace, or governance cannot be updated.
    pub fn name_person(
        self,
        user_public_key: [u8; 32],
        name: &str,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let name = tilo_user_identity::display_name(name)
            .map_err(tilo_user_identity::DisplayNameError::reason)?;
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let root = hex::encode(user_public_key);
        let mut rows = Vec::new();
        {
            let names = &doc.policy().names;
            for (id, identity) in doc.identities() {
                let identity = identity.as_ref().map_err(err)?;
                if identity.device_cert.user_public_key == user_public_key
                    && identity.has_admission()
                    && names.get(id).map(String::as_str) != Some(name)
                {
                    rows.push(id.clone());
                }
            }
            // A name under the root is the person's own, and the workspace
            // keeps it for whoever holds a role here: the owner, or a member.
            // Anyone else's would name nobody it holds.
            let holds_a_role = doc
                .workspace_policy(&self.peer.core.workspace)
                .active_role(&root)
                .is_some();
            if rows.is_empty() && !holds_a_role {
                return Err("the person is not a member of the workspace".into());
            }
            if holds_a_role && names.get(&root).map(String::as_str) != Some(name) {
                rows.push(root);
            }
        }
        if rows.is_empty() {
            return Ok(false);
        }
        let peers = doc
            .get(ROOT, "peers")
            .map_err(err)?
            .ok_or("peer names missing")?
            .1;
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        for id in &rows {
            doc.put(&peers, id, name).map_err(err)?;
        }
        self.peer.write_governance(doc, &before, key, now)
    }
    /// Drop each name the workspace keeps for nobody it holds: a key that is no
    /// admitted device's, not the owner's and no member's. A removal leaves
    /// none ([`Self::remove_member`]), but a person naming themselves while
    /// they are removed, on another device, merges into one; such a name makes
    /// the workspace's security basis unreadable, and every device's next
    /// write with it. A device that publishes governance drops them first.
    /// Whether the workspace changed.
    ///
    /// # Errors
    ///
    /// Returns an error if a name needs dropping and `key`'s device may not
    /// change governance, or governance cannot be updated.
    pub fn drop_names_of_nobody(
        self,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let Some(held) = self.peer.core.workspace_doc.as_ref() else {
            return Ok(false);
        };
        let owner = hex::encode(self.peer.core.owner);
        let declared = crate::members::replay_declared(held);
        let identities = held.identities();
        let nobody: Vec<String> = held
            .policy()
            .names
            .keys()
            .filter(|named| {
                **named != owner
                    && declared.owner.as_ref() != Some(*named)
                    && !identities.contains_key(*named)
                    && !declared.members.contains_key(*named)
            })
            .cloned()
            .collect();
        if nobody.is_empty() {
            return Ok(false);
        }
        let mut doc = held.clone();
        let peers = workspace_map(&doc, "peers")?;
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        for named in &nobody {
            doc.delete(&peers, named.as_str()).map_err(err)?;
        }
        self.peer.write_governance(doc, &before, key, now)
    }
    /// Rewrite each administrator's grant still in the shape workspaces held
    /// before [`tilo_action_policy::roles::administrator_grant`] (on the
    /// workspace's resource, where the Editor's own Deny outranked it) to that
    /// grant, keeping its id and giver. A device runs it before it publishes
    /// governance, so a workspace's next governance change carries its
    /// administrators' authority. Whether the workspace changed.
    ///
    /// # Errors
    ///
    /// Returns an error if a grant needs rewriting and `key`'s device may not
    /// change it, or governance cannot be updated.
    pub fn reconcile_administrator_grants(
        self,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let Some(doc) = self.peer.core.workspace_doc.as_ref() else {
            return Ok(false);
        };
        let workspace = self.peer.core.workspace.as_str();
        let grants = &doc.workspace_policy(workspace).grants;
        let mut rewritten = grants
            .iter()
            .filter(|grant| {
                tilo_action_policy::roles::is_legacy_administrator_grant(workspace, grant)
            })
            .filter_map(|grant| match &grant.grantee {
                Grantee::Member(tilo_action_policy::MemberId::Person(person)) => {
                    Some(tilo_action_policy::roles::administrator_grant(
                        workspace,
                        person.as_str(),
                        grant.granted_by.clone(),
                    ))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        // Every administrator moderates the workspace too.
        for grant in grants {
            if let Some(person) = grant.grantee.person_id()
                && grant.id == tilo_action_policy::roles::administrator_grant_id(person)
                && grant.answer == tilo_action_policy::Answer::Allow
            {
                let moderation_id =
                    tilo_action_policy::roles::administrator_moderation_grant_id(person);
                if !grants.iter().any(|held| held.id == moderation_id) {
                    rewritten.push(tilo_action_policy::roles::administrator_moderation_grant(
                        workspace,
                        person,
                        grant.granted_by.clone(),
                    ));
                }
            }
        }
        if rewritten.is_empty() {
            return Ok(false);
        }
        self.peer
            .governance_mut()
            .put_member_grants(&rewritten, key, now)
    }
    /// Remove grants a member's device decided to take back, by id.
    /// Memberships are removed through member governance.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make or governance
    /// cannot be updated.
    pub fn remove_member_grants(
        self,
        ids: &[GrantId],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_member_writer(key, now)?;
        let doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let held = &doc.workspace_policy(&self.peer.core.workspace).grants;
        let changes: Vec<Change> = ids
            .iter()
            .filter(|id| held.iter().any(|grant| grant.id == **id))
            .map(|id| Change::Revoke(id.clone()))
            .collect();
        self.peer.write_changes(doc, changes, key, now)
    }
    /// Write grants a member's device decided, such as those an assigned
    /// grant set expands to, keyed by their ids.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make, a grant has no
    /// id or names another workspace, or governance cannot be updated.
    pub fn put_member_grants(
        self,
        grants: &[Grant],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_member_writer(key, now)?;
        let doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let held = &doc.workspace_policy(&self.peer.core.workspace).grants;
        let mut changes = Vec::new();
        for grant in grants {
            if grant.id.as_str().trim().is_empty()
                || grant.node.workspace_id() != Some(self.peer.core.workspace.as_str())
            {
                return Err(format!(
                    "grant {:?} cannot be written as workspace policy",
                    grant.id
                ));
            }
            match held.iter().find(|kept| kept.id == grant.id) {
                Some(kept) if kept == grant => continue,
                // A grant kept under the id it had with different terms is one
                // revoked and given again.
                Some(_) => changes.push(Change::Revoke(grant.id.clone())),
                None => {}
            }
            changes.push(Change::Grant(grant.clone()));
        }
        self.peer.write_changes(doc, changes, key, now)
    }
    /// Remove a member and their grants; someone removing
    /// themselves also takes their own devices' rows.
    ///
    /// # Errors
    ///
    /// Returns an error if the change is not `key`'s device's to make, the user is the structural
    /// owner or not a member, or governance cannot be updated.
    pub fn remove_member(
        self,
        user_public_key: [u8; 32],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.remove_members(&[user_public_key], key, now)
    }
    /// Remove every one of `people` and their grants as one change of the
    /// member list, as [`Self::remove_member`] removes one.
    ///
    /// # Errors
    ///
    /// As [`Self::remove_member`], for any of them.
    pub fn remove_members(
        self,
        people: &[[u8; 32]],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.peer.require_member_writer(key, now)?;
        if people.contains(&self.peer.core.owner) {
            return Err("workspace owner cannot be removed".into());
        }
        let mut doc = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let policy = doc.workspace_policy(&self.peer.core.workspace);
        let signer = self
            .peer
            .certificate(&key.device_public_key(), now)
            .ok()
            .map(|cert| cert.user_public_key);
        let mut changes = Vec::new();
        let mut gone = Vec::new();
        for user_public_key in people {
            let person = hex::encode(user_public_key);
            // Removing a member takes every grant naming them with them; someone
            // who is no member loses the grants that still name them.
            if policy.members.contains_key(&person) {
                changes.push(Change::RemoveMember {
                    person: person.clone(),
                });
            } else {
                changes.extend(
                    policy
                        .grants
                        .iter()
                        .filter(|grant| grant.grantee.person_id() == Some(person.as_str()))
                        .map(|grant| Change::Revoke(grant.id.clone())),
                );
            }
            gone.push(person);
        }
        if changes.is_empty() {
            return Ok(false);
        }
        let before = doc.get_heads();
        doc.set_actor(self.peer.workspace_actor_of(key));
        // Someone who leaves takes their own devices off the workspace with
        // them: the rows that admit a device go, so no key of theirs stays
        // named here. Another person's devices stay until whoever may remove
        // them takes them.
        if let Some(leaving) = signer.filter(|signer| people.contains(signer)) {
            let identities = workspace_map(&doc, "peer_identities_v1")?;
            let own: Vec<String> = crate::workspace::governance_of(Some(&doc))
                .identities
                .into_iter()
                .filter(|(_, bytes)| {
                    super::DeviceRow::decode(bytes)
                        .is_ok_and(|identity| identity.device_cert.user_public_key == leaving)
                })
                .map(|(device, _)| device)
                .collect();
            for device in own {
                doc.delete(&identities, device.as_str()).map_err(err)?;
                gone.push(device);
            }
        }
        // The names kept for whom the workspace no longer holds go with them: the
        // person's own, under their root, and their devices' whose rows went. A
        // name for nobody the workspace holds makes its security basis
        // unreadable, and every device's next write with it.
        let peers = workspace_map(&doc, "peers")?;
        for named in gone {
            if doc.get(&peers, named.as_str()).map_err(err)?.is_some() {
                doc.delete(&peers, named.as_str()).map_err(err)?;
            }
        }
        let stamp = self.peer.own_action(
            tilo_action_policy::roles::governance::REMOVE_MEMBER,
            key,
            now,
        );
        self.peer.naming(stamp, |peer| {
            peer.write_membership(
                doc,
                &before,
                key,
                now,
                Some(crate::members::MembershipOpCommand::Project { changes }),
            )
        })
    }
}

/// The action an entry admission is, for whoever reads the workspace's
/// members: the person came in under the workspace's entry, which is the only
/// thing that proposed them. Its own request names it.
fn joined_entry(cert: &DeviceCert, request_id: [u8; 32]) -> super::ActionStamp {
    let person = |root: [u8; 32]| {
        tilo_action_policy::Actor::Member(tilo_action_policy::MemberId::person(hex::encode(root)))
    };
    // The entry proposed them, and it names no person: the stamp suggests
    // nobody.
    super::ActionStamp::of_invocation(
        tilo_action_policy::roles::governance::JOIN,
        person(cert.user_public_key),
        hex::encode(request_id),
    )
}

/// The action a redemption of an invitation is, for whoever reads the
/// workspace's members: the redeeming person joined, proposed by whoever sent
/// the invitation (its issuer's person, or the person a host signed it for).
/// The redemption's own request names it, so a write made again for its
/// signatures names the same one.
fn joined(redemption: &crate::join::VerifiedRedemption) -> super::ActionStamp {
    let person = |root: [u8; 32]| {
        tilo_action_policy::Actor::Member(tilo_action_policy::MemberId::person(hex::encode(root)))
    };
    let join = redemption.join();
    let inviter = join.inviter().map_or_else(
        || join.grant().issuer.user_public_key,
        |inviter| inviter.user_public_key,
    );
    let mut stamp = super::ActionStamp::of_invocation(
        tilo_action_policy::roles::governance::JOIN,
        person(redemption.certificate().user_public_key),
        hex::encode(redemption.request_id()),
    );
    stamp.suggested_by = Some(person(inviter));
    stamp
}

impl super::StatePeer {
    /// The current member writing with `key`, as its own device's authority
    /// holds it: what inviting is decided by.
    fn inviter(&self, key: &dyn DeviceSigner, now: SystemTime) -> Result<MemberAccess, String> {
        let device = key.device_public_key();
        self.require_member_writer(key, now)?;
        self.membership(device, now)
    }

    /// A current member's device whose person may invite: what an
    /// admission through an invitation is written by.
    fn require_inviter(&self, key: &dyn DeviceSigner, now: SystemTime) -> Result<(), String> {
        let member = self.inviter(key, now)?;
        member
            .policy
            .check_action(
                &member.authority(),
                tilo_action_policy::roles::governance::ADD_MEMBER,
                &tilo_action_policy::Context::at_time(now),
                false,
            )
            .map_err(|refusal| format!("the writer may not invite: {refusal}"))
    }

    /// A current member's device whose person may invite, and may hand out
    /// `role` with what it makes: checked where an invitation is written, so
    /// no surface issues a link its person could not redeem itself.
    pub(crate) fn require_invitation_authority(
        &self,
        key: &dyn DeviceSigner,
        role: Role,
        now: SystemTime,
    ) -> Result<(), String> {
        let member = self.inviter(key, now)?;
        member
            .policy
            .check_admission(
                &member.authority(),
                role,
                &tilo_action_policy::Context::at_time(now),
                false,
            )
            .map_err(|refusal| format!("the writer may not invite: {refusal}"))
    }

    /// Whether `key`'s device may invite someone as an administrator at
    /// `now` ([`tilo_action_policy::WorkspacePolicy::check_administrator_invite`]).
    pub(crate) fn require_administrator_invitation(
        &self,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<(), String> {
        let member = self.inviter(key, now)?;
        member
            .policy
            .check_administrator_invite(
                person_of(&member)?,
                &member.authority(),
                &tilo_action_policy::Context::at_time(now),
                false,
            )
            .map_err(|refusal| format!("the writer may not invite an administrator: {refusal}"))
    }
}
