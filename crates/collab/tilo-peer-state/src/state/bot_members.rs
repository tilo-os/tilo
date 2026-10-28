//! Installed bots as governed members of a room.
//!
//! Installing writes three things in one governed change: the bot device's
//! identity carrying its installation, the bot's grants (its program's rules
//! held to this room, plus the deny on governance), and the program bytes.
//! Every member then reads what runs and enforces the grants with the code
//! it already has. Updates, renewals and every later bot command are
//! governed changes too; none of them is host-local state.

use super::facades::PeerGovernance;
use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
use tilo_action_policy::{
    Context, Grant, GrantId, RoomPolicy, bot,
    roles::{self, governance},
};
use tilo_automation::program::{parse_program, program_hash};
use tilo_user_identity::DeviceCert;

use super::delegates::{GovernedDelegateRow, holds_no_power};
use super::{GovernedIdentity, SigningKey, StatePeer, SystemTime, err, governed_map, now_ns};
use crate::installation::Installation;
use crate::workspace_membership::{BotDevice, BotInstall, BotRenewal};

/// The governed map that keeps bot programs, by BLAKE3 hex.
const PROGRAMS: &str = "bot_programs_v1";
/// The governed map that keeps the bots the room asks its host to run, by
/// directory entry.
const REQUESTS: &str = "bot_requests_v1";

/// A bot the room asks its host to run, as the room keeps the request.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RequestedBot {
    /// The directory entry and version asked for.
    pub request: crate::workspace_membership::BotRequest,
    /// The person who asked.
    pub requested_by: String,
    /// When they asked, ns since the epoch.
    pub requested_at_ns: u64,
}

/// One installed bot on a room's roster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledBot {
    /// Its governed row.
    pub installation: Installation,
    /// The bot device's key, which signs everything it writes.
    pub device_public_key: [u8; 32],
    /// When its delegation ends, ns since the epoch.
    pub delegation_expires_at_ns: u64,
    /// When this device row was withdrawn: uninstalled, or moved to a new
    /// key.
    pub withdrawn_at_ns: Option<u64>,
    /// Whether it acts in the room at the time the roster was read.
    pub acting: bool,
}

/// Everything one governed bot change writes, committed together.
#[derive(Default)]
pub(super) struct BotWrite {
    pub(super) identities: Vec<GovernedIdentity>,
    pub(super) revoke: Vec<GrantId>,
    pub(super) grant: Vec<Grant>,
    pub(super) program: Option<(String, String)>,
    pub(super) drop_program: Option<String>,
}

impl StatePeer {
    pub(super) fn bot_identities(&self) -> Result<Vec<GovernedIdentity>, String> {
        let doc = self
            .core
            .room_doc
            .as_ref()
            .ok_or("room governance missing")?;
        let mut out = Vec::new();
        for bytes in doc.policy().identities.values() {
            let identity: GovernedIdentity = serde_json::from_slice(bytes).map_err(err)?;
            if identity
                .delegate
                .as_ref()
                .is_some_and(|row| row.installation.is_some())
            {
                out.push(identity);
            }
        }
        Ok(out)
    }

    /// The identity currently carrying installation `id`: the one not
    /// withdrawn. `Err` when only withdrawn ones exist.
    pub(super) fn live_bot(&self, id: &str) -> Result<Option<GovernedIdentity>, String> {
        let rows: Vec<GovernedIdentity> = self
            .bot_identities()?
            .into_iter()
            .filter(|identity| {
                identity
                    .delegate
                    .as_ref()
                    .and_then(|row| row.installation.as_ref())
                    .is_some_and(|installation| installation.id.as_str() == id)
            })
            .collect();
        if rows.is_empty() {
            return Ok(None);
        }
        match rows.into_iter().find(|identity| {
            identity
                .delegate
                .as_ref()
                .is_some_and(|row| row.withdrawn_at_ns.is_none())
        }) {
            Some(live) => Ok(Some(live)),
            None => Err("the bot was uninstalled".into()),
        }
    }

    /// The grants installation `id` holds in the room now.
    pub(super) fn bot_grants(&self, id: &str, now: SystemTime) -> Result<Vec<Grant>, String> {
        let _ = now;
        let policy = self
            .core
            .room_doc
            .as_ref()
            .ok_or("room governance missing")?
            .policy()
            .room_policy(&self.core.room);
        let prefix = bot::grant_prefix(id);
        Ok(policy
            .grants
            .into_iter()
            .filter(|grant| grant.id.as_str().starts_with(&prefix))
            .collect())
    }

    /// Check a bot device for installation `id` of `installer` (hex root).
    pub(super) fn check_bot_device(
        &self,
        device: &BotDevice,
        id: &str,
        installer: &str,
        now: SystemTime,
    ) -> Result<GovernedDelegateRow, String> {
        let BotDevice { cert, delegation } = device;
        if hex::encode(cert.user_public_key) != installer {
            return Err("the bot's certificate is not its installer's".into());
        }
        if !holds_no_power(cert) {
            return Err("a bot's certificate holds no capability".into());
        }
        tilo_user_identity::verify_device_cert_at(cert, now_ns(now)).map_err(err)?;
        if self.core.revocations.is_revoked(&cert.device_public_key) {
            return Err("device revoked".into());
        }
        if delegation.delegate.installation() != Some(id) {
            return Err("the delegation names another installation".into());
        }
        Ok(GovernedDelegateRow {
            delegation: delegation.clone(),
            withdrawn_at_ns: None,
            installation: None,
        })
    }

    pub(super) fn apply_install_bot(
        &mut self,
        caller: &DeviceCert,
        install: &BotInstall,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let member = self.permissions(caller.device_public_key, now)?;
        let cx = Context::at_time(now);
        let bound = roles::foreign_device_bound();
        let id = install.installation.id.as_str().to_owned();
        let room = self.core.room.clone();
        let grants = program_grants(&room, install, &member.policy, &cx)?;

        let live = self.live_bot(&id)?;
        let mut identity = match (&live, &install.device) {
            (None, Some(device)) => {
                if install.installation.installed_by != member.person {
                    return Err("a bot is installed by the person installing it".into());
                }
                let authority = member.authority(&bound);
                member
                    .policy
                    .check_action(&authority, governance::INSTALL_BOT, &cx, false)
                    .map_err(|refusal| refusal.to_string())?;
                let mut row = self.check_bot_device(device, &id, &member.person, now)?;
                row.installation = Some(install.installation.clone());
                row.acting(&device.cert, &room, now_ns(now))
                    .map_err(|reason| reason.to_string())?;
                if self
                    .core
                    .certificates
                    .contains_key(&device.cert.device_public_key)
                {
                    return Err("the bot's device is already in this room".into());
                }
                GovernedIdentity {
                    device_cert: device.cert.clone(),
                    revocations: vec![],
                    admission: None,
                    admissions: vec![],
                    workspace_delegations: vec![],
                    delegate: Some(row),
                }
            }
            (None, None) => return Err("a new bot needs its device and delegation".into()),
            (Some(_), Some(_)) => {
                return Err("an update keeps the bot's device; renew it to move it".into());
            }
            (Some(live), None) => {
                let row = live.delegate.as_ref().ok_or("bot row missing")?;
                let old = row.installation.as_ref().ok_or("bot row missing")?;
                if old.installed_by != install.installation.installed_by {
                    return Err("an update keeps the installer; adopt the bot to change it".into());
                }
                let old_grants = self.bot_grants(&id, now)?;
                let action = if bot::fits_within(&old_grants, &grants) {
                    governance::PAUSE_BOT
                } else {
                    governance::INSTALL_BOT
                };
                let authority = member.authority(&bound);
                member
                    .policy
                    .check_action(&authority, action, &cx, false)
                    .map_err(|refusal| refusal.to_string())?;
                let mut updated = live.clone();
                let mut next = install.installation.clone();
                next.paused = old.paused;
                if next.armed_at.is_empty() {
                    next.armed_at.clone_from(&old.armed_at);
                }
                next.connector_cursors_from_ms = old.connector_cursors_from_ms;
                if &next == old && old_grants == grants {
                    return Ok(false);
                }
                if let Some(row) = updated.delegate.as_mut() {
                    row.installation = Some(next);
                }
                updated
            }
        };

        let old_program = live
            .as_ref()
            .and_then(|identity| identity.delegate.as_ref())
            .and_then(|row| row.installation.as_ref())
            .map(|installation| installation.program.clone())
            .filter(|old| old != &install.installation.program);
        let revoke = self
            .bot_grants(&id, now)?
            .into_iter()
            .map(|grant| grant.id)
            .collect();
        identity.revocations.clear();
        self.put_bot_change(
            &BotWrite {
                identities: vec![identity],
                revoke,
                grant: grants,
                program: Some((
                    install.installation.program.clone(),
                    install.program.clone(),
                )),
                drop_program: old_program,
            },
            key,
            now,
        )
    }

    /// Renew or move an installed bot's delegation, as its installer.
    pub(super) fn apply_renew_bot(
        &mut self,
        caller: &DeviceCert,
        renewal: &BotRenewal,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let member = self.permissions(caller.device_public_key, now)?;
        let id = renewal.installation.as_str();
        let live = self.live_bot(id)?.ok_or("no such bot")?;
        let row = live.delegate.clone().ok_or("bot row missing")?;
        let installation = row.installation.clone().ok_or("bot row missing")?;
        if installation.installed_by != member.person {
            return Err("only the bot's installer renews it".into());
        }
        let mut fresh = self.check_bot_device(&renewal.device, id, &member.person, now)?;
        fresh.installation = Some(installation);
        fresh
            .acting(&renewal.device.cert, &self.core.room, now_ns(now))
            .map_err(|reason| reason.to_string())?;
        let same_device =
            renewal.device.cert.device_public_key == live.device_cert.device_public_key;
        let identities = if same_device {
            if renewal.device.cert != live.device_cert {
                return Err("certificate conflict".into());
            }
            if fresh.delegation.issued_at_ns <= row.delegation.issued_at_ns {
                return Ok(false);
            }
            let mut renewed = live;
            renewed.delegate = Some(fresh);
            vec![renewed]
        } else {
            if self
                .core
                .certificates
                .contains_key(&renewal.device.cert.device_public_key)
            {
                return Err("the bot's new device is already in this room".into());
            }
            let mut withdrawn = live;
            if let Some(old) = withdrawn.delegate.as_mut() {
                old.withdrawn_at_ns = Some(now_ns(now));
            }
            vec![
                withdrawn,
                GovernedIdentity {
                    device_cert: renewal.device.cert.clone(),
                    revocations: vec![],
                    admission: None,
                    admissions: vec![],
                    workspace_delegations: vec![],
                    delegate: Some(fresh),
                },
            ]
        };
        self.put_bot_change(
            &BotWrite {
                identities,
                ..BotWrite::default()
            },
            key,
            now,
        )
    }

    /// Write `write` into this room's governance in one commit, as the
    /// pinned authority `key`, and take it in as every member will.
    pub(super) fn put_bot_change(
        &mut self,
        write: &BotWrite,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.require_governance_writer(key, now)?;
        let mut doc = self
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let before = doc.get_heads();
        let identities = governed_map(&doc, "peer_identities_v1")?;
        let names = governed_map(&doc, "peers")?;
        let grants = governed_map(&doc, "grants")?;
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        for identity in &write.identities {
            let id = hex::encode(identity.device_cert.device_public_key);
            doc.put(
                &identities,
                &id,
                automerge::ScalarValue::Bytes(serde_json::to_vec(identity).map_err(err)?),
            )
            .map_err(err)?;
            doc.put(&names, &id, identity.device_cert.device_label.as_str())
                .map_err(err)?;
        }
        for id in &write.revoke {
            if doc.get(&grants, id.as_str()).map_err(err)?.is_some() {
                doc.delete(&grants, id.as_str()).map_err(err)?;
            }
        }
        for grant in &write.grant {
            if grant.node.room_id() != Some(self.core.room.as_str()) {
                return Err(format!("grant {:?} names another room", grant.id));
            }
            doc.put(
                &grants,
                grant.id.as_str(),
                automerge::ScalarValue::Bytes(serde_json::to_vec(grant).map_err(err)?),
            )
            .map_err(err)?;
        }
        if write.program.is_some() || write.drop_program.is_some() {
            let programs = match doc.get(ROOT, PROGRAMS).map_err(err)? {
                Some((_, map)) => map,
                None => doc.put_object(ROOT, PROGRAMS, ObjType::Map).map_err(err)?,
            };
            if let Some((hash, text)) = &write.program {
                doc.put(
                    &programs,
                    hash,
                    automerge::ScalarValue::Bytes(text.as_bytes().to_vec()),
                )
                .map_err(err)?;
            }
            if let Some(hash) = &write.drop_program
                && doc.get(&programs, hash).map_err(err)?.is_some()
            {
                doc.delete(&programs, hash).map_err(err)?;
            }
        }
        doc.commit();
        self.apply_governance_doc(doc, &before, key, now)
    }
}
impl StatePeer {
    /// Ask the room's host to run directory bot `request`, as `caller`, who
    /// must be able to install bots here. Asking again for the same entry
    /// replaces the version asked for.
    pub(super) fn apply_request_bot(
        &mut self,
        caller: &DeviceCert,
        request: &crate::workspace_membership::BotRequest,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let member = self.check_bot_governance(caller, governance::INSTALL_BOT, now)?;
        if let Some(program) = &request.program {
            // The host runs a developer's program as the room's root, so the
            // person asking must hold everything it asks for themselves.
            let shortcut = parse_program(program.as_bytes()).map_err(|error| error.to_string())?;
            let grants = bot::grants(
                &self.core.room,
                &request.entry,
                &member.person,
                &shortcut.grant,
            )
            .map_err(|error| error.to_string())?;
            let asking =
                tilo_action_policy::Standing::person(member.policy.clone(), member.person.clone());
            let authority = asking.authority();
            let cx = Context::at_time(now);
            for grant in &grants {
                RoomPolicy::check_within(&authority, grant, &cx).map_err(|refusal| {
                    format!("the bot's program asks for more than you hold: {refusal}")
                })?;
            }
        }
        let previous = self.governance().bot_request(&request.entry);
        if previous
            .as_ref()
            .is_some_and(|previous| &previous.request == request)
        {
            return Ok(false);
        }
        let row = RequestedBot {
            request: request.clone(),
            requested_by: member.person,
            requested_at_ns: now_ns(now),
        };
        self.put_bot_request(&request.entry, Some(&row), key, now)
    }

    /// Take back the request for `entry`, as `caller`, who must be able to
    /// uninstall bots here.
    pub(super) fn apply_withdraw_bot_request(
        &mut self,
        caller: &DeviceCert,
        entry: &str,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_bot_governance(caller, governance::UNINSTALL_BOT, now)?;
        if self.governance().bot_request(entry).is_none() {
            return Ok(false);
        }
        self.put_bot_request(entry, None, key, now)
    }

    /// Write or remove the request for `entry` in one governed commit.
    fn put_bot_request(
        &mut self,
        entry: &str,
        row: Option<&RequestedBot>,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.require_governance_writer(key, now)?;
        let mut doc = self
            .core
            .room_doc
            .clone()
            .ok_or("room governance missing")?;
        let before = doc.get_heads();
        doc.set_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
        let requests = match doc.get(ROOT, REQUESTS).map_err(err)? {
            Some((_, map)) => map,
            None => doc.put_object(ROOT, REQUESTS, ObjType::Map).map_err(err)?,
        };
        match row {
            Some(row) => doc
                .put(
                    &requests,
                    entry,
                    automerge::ScalarValue::Bytes(serde_json::to_vec(row).map_err(err)?),
                )
                .map_err(err)?,
            None => doc.delete(&requests, entry).map_err(err)?,
        }
        doc.commit();
        self.apply_governance_doc(doc, &before, key, now)
    }
}

impl PeerGovernance<'_> {
    /// Every bot the room asks its host to run, by entry.
    #[must_use]
    pub fn bot_requests(self) -> Vec<RequestedBot> {
        self.peer
            .core
            .room_doc
            .as_ref()
            .map_or_else(Vec::new, |doc| requests_in(&**doc))
    }

    /// The room's request for directory entry `entry`, if it stands.
    #[must_use]
    pub fn bot_request(self, entry: &str) -> Option<RequestedBot> {
        let doc = self.peer.core.room_doc.as_ref()?;
        let (_, requests) = doc.get(ROOT, REQUESTS).ok()??;
        read_request(&**doc, &requests, entry)
    }

    /// The program installation rows name by `hash`, as the room keeps it.
    #[must_use]
    pub fn bot_program(self, hash: &str) -> Option<String> {
        let doc = self.peer.core.room_doc.as_ref()?;
        let (_, programs) = doc.get(ROOT, PROGRAMS).ok()??;
        match doc.get(&programs, hash).ok()?? {
            (automerge::Value::Scalar(value), _) => match value.as_ref() {
                automerge::ScalarValue::Bytes(bytes) => String::from_utf8(bytes.clone()).ok(),
                _ => None,
            },
            _ => None,
        }
    }
    /// Every installed bot on this room's roster, acting or not at `now`,
    /// withdrawn rows included so history stays attributed.
    pub fn installations(self, now: SystemTime) -> Result<Vec<InstalledBot>, String> {
        let mut out = Vec::new();
        for identity in self.peer.bot_identities()? {
            let acting = self
                .peer
                .permissions(identity.device_cert.device_public_key, now)
                .is_ok();
            let Some(row) = identity.delegate else {
                continue;
            };
            let Some(installation) = row.installation else {
                continue;
            };
            out.push(InstalledBot {
                installation,
                device_public_key: identity.device_cert.device_public_key,
                delegation_expires_at_ns: row.delegation.expires_at_ns,
                withdrawn_at_ns: row.withdrawn_at_ns,
                acting,
            });
        }
        out.sort_by(|a, b| {
            (a.installation.id.as_str(), a.withdrawn_at_ns.is_none())
                .cmp(&(b.installation.id.as_str(), b.withdrawn_at_ns.is_none()))
        });
        Ok(out)
    }
    /// Installation `id` as of `now`: its live row, else its last withdrawn
    /// one.
    #[must_use]
    pub fn installation(self, id: &str, now: SystemTime) -> Option<InstalledBot> {
        let rows = self.installations(now).ok()?;
        let mut matching = rows
            .into_iter()
            .filter(|row| row.installation.id.as_str() == id);
        let all: Vec<InstalledBot> = matching.by_ref().collect();
        all.iter()
            .find(|row| row.withdrawn_at_ns.is_none())
            .or_else(|| all.iter().max_by_key(|row| row.withdrawn_at_ns))
            .cloned()
    }
}

impl StatePeer {
    /// Decide governance action `action` for `caller` on bots.
    fn check_bot_governance(
        &self,
        caller: &DeviceCert,
        action: &str,
        now: SystemTime,
    ) -> Result<super::MemberAuthority, String> {
        let member = self.permissions(caller.device_public_key, now)?;
        let bound = roles::foreign_device_bound();
        member
            .policy
            .check_action(
                &member.authority(&bound),
                action,
                &Context::at_time(now),
                false,
            )
            .map_err(|refusal| refusal.to_string())?;
        Ok(member)
    }

    /// Withdraw installation `id` and revoke its grants, as `caller`.
    pub(super) fn apply_uninstall_bot(
        &mut self,
        caller: &DeviceCert,
        id: &str,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_bot_governance(caller, governance::UNINSTALL_BOT, now)?;
        let mut live = self.live_bot(id)?.ok_or("no such bot")?;
        if let Some(row) = live.delegate.as_mut() {
            row.withdrawn_at_ns = Some(now_ns(now));
        }
        let revoke = self
            .bot_grants(id, now)?
            .into_iter()
            .map(|grant| grant.id)
            .collect();
        self.put_bot_change(
            &BotWrite {
                identities: vec![live],
                revoke,
                ..BotWrite::default()
            },
            key,
            now,
        )
    }

    /// Change installation `id`'s row with `change` (which says whether it
    /// changed anything), as `caller` holding `action`.
    pub(super) fn apply_bot_flag(
        &mut self,
        caller: &DeviceCert,
        id: &str,
        action: &str,
        change: impl FnOnce(&mut Installation) -> bool,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_bot_governance(caller, action, now)?;
        let mut live = self.live_bot(id)?.ok_or("no such bot")?;
        let installation = live
            .delegate
            .as_mut()
            .and_then(|row| row.installation.as_mut())
            .ok_or("bot row missing")?;
        if !change(installation) {
            return Ok(false);
        }
        self.put_bot_change(
            &BotWrite {
                identities: vec![live],
                ..BotWrite::default()
            },
            key,
            now,
        )
    }

    pub(super) fn apply_adopt_bot(
        &mut self,
        caller: &DeviceCert,
        adoption: &crate::workspace_membership::BotAdoption,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let member = self.check_bot_governance(caller, governance::ADOPT_BOT, now)?;
        let id = adoption.installation.as_str();
        let live = self.live_bot(id)?.ok_or("no such bot")?;
        let mut installation = live
            .delegate
            .as_ref()
            .and_then(|row| row.installation.clone())
            .ok_or("bot row missing")?;
        if installation.installed_by == member.person {
            return Err("the bot is already yours".into());
        }
        installation.installed_by.clone_from(&member.person);
        let mut fresh = self.check_bot_device(&adoption.device, id, &member.person, now)?;
        fresh.installation = Some(installation);
        fresh
            .acting(&adoption.device.cert, &self.core.room, now_ns(now))
            .map_err(|reason| reason.to_string())?;
        if self
            .core
            .certificates
            .contains_key(&adoption.device.cert.device_public_key)
        {
            return Err("the bot's new device is already in this room".into());
        }
        // The adopter answers for the bot from now on, so its grant must sit
        // within what the adopter may do, and names them as its grantor.
        let cx = Context::at_time(now);
        let bound = roles::foreign_device_bound();
        let authority = member.authority(&bound);
        let old = self.bot_grants(id, now)?;
        let mut reissued = Vec::with_capacity(old.len());
        for grant in &old {
            let mut next = grant.clone();
            next.granted_by = tilo_action_policy::Subject::person(member.person.clone());
            RoomPolicy::check_within(&authority, &next, &cx)
                .map_err(|refusal| refusal.to_string())?;
            reissued.push(next);
        }
        let mut withdrawn = live;
        if let Some(old_row) = withdrawn.delegate.as_mut() {
            old_row.withdrawn_at_ns = Some(now_ns(now));
        }
        let adopted = GovernedIdentity {
            device_cert: adoption.device.cert.clone(),
            revocations: vec![],
            admission: None,
            admissions: vec![],
            workspace_delegations: vec![],
            delegate: Some(fresh),
        };
        self.put_bot_change(
            &BotWrite {
                identities: vec![withdrawn, adopted],
                revoke: old.into_iter().map(|grant| grant.id).collect(),
                grant: reissued,
                ..BotWrite::default()
            },
            key,
            now,
        )
    }
}

/// The grants `install`'s program gives its bot in `room`, checked: the
/// program matches its hash, parses, names nothing its steps do not use,
/// and gives the bot no more than its installer holds in `policy` now.
fn program_grants(
    room: &str,
    install: &BotInstall,
    policy: &RoomPolicy,
    cx: &Context<'_>,
) -> Result<Vec<Grant>, String> {
    if program_hash(install.program.as_bytes()) != install.installation.program {
        return Err("the program does not match its hash".into());
    }
    let shortcut = parse_program(install.program.as_bytes()).map_err(|error| error.to_string())?;
    if let Some(rule) = tilo_automation::grant::unused_rules(&shortcut).first() {
        return Err(format!(
            "the bot's grant names {}, which none of its steps or triggers use",
            rule.node
        ));
    }
    let installed_by = &install.installation.installed_by;
    let grants = bot::grants(
        room,
        install.installation.id.as_str(),
        installed_by,
        &shortcut.grant,
    )
    .map_err(|error| error.to_string())?;
    // Nobody hands a bot more than its installer holds right now.
    let installer = tilo_action_policy::Standing::person(policy.clone(), installed_by.clone());
    let authority = installer.authority();
    for grant in &grants {
        RoomPolicy::check_within(&authority, grant, cx).map_err(|refusal| refusal.to_string())?;
    }
    Ok(grants)
}

/// The request kept under `entry` in `requests`, if it reads as one for
/// that entry: a row naming another entry than its key is not trusted.
/// Every bot request room document `doc` keeps, by entry.
pub(crate) fn requests_in(doc: &impl ReadDoc) -> Vec<RequestedBot> {
    let Ok(Some((_, requests))) = doc.get(ROOT, REQUESTS) else {
        return Vec::new();
    };
    let mut out: Vec<RequestedBot> = doc
        .keys(&requests)
        .filter_map(|entry| read_request(doc, &requests, &entry))
        .collect();
    out.sort_by(|a, b| a.request.entry.cmp(&b.request.entry));
    out
}

fn read_request(
    doc: &impl ReadDoc,
    requests: &automerge::ObjId,
    entry: &str,
) -> Option<RequestedBot> {
    match doc.get(requests, entry).ok()?? {
        (automerge::Value::Scalar(value), _) => match value.as_ref() {
            automerge::ScalarValue::Bytes(bytes) => serde_json::from_slice::<RequestedBot>(bytes)
                .ok()
                .filter(|row| row.request.entry == entry && row.request.validate().is_ok()),
            _ => None,
        },
        _ => None,
    }
}
