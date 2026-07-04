//! Workspace roster panel — "who's here, what are they doing".
//!
//! A live-emitted (not CRDT) view of every peer subscribed to the current
//! workspace. Data flows in from the collab broker as a stream of
//! [`RosterUpdate`]s; the panel renders identity chips with a status
//! indicator and last-seen timestamp.
//!
//! The panel lives in the shell chrome (typically the topbar area). It's
//! deliberately kept as a plain data model with its own widget so tests
//! can drive the model directly without spinning a shell.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::shell::cursor_interp::color_from_pubkey;

/// A peer becomes "idle" this long after their last activity.
pub const IDLE_AFTER: Duration = Duration::from_secs(30);
/// A peer becomes "offline" this long after their last activity.
pub const OFFLINE_AFTER: Duration = Duration::from_secs(120);
/// Grey chips linger this long after going offline, then are pruned.
pub const PRUNE_AFTER: Duration = Duration::from_secs(15);

/// Live status of a peer in the roster.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PeerStatus {
    /// Currently active (activity within [`IDLE_AFTER`]).
    Active,
    /// Not actively producing input, but still connected.
    Idle,
    /// Disconnected or long silent — the chip greys out and starts to
    /// fade for [`PRUNE_AFTER`] before disappearing.
    Offline,
}

/// Rendered view of a single roster entry at some instant.
#[derive(Clone, Debug, PartialEq)]
pub struct RosterEntry {
    pub user_id: String,
    pub label: String,
    /// 2-letter initials derived from `label`.
    pub initials: String,
    pub color: [f32; 4],
    /// Human-readable last-seen string ("active now", "3s ago", …).
    pub last_seen_display: String,
    /// Numeric last-seen for testing / dense chrome tooltips.
    pub last_seen_secs: u64,
    pub status: PeerStatus,
    /// If the peer is in an app right now, the surface id we should
    /// focus when the chip is clicked. `None` disables click-to-jump.
    pub focus_target: Option<String>,
    /// Fade-out alpha for offline chips lingering in the prune window
    /// (1.0 while online, decays to 0.0 by the end of the prune window).
    pub alpha: f32,
}

/// Roster-store update pushed by the collab broker.
#[derive(Clone, Debug)]
pub enum RosterUpdate {
    /// Peer arrived / their metadata changed.
    Upsert {
        user_id: String,
        label: String,
        pubkey: Vec<u8>,
        focus_target: Option<String>,
        t: Instant,
    },
    /// Peer recorded activity (moved cursor, sent an op, etc.). Bumps
    /// their last-seen timestamp without changing metadata.
    Activity {
        user_id: String,
        t: Instant,
    },
    /// Peer disconnected. Chip goes grey immediately and is pruned
    /// after [`PRUNE_AFTER`].
    Disconnect {
        user_id: String,
        t: Instant,
    },
}

#[derive(Clone, Debug)]
struct PeerRecord {
    label: String,
    color: [f32; 4],
    focus_target: Option<String>,
    /// Wall-clock of last observed activity.
    last_seen: Instant,
    /// Wall-clock of an explicit disconnect note. `Some` forces the
    /// chip's status to `Offline` regardless of `last_seen`.
    disconnected_at: Option<Instant>,
}

/// In-memory roster. Fed by the shell's collab pump; consumed by the
/// roster widget.
#[derive(Default)]
pub struct RosterModel {
    peers: HashMap<String, PeerRecord>,
}

impl RosterModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, update: RosterUpdate) {
        match update {
            RosterUpdate::Upsert {
                user_id,
                label,
                pubkey,
                focus_target,
                t,
            } => {
                let color = color_from_pubkey(&pubkey);
                let record = self.peers.entry(user_id).or_insert(PeerRecord {
                    label: label.clone(),
                    color,
                    focus_target: focus_target.clone(),
                    last_seen: t,
                    disconnected_at: None,
                });
                record.label = label;
                record.color = color;
                record.focus_target = focus_target;
                record.last_seen = t;
                record.disconnected_at = None;
            }
            RosterUpdate::Activity { user_id, t } => {
                if let Some(record) = self.peers.get_mut(&user_id) {
                    record.last_seen = record.last_seen.max(t);
                    // Any activity clears a stale disconnect flag so
                    // reconnecting peers rejoin as green.
                    record.disconnected_at = None;
                }
            }
            RosterUpdate::Disconnect { user_id, t } => {
                if let Some(record) = self.peers.get_mut(&user_id) {
                    record.disconnected_at = Some(t);
                }
            }
        }
    }

    /// Render the roster at instant `now`, in a deterministic order
    /// (active peers first, then alphabetical by user_id inside each
    /// status bucket).
    pub fn render(&self, now: Instant) -> Vec<RosterEntry> {
        let mut out: Vec<RosterEntry> = Vec::with_capacity(self.peers.len());
        for (uid, r) in &self.peers {
            if let Some(entry) = render_one(uid, r, now) {
                out.push(entry);
            }
        }
        out.sort_by(|a, b| {
            status_rank(a.status)
                .cmp(&status_rank(b.status))
                .then_with(|| a.user_id.cmp(&b.user_id))
        });
        out
    }

    /// Drop peers whose offline prune window has fully elapsed. Callers
    /// can invoke this on a slow timer.
    pub fn gc(&mut self, now: Instant) {
        self.peers.retain(|_, r| {
            match r.disconnected_at {
                Some(t) => now.saturating_duration_since(t) < PRUNE_AFTER,
                None => now.saturating_duration_since(r.last_seen)
                    < OFFLINE_AFTER + PRUNE_AFTER,
            }
        });
    }

    /// Number of tracked peers (any status). Useful for tests +
    /// "3 peers here" chrome hints.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

fn status_rank(s: PeerStatus) -> u8 {
    match s {
        PeerStatus::Active => 0,
        PeerStatus::Idle => 1,
        PeerStatus::Offline => 2,
    }
}

fn render_one(uid: &str, r: &PeerRecord, now: Instant) -> Option<RosterEntry> {
    let (status, alpha) = classify(r, now)?;
    let age = now.saturating_duration_since(r.last_seen);
    let last_seen_secs = age.as_secs();
    Some(RosterEntry {
        user_id: uid.to_string(),
        label: r.label.clone(),
        initials: initials(&r.label, uid),
        color: [r.color[0], r.color[1], r.color[2], r.color[3] * alpha],
        last_seen_display: format_last_seen(age),
        last_seen_secs,
        status,
        focus_target: r.focus_target.clone(),
        alpha,
    })
}

fn classify(r: &PeerRecord, now: Instant) -> Option<(PeerStatus, f32)> {
    if let Some(dt) = r.disconnected_at {
        let elapsed = now.saturating_duration_since(dt);
        if elapsed >= PRUNE_AFTER {
            return None;
        }
        let alpha = 1.0 - elapsed.as_secs_f32() / PRUNE_AFTER.as_secs_f32();
        return Some((PeerStatus::Offline, alpha.clamp(0.0, 1.0)));
    }
    let age = now.saturating_duration_since(r.last_seen);
    if age < IDLE_AFTER {
        Some((PeerStatus::Active, 1.0))
    } else if age < OFFLINE_AFTER {
        Some((PeerStatus::Idle, 1.0))
    } else {
        let over = age - OFFLINE_AFTER;
        if over >= PRUNE_AFTER {
            return None;
        }
        let alpha = 1.0 - over.as_secs_f32() / PRUNE_AFTER.as_secs_f32();
        Some((PeerStatus::Offline, alpha.clamp(0.0, 1.0)))
    }
}

fn format_last_seen(age: Duration) -> String {
    let secs = age.as_secs();
    match secs {
        0..=5 => "active now".to_string(),
        6..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

/// Compute a 2-character initials chip fragment.
///
/// For labels with a space, we take the first letter of the first two
/// space-separated tokens ("Quinten Möller" → "QM"). Single-token
/// labels take the first two ASCII-alphanumeric characters. Empty
/// labels fall through to the first two characters of `user_id`
/// (uppercased) so the chip is never blank.
pub fn initials(label: &str, user_id: &str) -> String {
    let l = label.trim();
    let src: String = if l.is_empty() { user_id.to_string() } else { l.to_string() };
    if src.contains(char::is_whitespace) {
        let toks: Vec<&str> = src.split_whitespace().collect();
        let a = toks.first().and_then(|t| t.chars().next()).unwrap_or('?');
        let b = toks.get(1).and_then(|t| t.chars().next()).unwrap_or(a);
        return format!("{}{}", a.to_uppercase(), b.to_uppercase());
    }
    let mut chars = src.chars().filter(|c| c.is_ascii_alphanumeric());
    let a = chars.next().unwrap_or('?');
    let b = chars.next().unwrap_or(a);
    format!("{}{}", a.to_uppercase(), b.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upsert(uid: &str, label: &str, t: Instant) -> RosterUpdate {
        RosterUpdate::Upsert {
            user_id: uid.to_string(),
            label: label.to_string(),
            pubkey: vec![uid.as_bytes().first().copied().unwrap_or(0); 32],
            focus_target: None,
            t,
        }
    }

    #[test]
    fn empty_roster_renders_empty() {
        let m = RosterModel::new();
        assert!(m.render(Instant::now()).is_empty());
    }

    #[test]
    fn upsert_produces_active_chip() {
        let t0 = Instant::now();
        let mut m = RosterModel::new();
        m.apply(upsert("alice", "Alice Smith", t0));
        let r = m.render(t0);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].user_id, "alice");
        assert_eq!(r[0].label, "Alice Smith");
        assert_eq!(r[0].initials, "AS");
        assert_eq!(r[0].status, PeerStatus::Active);
        assert_eq!(r[0].last_seen_display, "active now");
    }

    #[test]
    fn peer_goes_idle_after_idle_after() {
        let t0 = Instant::now();
        let mut m = RosterModel::new();
        m.apply(upsert("alice", "Alice", t0));
        let r = m.render(t0 + IDLE_AFTER + Duration::from_millis(1));
        assert_eq!(r[0].status, PeerStatus::Idle);
    }

    #[test]
    fn peer_goes_offline_after_offline_after() {
        let t0 = Instant::now();
        let mut m = RosterModel::new();
        m.apply(upsert("alice", "Alice", t0));
        let r = m.render(t0 + OFFLINE_AFTER + Duration::from_millis(1));
        assert_eq!(r[0].status, PeerStatus::Offline);
    }

    #[test]
    fn disconnect_forces_offline_and_prune_fades() {
        let t0 = Instant::now();
        let mut m = RosterModel::new();
        m.apply(upsert("alice", "Alice", t0));
        m.apply(RosterUpdate::Disconnect {
            user_id: "alice".into(),
            t: t0,
        });
        let r = m.render(t0);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].status, PeerStatus::Offline);
        assert!(r[0].alpha > 0.99);
        // Halfway through the prune window: alpha ≈ 0.5.
        let r = m.render(t0 + PRUNE_AFTER / 2);
        assert!((r[0].alpha - 0.5).abs() < 0.1);
        // After the prune window: chip is gone.
        let r = m.render(t0 + PRUNE_AFTER + Duration::from_millis(1));
        assert!(r.is_empty());
    }

    #[test]
    fn gc_reclaims_pruned_peers() {
        let t0 = Instant::now();
        let mut m = RosterModel::new();
        m.apply(upsert("alice", "Alice", t0));
        m.apply(RosterUpdate::Disconnect {
            user_id: "alice".into(),
            t: t0,
        });
        assert_eq!(m.len(), 1);
        m.gc(t0 + PRUNE_AFTER + Duration::from_millis(1));
        assert_eq!(m.len(), 0);
    }

    #[test]
    fn activity_bumps_last_seen_and_reactivates() {
        let t0 = Instant::now();
        let mut m = RosterModel::new();
        m.apply(upsert("alice", "Alice", t0));
        // Sitting idle.
        assert_eq!(m.render(t0 + IDLE_AFTER + Duration::from_secs(1))[0].status, PeerStatus::Idle);
        // Fresh activity bumps them back to Active.
        m.apply(RosterUpdate::Activity {
            user_id: "alice".into(),
            t: t0 + IDLE_AFTER + Duration::from_secs(2),
        });
        let r = m.render(t0 + IDLE_AFTER + Duration::from_secs(2));
        assert_eq!(r[0].status, PeerStatus::Active);
    }

    #[test]
    fn render_order_active_then_idle_then_offline() {
        // t0 sits offset from now so we can shift the "current" time
        // forward without underflowing Instant on older peers.
        let base = Instant::now();
        let mut m = RosterModel::new();
        m.apply(upsert("z_active", "Z", base + OFFLINE_AFTER));
        m.apply(upsert("a_idle", "A", base + IDLE_AFTER));
        m.apply(upsert("m_offline", "M", base));
        let now = base + OFFLINE_AFTER;
        let r = m.render(now);
        // z_active first, then a_idle, then m_offline
        let ids: Vec<&str> = r.iter().map(|e| e.user_id.as_str()).collect();
        assert_eq!(ids, vec!["z_active", "a_idle", "m_offline"]);
    }

    #[test]
    fn initials_from_full_name() {
        assert_eq!(initials("Alice Smith", "alice"), "AS");
        assert_eq!(initials("quinten", "quinten"), "QU");
        assert_eq!(initials("", "abcdef"), "AB");
        assert_eq!(initials("Q", "quinten"), "QQ");
    }

    #[test]
    fn last_seen_display_thresholds() {
        assert_eq!(format_last_seen(Duration::from_secs(0)), "active now");
        assert_eq!(format_last_seen(Duration::from_secs(3)), "active now");
        assert_eq!(format_last_seen(Duration::from_secs(15)), "15s ago");
        assert_eq!(format_last_seen(Duration::from_secs(120)), "2m ago");
        assert_eq!(format_last_seen(Duration::from_secs(4000)), "1h ago");
    }
}
