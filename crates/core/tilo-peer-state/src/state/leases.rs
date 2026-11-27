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
//! measured round trip), then the claim whose holder started earliest, then the
//! device key, and the first of them holds. Claims that rank alike, as unranked
//! ones do, go to the device that claimed first. Two takers after a partition
//! therefore converge on one winner; the loser withdraws its claim and stops the
//! work instead of repeating it.
//!
//! A claim is over only when its deadline is [`CLOCK_SKEW_NS`] in the past. A
//! device takes a lease over only then, so devices whose clocks differ by
//! less than that bound never disagree about whether the lease is free.
//! Devices keep their clocks within the bound (any NTP-served machine does);
//! one ahead of the others by more than it can take a lease another device
//! still holds.

use super::SystemTime;
use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use super::{Duration, device_rows, now_ns};
use neighbour_wire::placement::{self, Answer, Power, Rank};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Key of the workspace's leases in its own document: a map, there from the
/// workspace's founding, from `<lease id>/<claiming device key hex>` to its
/// claim ([`device_rows`]).
pub const LEASES_KEY: &str = "leases_v1";
/// How far two devices' clocks may differ and still agree on whether a lease
/// is over: a claim's deadline is only taken as passed, and the lease as free
/// to take, once it is this far in the past.
pub const CLOCK_SKEW_NS: u64 = 60_000_000_000;

/// The lease running the workspace's VM of `environment` is taken under: the
/// one device whose VM service boots it, so it is up on exactly one.
#[must_use]
pub fn vm_lease(environment: &str) -> String {
    format!("vm/{environment}")
}

/// The one input a VM's placement weighs: its disk.
const DISK: [u8; 32] = [0; 32];

/// What a VM machine ranks itself by for a workspace's VM: its own `rank` where
/// it `holds` the VM's disk. One that would have to fetch the disk of
/// `disk_bytes` ranks below every machine that holds it, whatever their loads,
/// and among those that would, by how long the fetch takes
/// ([`placement::transfer`]) as its round trip: the VM stays where its disk
/// is, and moves only once that machine is gone.
#[must_use]
pub fn vm_rank(rank: Rank, disk_bytes: u64, holds: bool) -> Rank {
    if holds {
        return rank;
    }
    let work = neighbour_wire::placement::Work {
        inputs: vec![(DISK, disk_bytes)],
        input_bytes: disk_bytes,
        local_wall_ms: None,
    };
    let moving = placement::transfer(&Answer::taking(rank, Vec::new()), &work, rank.latency_ms);
    Rank {
        load_permille: rank.load_permille.saturating_add(1_000),
        latency_ms: u32::try_from(moving.moving_ms).unwrap_or(u32::MAX),
        ..rank
    }
}

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
    /// When this device's hold of the term began, ns since the epoch on its
    /// clock: kept across its renewals, so of two claims that rank alike the
    /// one made first holds.
    pub since_ns: u64,
}

impl LeaseClaim {
    /// How this claim orders against `other` for holding the lease: better
    /// rank first, then the earlier start, then the lower device key.
    fn order(&self, other: &Self) -> Ordering {
        let rank = |claim: &Self| {
            (
                claim.rank.power,
                claim.rank.load_permille,
                claim.rank.latency_ms,
            )
        };
        rank(self)
            .cmp(&rank(other))
            .then(self.since_ns.cmp(&other.since_ns))
            .then(self.device.cmp(&other.device))
    }
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
    /// A peer one version back writes no start (the one-version window,
    /// `docs/architecture/compatibility.md`): its claim reads as one made at
    /// the epoch, and the frozen claim it wrote round-trips unchanged.
    #[serde(default, skip_serializing_if = "is_zero")]
    since_ns: u64,
}

/// Whether a stored start is the epoch, which a claim of this build never is.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde hands the field by reference, and the lint does not see it in every build"
)]
const fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// A claim as every device reads it from its stored text.
fn claim_of(device: [u8; 32], json: &str) -> Option<LeaseClaim> {
    let claimed = serde_json::from_str::<StoredClaim>(json).ok()?;
    Some(LeaseClaim {
        device,
        term: claimed.term,
        expires_at_ns: claimed.expires_at_ns,
        rank: claimed.rank,
        since_ns: claimed.since_ns,
    })
}

/// The claims held under `id` in `doc`.
fn claims_of(doc: Option<&automerge::AutoCommit>, id: &str) -> Vec<LeaseClaim> {
    device_rows::rows_of(doc, LEASES_KEY, id)
        .into_iter()
        .filter_map(|(device, json)| claim_of(device, &json))
        .collect()
}

/// The lease `id` made of `claims` at `now_ns`: the first live claim of the
/// latest term ([`LeaseClaim::order`]) is the holder.
fn lease_of(id: &str, claims: Vec<LeaseClaim>, now_ns: u64) -> Lease {
    let term = claims.iter().map(|claim| claim.term).max().unwrap_or(0);
    let holder = claims
        .iter()
        .filter(|claim| {
            claim.term == term && claim.expires_at_ns.saturating_add(CLOCK_SKEW_NS) >= now_ns
        })
        .min_by(|left, right| left.order(right))
        .copied();
    Lease {
        id: id.to_owned(),
        term,
        holder,
        claims,
    }
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
        let mut claims = std::collections::BTreeMap::<String, Vec<LeaseClaim>>::new();
        for (id, device, json) in
            device_rows::all_rows(self.peer.core.workspace_doc.as_deref(), LEASES_KEY)
        {
            if let Some(claim) = claim_of(device, &json) {
                claims.entry(id).or_default().push(claim);
            }
        }
        claims
            .into_iter()
            .map(|(id, claims)| lease_of(&id, claims, now_ns(now)))
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
                self.write_claim_removed(id, &device.device_public_key, now)?;
            }
            return Ok(None);
        }
        // A deadline still live keeps its term: the holder is renewing. A
        // lease that is over, or a first claim, starts the next term.
        let live = mine.is_some_and(|claim| {
            claim.term == held.term && claim.expires_at_ns.saturating_add(CLOCK_SKEW_NS) >= now_ns
        });
        let (term, since_ns) = match mine {
            Some(claim) if live => (held.term, claim.since_ns),
            _ => (
                held.term.checked_add(1).ok_or("lease term exhausted")?,
                now_ns,
            ),
        };
        let claim = LeaseClaim {
            device: device.device_public_key,
            term,
            expires_at_ns: now_ns.saturating_add(ttl_ns),
            rank,
            since_ns,
        };
        self.write_claim(id, claim, now)?;
        let after = self.peer.workspace().lease(id, now);
        if after
            .holder
            .is_some_and(|holder| holder.device == device.device_public_key)
        {
            Ok(Some(claim))
        } else {
            // Another device won the same term. Its call is the work's; this
            // device's claim goes so it does not keep contesting it.
            self.write_claim_removed(id, &device.device_public_key, now)?;
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
        self.write_claim_removed(id, &device.device_public_key, now)
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
            tilo_action_types::ActionKind::Undoable,
            now,
        ) {
            return Err("lease claimant may not change this workspace".into());
        }
        Ok(())
    }

    /// Write `claim` into its device's row under its lease, as this device's
    /// own change.
    fn write_claim(&mut self, id: &str, claim: LeaseClaim, now: SystemTime) -> Result<(), String> {
        let json = serde_json::to_string(&StoredClaim {
            term: claim.term,
            expires_at_ns: claim.expires_at_ns,
            rank: claim.rank,
            since_ns: claim.since_ns,
        })
        .map_err(|error| error.to_string())?;
        let key = device_rows::row_key(id, &claim.device);
        self.write_row(LEASES_KEY, &key, &claim.device, Some(&json), now)?;
        Ok(())
    }

    /// Remove `device`'s claim under `id`, answering whether one was there.
    fn write_claim_removed(
        &mut self,
        id: &str,
        device: &[u8; 32],
        now: SystemTime,
    ) -> Result<bool, String> {
        let key = device_rows::row_key(id, device);
        self.write_row(LEASES_KEY, &key, device, None, now)
    }
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
            since_ns: 0,
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

    /// Two VM machines that claim a workspace's VM at once settle on the one
    /// holding its disk, though the other is less loaded: the VM stays where
    /// it is, and moves only once that machine is gone.
    #[test]
    fn the_vm_goes_to_the_machine_that_holds_its_disk() {
        let (mut left, _left_key, mut right, _right_key) = desktops();
        let now = now();
        let (left_cert, right_cert) = (cert_of(&left), cert_of(&right));
        let lease = vm_lease("default");
        let disk = 8 << 30;
        let ttl = Duration::from_secs(60);
        // The left machine has the VM, so its disk is there.
        assert!(
            left.workspace_mut()
                .take_lease(&left_cert, &lease, ttl, vm_rank(rank(900), disk, true), now)
                .unwrap()
                .is_some()
        );
        sync(&left, &mut right);
        // Both machines restart after its claim is over, and claim it at once.
        let back = at(now, 62 + CLOCK_SKEW_NS / 1_000_000_000);
        for (peer, cert, holds, load) in [
            (&mut left, &left_cert, true, 900),
            (&mut right, &right_cert, false, 0),
        ] {
            let ranked = vm_rank(rank(load), disk, holds);
            assert!(
                peer.workspace_mut()
                    .take_lease(cert, &lease, ttl, ranked, back)
                    .unwrap()
                    .is_some()
            );
        }
        sync(&left, &mut right);
        sync(&right, &mut left);
        for peer in [&left, &right] {
            assert_eq!(
                peer.workspace()
                    .lease(&lease, back)
                    .holder
                    .map(|claim| claim.device),
                Some(left_cert.device_public_key),
                "the machine with the disk keeps the VM"
            );
        }
        // Once it is gone and its claim over, the other machine takes the VM.
        let over = at(back, 62 + CLOCK_SKEW_NS / 1_000_000_000);
        let taken = right
            .workspace_mut()
            .take_lease(
                &right_cert,
                &lease,
                ttl,
                vm_rank(rank(0), disk, false),
                over,
            )
            .unwrap()
            .expect("the lease moves");
        assert_eq!(taken.device, right_cert.device_public_key);
    }

    #[test]
    fn two_first_claims_on_a_new_lease_both_merge_and_the_better_ranked_holds() {
        let (left, _left_key, right, _right_key) = desktops();
        // A map two devices each put at one key while apart merges as the
        // map of the greater device key alone. That device ranks worse here,
        // so a lease kept in such maps would hold the wrong device.
        let (mut better, mut worse) =
            if cert_of(&left).device_public_key < cert_of(&right).device_public_key {
                (left, right)
            } else {
                (right, left)
            };
        let now = now();
        let better_cert = cert_of(&better);
        let worse_cert = cert_of(&worse);
        // Neither device could see the other when each claimed the lease no
        // device had claimed before.
        let better_claim = better
            .workspace_mut()
            .take_lease(&better_cert, "tick", Duration::from_secs(60), rank(0), now)
            .unwrap()
            .expect("alone, the better device wins");
        let worse_claim = worse
            .workspace_mut()
            .take_lease(&worse_cert, "tick", Duration::from_secs(60), rank(5), now)
            .unwrap()
            .expect("alone, the worse device wins too");
        assert_eq!(
            better_claim.term, worse_claim.term,
            "both started the same term"
        );

        // The partition ends: both claims are there on both devices, and the
        // less loaded device holds.
        sync(&better, &mut worse);
        sync(&worse, &mut better);
        let merged = better.workspace().lease("tick", now);
        assert_eq!(merged.holder, Some(better_claim));
        let mut claims = merged.claims.clone();
        claims.sort_by_key(|claim| claim.device);
        assert_eq!(claims, vec![better_claim, worse_claim], "both claims merge");
        assert_eq!(merged, worse.workspace().lease("tick", now));
        assert_eq!(worse.workspace().leases(now), vec![merged]);

        // The loser's next claim attempt stops it and withdraws its claim.
        assert!(
            worse
                .workspace_mut()
                .take_lease(&worse_cert, "tick", Duration::from_secs(60), rank(5), now)
                .unwrap()
                .is_none()
        );
        sync(&worse, &mut better);
        assert_eq!(
            better.workspace().lease("tick", now).claims,
            vec![better_claim],
            "one claim is left"
        );
    }

    /// Two devices that claim a lease neither saw claimed, ranking alike,
    /// converge on the one that claimed first once their claims merge, whatever
    /// their keys; the later one's renewal then answers that it lost, and a
    /// renewal of the winner's keeps the start it began with.
    #[test]
    fn of_two_alike_claims_the_earlier_holds_and_keeps_its_start() {
        let (mut left, _left_key, mut right, _right_key) = desktops();
        let now = now();
        let (left_cert, right_cert) = (cert_of(&left), cert_of(&right));
        // The higher key claims first, so the key alone would pick the other.
        let ((first, first_cert), (second, second_cert)) =
            if left_cert.device_public_key > right_cert.device_public_key {
                ((&mut left, &left_cert), (&mut right, &right_cert))
            } else {
                ((&mut right, &right_cert), (&mut left, &left_cert))
            };
        let earlier = first
            .workspace_mut()
            .take_lease(first_cert, "turn", Duration::ZERO, unranked(), now)
            .unwrap()
            .expect("alone, the first device holds it");
        second
            .workspace_mut()
            .take_lease(second_cert, "turn", Duration::ZERO, unranked(), at(now, 1))
            .unwrap()
            .expect("alone, the second device holds it too");

        sync(first, second);
        sync(second, first);
        let merged = second.workspace().lease("turn", at(now, 2));
        assert_eq!(merged.holder, Some(earlier), "the earlier claim holds");
        assert_eq!(merged, first.workspace().lease("turn", at(now, 2)));
        assert!(
            second
                .workspace_mut()
                .take_lease(second_cert, "turn", Duration::ZERO, unranked(), at(now, 2))
                .unwrap()
                .is_none(),
            "the later claim's renewal answers that it lost"
        );
        let renewed = first
            .workspace_mut()
            .take_lease(first_cert, "turn", Duration::ZERO, unranked(), at(now, 30))
            .unwrap()
            .expect("the winner renews");
        assert_eq!(
            renewed.since_ns, earlier.since_ns,
            "a renewal keeps its start"
        );

        // Silent for longer than the clocks may differ, the claim is over and
        // the other device takes the lease.
        let taken = second
            .workspace_mut()
            .take_lease(second_cert, "turn", Duration::ZERO, unranked(), at(now, 91))
            .unwrap()
            .expect("a claim silent past the skew is taken over");
        assert_eq!(
            taken.term,
            renewed.term + 1,
            "a takeover starts the next term"
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
