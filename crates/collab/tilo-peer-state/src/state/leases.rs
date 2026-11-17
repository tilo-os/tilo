//! Which key-holding device does one piece of work in a workspace.
//!
//! State merges by itself; a side effect does not. A lease names the device
//! doing a piece of work, with a deadline it keeps pushing out while it
//! works. The lease is a resource of the workspace's own state, so a group of
//! desktops holding a workspace has it with no server: whoever wants the work
//! writes its own claim, and every key holder reads the same claim set.
//!
//! Claims are per device, never one shared register, so two devices that claim
//! at the same time both keep their row after a merge and nothing is lost.
//! The holder is then decided the same way on every device: the claims are
//! ranked the way [`placement`](neighbour_wire::placement) ranks devices (power, load,
//! measured round trip, then the device key), and the best of them holds. Two
//! takers after a partition therefore converge on one winner; the loser
//! withdraws its claim and stops the work instead of repeating it.
//!
//! A claim is over only when its deadline is [`CLOCK_SKEW_NS`] in the past. A
//! device takes a lease over only then, so devices whose clocks differ by
//! less than that bound never disagree about whether the lease is free.
//! Devices keep their clocks within the bound (any NTP-served machine does);
//! one ahead of the others by more than it can take a lease another device
//! still holds.

use super::SystemTime;
use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use super::{Duration, ROOT, now_ns};
use automerge::transaction::Transactable as _;
use automerge::{ObjType, ReadDoc, ScalarValue, Value};
use neighbour_wire::placement::{self, Answer, Power, Rank};
use serde::{Deserialize, Serialize};

/// Key of the workspace's leases in its own document: a map from each lease's
/// id to a map from each claiming device key (hex) to its claim.
pub const LEASES_KEY: &str = "leases_v1";
/// How far two devices' clocks may differ and still agree on whether a lease
/// is over: a claim's deadline is only taken as passed, and the lease as free
/// to take, once it is this far in the past.
pub const CLOCK_SKEW_NS: u64 = 60_000_000_000;

/// What a device ranks itself by when it has nothing better to say: it takes
/// the work without preferring any power state or load.
#[must_use]
pub fn unranked() -> Rank {
    Rank {
        power: Power::Unknown,
        load_permille: 0,
        latency_ms: 0,
    }
}

/// One device's claim on a lease, as the workspace keeps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeaseClaim {
    /// The claiming device.
    pub device: [u8; 32],
    /// Which taking of the lease this claim belongs to. A claim on an expired
    /// lease starts the next term; every claim of an earlier one is struck off
    /// by it.
    pub term: u64,
    /// The deadline this device pushed out to, ns since the epoch, read on
    /// the claiming device's clock.
    pub expires_at_ns: u64,
    /// What the device ranks itself by.
    pub rank: Rank,
}

/// A lease as the workspace holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    /// The id the work is taken under.
    pub id: String,
    /// The term the live claims compete in.
    pub term: u64,
    /// The device holding it, if one does.
    pub holder: Option<LeaseClaim>,
    /// Every claim the workspace holds: live ones and ones never withdrawn.
    pub claims: Vec<LeaseClaim>,
}

impl Lease {
    /// Whether no device holds the lease: no claim of its latest term is
    /// still within [`CLOCK_SKEW_NS`] of its deadline.
    #[must_use]
    pub fn is_free(&self) -> bool {
        self.holder.is_none()
    }
}

/// How a claim is stored: the JSON every device reads.
#[derive(Serialize, Deserialize)]
struct StoredClaim {
    term: u64,
    expires_at_ns: u64,
    rank: Rank,
}

/// The claims held under `id` in `doc`.
fn claims_of(doc: Option<&automerge::AutoCommit>, id: &str) -> Vec<LeaseClaim> {
    let Some(doc) = doc else {
        return Vec::new();
    };
    let Ok(Some((Value::Object(ObjType::Map), leases))) = doc.get(ROOT, LEASES_KEY) else {
        return Vec::new();
    };
    let Ok(Some((Value::Object(ObjType::Map), claims))) = doc.get(&leases, id) else {
        return Vec::new();
    };
    let mut held = Vec::new();
    for device in doc.keys(&claims) {
        let Ok(Some((Value::Scalar(value), _))) = doc.get(&claims, device.as_str()) else {
            continue;
        };
        let ScalarValue::Str(json) = value.as_ref() else {
            continue;
        };
        let (Ok(claimed), Some(key)) = (
            serde_json::from_str::<StoredClaim>(json),
            device_of(&device),
        ) else {
            continue;
        };
        held.push(LeaseClaim {
            device: key,
            term: claimed.term,
            expires_at_ns: claimed.expires_at_ns,
            rank: claimed.rank,
        });
    }
    held
}

/// The lease `id` made of `claims` at `now_ns`: the best-ranked live claim of
/// the latest term is the holder.
fn lease_of(id: &str, claims: Vec<LeaseClaim>, now_ns: u64) -> Lease {
    let term = claims.iter().map(|claim| claim.term).max().unwrap_or(0);
    let live = |claim: &&LeaseClaim| {
        claim.term == term && claim.expires_at_ns.saturating_add(CLOCK_SKEW_NS) >= now_ns
    };
    let holder = placement::best(
        claims
            .iter()
            .filter(live)
            .map(|claim| (claim.device, Answer::taking(claim.rank, Vec::new()))),
    )
    .and_then(|(best, _)| {
        claims
            .iter()
            .find(|claim| claim.device == best && claim.term == term)
            .copied()
    });
    Lease {
        id: id.to_owned(),
        term,
        holder,
        claims,
    }
}

/// The key a device row's hex names, if it is one.
fn device_of(device: &str) -> Option<[u8; 32]> {
    let key = tilo_failure::ignored!(
        "a device row another build wrote is not a key this one can name",
        hex::decode(device).ok()
    )?;
    <[u8; 32]>::try_from(key.as_slice()).ok()
}

impl PeerWorkspace<'_> {
    /// The lease `id` as this peer's workspace holds it now.
    #[must_use]
    pub fn lease(self, id: &str, now: SystemTime) -> Lease {
        lease_of(
            id,
            claims_of(self.peer.core.workspace_doc.as_deref(), id),
            now_ns(now),
        )
    }

    /// Every lease the workspace holds, by id.
    #[must_use]
    pub fn leases(self, now: SystemTime) -> Vec<Lease> {
        let Some(doc) = self.peer.core.workspace_doc.as_deref() else {
            return Vec::new();
        };
        let Ok(Some((Value::Object(ObjType::Map), leases))) = doc.get(ROOT, LEASES_KEY) else {
            return Vec::new();
        };
        doc.keys(&leases)
            .map(|id| lease_of(&id, claims_of(Some(doc), &id), now_ns(now)))
            .collect()
    }
}

impl PeerWorkspaceMut<'_> {
    /// Claim `id` for `device`, a member's device whose exact current
    /// certificate this workspace holds and who may change it, for `ttl` from
    /// `now`, ranked by `rank`. A lease another device holds live is not
    /// taken.
    ///
    /// The claim goes in the device's own row, so no other device's claim is
    /// replaced; the winner is read back from the merged claims. One that does
    /// not win withdraws its claim and answers `None`, so a device that lost
    /// the ranking never goes on to do the work and does not contest the
    /// winner on the next pass.
    ///
    /// # Errors
    ///
    /// `device` is not the exact current certificate of a device that may
    /// change this workspace, or the claim cannot be written.
    pub fn take_lease(
        mut self,
        device: &tilo_user_identity::DeviceCert,
        id: &str,
        ttl: Duration,
        rank: Rank,
        now: SystemTime,
    ) -> Result<Option<LeaseClaim>, String> {
        self.check_writer(device, now)?;
        let now_ns = now_ns(now);
        let ttl_ns = u64::try_from(ttl.as_nanos()).map_err(|_| "lease ttl overflows")?;
        let held = self.peer.workspace().lease(id, now);
        let mine = held
            .claims
            .iter()
            .find(|claim| claim.device == device.device_public_key)
            .copied();
        if let Some(holder) = held.holder
            && holder.device != device.device_public_key
        {
            // Another device holds it. A claim of this device's that did not
            // win the term goes with it, so it stops being a contender.
            if mine.is_some_and(|claim| claim.term <= holder.term) {
                self.write_claim_removed(id, &device.device_public_key)?;
            }
            return Ok(None);
        }
        // A deadline still live keeps its term: the holder is renewing. A
        // lease that is over, or a first claim, starts the next term.
        let live = mine.is_some_and(|claim| {
            claim.term == held.term && claim.expires_at_ns.saturating_add(CLOCK_SKEW_NS) >= now_ns
        });
        let term = if live {
            held.term
        } else {
            held.term.checked_add(1).ok_or("lease term exhausted")?
        };
        let claim = LeaseClaim {
            device: device.device_public_key,
            term,
            expires_at_ns: now_ns.saturating_add(ttl_ns),
            rank,
        };
        self.write_claim(id, claim)?;
        let after = self.peer.workspace().lease(id, now);
        if after
            .holder
            .is_some_and(|holder| holder.device == device.device_public_key)
        {
            Ok(Some(claim))
        } else {
            // Another device won the same term. Its call is the work's; this
            // device's claim goes so it does not keep contesting it.
            self.write_claim_removed(id, &device.device_public_key)?;
            Ok(None)
        }
    }

    /// Stop claiming `id` for `device`. Answers whether a claim was there.
    ///
    /// # Errors
    ///
    /// `device` is not the exact current certificate of a device that may
    /// change this workspace, or the claim cannot be removed.
    pub fn release_lease(
        mut self,
        device: &tilo_user_identity::DeviceCert,
        id: &str,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.check_writer(device, now)?;
        self.write_claim_removed(id, &device.device_public_key)
    }

    /// Check that `device` is this workspace's exact current certificate for a
    /// device allowed to make undoable changes in it.
    pub(super) fn check_writer(
        &self,
        device: &tilo_user_identity::DeviceCert,
        now: SystemTime,
    ) -> Result<(), String> {
        if self
            .peer
            .certificate(&device.device_public_key, now)
            .map_err(|error| error.to_string())?
            != device
        {
            return Err("lease claimant is not exact current admitted evidence".into());
        }
        let member = self.peer.membership(device.device_public_key, now)?;
        if !member.allows(
            &tilo_action_policy::TreePath::workspace(&self.peer.core.workspace),
            tilo_action_policy::ActionKind::Undoable,
            now,
        ) {
            return Err("lease claimant may not change this workspace".into());
        }
        Ok(())
    }

    /// Write `claim` into its device's row under its lease, as this device's
    /// own change.
    fn write_claim(&mut self, id: &str, claim: LeaseClaim) -> Result<(), String> {
        let json = serde_json::to_string(&StoredClaim {
            term: claim.term,
            expires_at_ns: claim.expires_at_ns,
            rank: claim.rank,
        })
        .map_err(|error| error.to_string())?;
        self.write_claim_row(id, &claim.device, Some(&json))
    }

    /// Remove `device`'s claim under `id`, answering whether one was there.
    fn write_claim_removed(&mut self, id: &str, device: &[u8; 32]) -> Result<bool, String> {
        let held = {
            let doc = self.peer.core.workspace_doc.as_deref();
            let row = doc.and_then(|doc| {
                let (Value::Object(ObjType::Map), leases) = doc.get(ROOT, LEASES_KEY).ok()?? else {
                    return None;
                };
                let (Value::Object(ObjType::Map), claims) = doc.get(&leases, id).ok()?? else {
                    return None;
                };
                doc.get(&claims, hex::encode(device).as_str()).ok()?
            });
            row.is_some()
        };
        if !held {
            return Ok(false);
        }
        self.write_claim_row(id, device, None)?;
        Ok(true)
    }

    /// Put `json` in `device`'s row under `id`, or remove the row when `json`
    /// is `None`.
    fn write_claim_row(
        &mut self,
        id: &str,
        device: &[u8; 32],
        json: Option<&str>,
    ) -> Result<(), String> {
        let actor = self.peer.workspace_actor_for(device);
        let workspace = self
            .peer
            .core
            .workspace_doc
            .as_deref()
            .ok_or("lease workspace is absent")?;
        let mut candidate = workspace.clone();
        candidate.set_actor(actor);
        let leases = match candidate.get(ROOT, LEASES_KEY).map_err(message)? {
            Some((Value::Object(ObjType::Map), leases)) => leases,
            Some(_) => return Err("the workspace's leases are malformed".into()),
            None => candidate
                .put_object(ROOT, LEASES_KEY, ObjType::Map)
                .map_err(message)?,
        };
        let claims = match candidate.get(&leases, id).map_err(message)? {
            Some((Value::Object(ObjType::Map), claims)) => claims,
            Some(_) => return Err(format!("lease `{id}` is malformed")),
            None => candidate
                .put_object(&leases, id, ObjType::Map)
                .map_err(message)?,
        };
        let row = hex::encode(device);
        match json {
            Some(json) => candidate
                .put(&claims, row.as_str(), json)
                .map_err(message)?,
            None => candidate.delete(&claims, row.as_str()).map_err(message)?,
        }
        candidate.commit();
        self.peer.core.workspace_doc = Some(candidate.into());
        Ok(())
    }
}

/// A document operation's error as text.
fn message(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "test arithmetic asserts a deadline"
)]
mod tests {
    use super::*;
    use crate::owner::testing::now;
    use crate::state::test_desktops::{cert_of, desktops, sync};
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::DeviceCert;

    fn rank(load: u32) -> Rank {
        Rank {
            power: Power::Unknown,
            load_permille: load,
            latency_ms: 0,
        }
    }

    fn at(now: SystemTime, seconds: u64) -> SystemTime {
        now + Duration::from_secs(seconds)
    }

    /// A claim reaches the workspace's other holders as a change to its
    /// document, so a server of the previous peer version, still running
    /// while its person's servers update one at a time, reads the claims of
    /// this build and this build reads its: the claim as that version wrote
    /// it is frozen beside `neighbour-wire`.
    #[test]
    fn a_claim_reads_and_writes_as_the_previous_peer_version_wrote_it() {
        const FROZEN: &str = include_str!("../../../neighbour-wire/previous-peer-frames.txt");
        let kept = FROZEN
            .lines()
            .find_map(|line| line.strip_prefix("lease-claim "))
            .expect("previous-peer-frames.txt keeps a lease claim");
        let today = serde_json::to_string(&StoredClaim {
            term: 3,
            expires_at_ns: 4_000_000_000,
            rank: rank(250),
        })
        .unwrap();
        assert_eq!(today, kept, "this build writes a lease claim differently");
        let read: StoredClaim = serde_json::from_str(kept).unwrap();
        assert_eq!(serde_json::to_string(&read).unwrap(), kept);
    }

    #[test]
    fn one_device_holds_the_lease_and_a_second_takes_it_only_once_it_is_over() {
        let (mut left, _left_key, mut right, _right_key) = desktops();
        let now = now();
        let left_cert = cert_of(&left);
        let right_cert = cert_of(&right);

        let claim = left
            .workspace_mut()
            .take_lease(&left_cert, "tick", Duration::from_secs(60), rank(0), now)
            .unwrap()
            .expect("the first claimant holds it");
        assert_eq!(left.workspace().lease("tick", now).holder, Some(claim));

        sync(&left, &mut right);
        assert_eq!(right.workspace().lease("tick", now).holder, Some(claim));
        assert!(
            right
                .workspace_mut()
                .take_lease(&right_cert, "tick", Duration::from_secs(60), rank(0), now)
                .unwrap()
                .is_none(),
            "a live lease is not taken"
        );
        assert_eq!(
            right
                .workspace()
                .lease("tick", now)
                .claims
                .iter()
                .map(|claim| claim.device)
                .collect::<Vec<_>>(),
            vec![left_cert.device_public_key],
            "a device that does not hold it leaves no claim of its own"
        );

        // The holder stops renewing: only once its deadline is over by the
        // skew bound does the other device take it.
        let just_over = at(now, 61);
        assert!(
            right.workspace().lease("tick", just_over).holder.is_some(),
            "within the skew bound the lease still holds"
        );
        let over = at(now, 62 + CLOCK_SKEW_NS / 1_000_000_000);
        assert!(right.workspace().lease("tick", over).is_free());
        let taken = right
            .workspace_mut()
            .take_lease(&right_cert, "tick", Duration::from_secs(60), rank(0), over)
            .unwrap()
            .expect("the second device takes the over lease");
        assert_eq!(taken.term, claim.term + 1, "a takeover starts a term");
        sync(&right, &mut left);
        assert_eq!(left.workspace().lease("tick", over).holder, Some(taken));
    }

    #[test]
    fn two_takers_after_a_partition_merge_on_one_winner_and_the_loser_withdraws() {
        let (mut left, _left_key, mut right, _right_key) = desktops();
        let now = now();
        let left_cert = cert_of(&left);
        let right_cert = cert_of(&right);
        // The lease was over and neither device could see the other: both
        // claim it at once.
        let over = at(now, 62 + CLOCK_SKEW_NS / 1_000_000_000);
        let left_claim = left
            .workspace_mut()
            .take_lease(&left_cert, "tick", Duration::from_secs(60), rank(5), over)
            .unwrap()
            .expect("alone, the left device wins");
        let right_claim = right
            .workspace_mut()
            .take_lease(&right_cert, "tick", Duration::from_secs(60), rank(0), over)
            .unwrap()
            .expect("alone, the right device wins too");
        assert_eq!(
            left_claim.term, right_claim.term,
            "both started the same term"
        );

        // The partition ends. The claims rank the same on both devices and
        // the right device, less loaded, holds.
        sync(&left, &mut right);
        sync(&right, &mut left);
        let winner = left.workspace().lease("tick", over);
        assert_eq!(
            winner.holder.map(|holder| holder.device),
            Some(right_cert.device_public_key)
        );
        assert_eq!(winner, right.workspace().lease("tick", over));

        // The loser's next claim attempt stops it and withdraws its claim.
        assert!(
            left.workspace_mut()
                .take_lease(&left_cert, "tick", Duration::from_secs(60), rank(5), over)
                .unwrap()
                .is_none()
        );
        sync(&left, &mut right);
        assert_eq!(
            right
                .workspace()
                .lease("tick", over)
                .claims
                .iter()
                .map(|claim| claim.device)
                .collect::<Vec<_>>(),
            vec![right_cert.device_public_key],
            "one claim is left"
        );
    }

    #[test]
    fn a_renewing_holder_keeps_its_term() {
        let (mut left, _left_key, _, _right_key) = desktops();
        let now = now();
        let left_cert = cert_of(&left);
        let first = left
            .workspace_mut()
            .take_lease(&left_cert, "tick", Duration::from_secs(60), unranked(), now)
            .unwrap()
            .unwrap();
        let renewed = left
            .workspace_mut()
            .take_lease(
                &left_cert,
                "tick",
                Duration::from_secs(60),
                unranked(),
                at(now, 30),
            )
            .unwrap()
            .unwrap();
        assert_eq!(renewed.term, first.term, "a renewal keeps its term");
        assert!(renewed.expires_at_ns > first.expires_at_ns);
        assert_eq!(
            left.workspace().lease("tick", at(now, 30)).holder,
            Some(renewed)
        );
        // A desktop alone needs no server: the lease is one workspace
        // resource among the others.
        assert_eq!(left.workspace().leases(now).len(), 1);
    }

    #[test]
    fn a_device_that_may_not_change_the_workspace_holds_no_lease() {
        let (mut left, _left_key, _, _right_key) = desktops();
        let now = now();
        let stranger_root = SigningKey::from_bytes(&[77; 32]);
        let stranger_key = SigningKey::from_bytes(&[78; 32]);
        let mut stranger = DeviceCert {
            user_public_key: stranger_root.verifying_key().to_bytes(),
            device_public_key: stranger_key.verifying_key().to_bytes(),
            device_label: "stranger".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut stranger, &stranger_root).unwrap();
        assert!(
            left.workspace_mut()
                .take_lease(&stranger, "tick", Duration::from_secs(60), unranked(), now)
                .is_err()
        );
        assert!(left.workspace().lease("tick", now).is_free());
    }
}
