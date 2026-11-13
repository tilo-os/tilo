//! What a hosted VM used, as records an account's bill is made from: a VM is billed
//! per second while its hypervisor runs, a held snapshot by size and time. Times
//! are wall-clock milliseconds, and whole seconds are billed with the remainder
//! carried to the next run ([`Ledger`] keeps the records, [`summarize`] sums them;
//! nothing here knows prices).

use std::fs::{File, OpenOptions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::size::VmSize;
use super::valid_name;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmIdentity {
    /// `<workspace>/<environment>`: one VM per hosted workspace environment.
    pub vm: String,
    pub workspace: String,
    pub environment: String,
    pub account: String,
    pub size: VmSize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageKind {
    Running,
    Snapshot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunEnd {
    Suspended,
    Stopped,
    Crashed,
    /// The service stopped while it ran; closed at the last moment it was seen alive.
    Recovered,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub kind: UsageKind,
    #[serde(flatten)]
    pub identity: VmIdentity,
    pub start_ms: u64,
    pub end_ms: u64,
    pub millis: u64,
    /// Whole seconds billed, with the remainder carried to the next run.
    pub seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<RunEnd>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct OpenRun {
    pub start_ms: u64,
    pub last_seen_ms: u64,
    /// When the first Linux action of this run came: the run is billed from then. A run that only
    /// held the Browser is not billed, as its page actions are counted instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billed_from_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HeldSnapshot {
    pub start_ms: u64,
    pub bytes: u64,
    /// What of a held snapshot's time is not yet billed, in milliseconds.
    #[serde(default)]
    pub carry_ms: u64,
}

/// One VM's meter. Saved beside the VM after every change, so a restarted service carries on where
/// it was.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Meter {
    pub identity: VmIdentity,
    #[serde(default)]
    pub open: Option<OpenRun>,
    #[serde(default)]
    pub snapshot: Option<HeldSnapshot>,
    /// Running milliseconds not yet billed as a whole second.
    #[serde(default)]
    pub carry_ms: u64,
    /// Snapshot milliseconds not yet billed as a whole second.
    #[serde(default)]
    pub snapshot_carry_ms: u64,
}

impl Meter {
    #[must_use]
    pub const fn new(identity: VmIdentity) -> Self {
        Self {
            identity,
            open: None,
            snapshot: None,
            carry_ms: 0,
            snapshot_carry_ms: 0,
        }
    }

    #[must_use]
    pub const fn running(&self) -> bool {
        self.open.is_some()
    }

    /// The hypervisor was started at `now`, for a Linux action when `billed`. A run already open
    /// stays open: starting twice is still one run.
    pub fn started(&mut self, now: u64, billed: bool) {
        if self.open.is_none() {
            self.open = Some(OpenRun {
                start_ms: now,
                last_seen_ms: now,
                billed_from_ms: billed.then_some(now),
            });
        }
    }

    /// A Linux action ran at `now`: the open run is billed from its first.
    pub fn billed(&mut self, now: u64) {
        if let Some(open) = self.open.as_mut() {
            open.billed_from_ms.get_or_insert(now);
        }
    }

    pub fn seen(&mut self, now: u64) {
        if let Some(open) = self.open.as_mut() {
            open.last_seen_ms = open.last_seen_ms.max(now);
        }
    }

    /// The hypervisor is gone since `now`. `None` when nothing billed ran.
    pub fn stopped(&mut self, now: u64, ended: RunEnd) -> Option<UsageRecord> {
        let open = self.open.take()?;
        let start = open.billed_from_ms?;
        Some(self.run_record(start, now.max(start), ended))
    }

    /// Close a run the service left open, at the last moment it was seen.
    pub fn recover(&mut self) -> Option<UsageRecord> {
        let open = self.open.take()?;
        let start = open.billed_from_ms?;
        Some(self.run_record(start, open.last_seen_ms.max(start), RunEnd::Recovered))
    }

    fn run_record(&mut self, start_ms: u64, end_ms: u64, ended: RunEnd) -> UsageRecord {
        let millis = end_ms.saturating_sub(start_ms);
        let total = self.carry_ms.saturating_add(millis);
        self.carry_ms = total % 1000;
        UsageRecord {
            kind: UsageKind::Running,
            identity: self.identity.clone(),
            start_ms,
            end_ms,
            millis,
            seconds: total / 1000,
            ended: Some(ended),
            snapshot_bytes: None,
        }
    }

    /// A snapshot of `bytes` was written at `now`. One that was held is replaced, and its record
    /// returned.
    pub fn snapshot_taken(&mut self, now: u64, bytes: u64) -> Option<UsageRecord> {
        let replaced = self.snapshot_dropped(now);
        self.snapshot = Some(HeldSnapshot {
            start_ms: now,
            bytes,
            carry_ms: 0,
        });
        replaced
    }

    pub fn snapshot_dropped(&mut self, now: u64) -> Option<UsageRecord> {
        let held = self.snapshot.take()?;
        let end_ms = now.max(held.start_ms);
        let millis = end_ms.saturating_sub(held.start_ms);
        let total = self.snapshot_carry_ms.saturating_add(millis);
        self.snapshot_carry_ms = total % 1000;
        Some(UsageRecord {
            kind: UsageKind::Snapshot,
            identity: self.identity.clone(),
            start_ms: held.start_ms,
            end_ms,
            millis,
            seconds: total / 1000,
            ended: None,
            snapshot_bytes: Some(held.bytes),
        })
    }

    pub fn load(path: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        tilo_files::native::atomic::write_creating_parent(path, &bytes)
    }
}

/// Every account's usage records, one file per account.
#[derive(Clone, Debug)]
pub struct Ledger {
    dir: PathBuf,
}

impl Ledger {
    #[must_use]
    pub const fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn file(&self, account: &str) -> std::io::Result<PathBuf> {
        if !valid_name(account) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{account:?} is not an account id"),
            ));
        }
        Ok(self.dir.join(format!("{account}.jsonl")))
    }

    /// Append `record` to its account's file and wait until it is on disk: a record is money.
    pub fn append(&self, record: &UsageRecord) -> std::io::Result<()> {
        let path = self.file(&record.identity.account)?;
        std::fs::create_dir_all(&self.dir)?;
        let mut line = serde_json::to_vec(record).map_err(std::io::Error::other)?;
        line.push(b'\n');
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(&line)?;
        file.sync_data()
    }

    /// The account's records, oldest first. A line that is not a record (a torn write at a crash)
    /// is skipped.
    pub fn records(&self, account: &str) -> std::io::Result<Vec<UsageRecord>> {
        let file = match File::open(self.file(account)?) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        Ok(BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str(&line).ok())
            .collect())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SizeUsage {
    pub size: VmSize,
    pub running_seconds: u64,
    /// Snapshot storage, in byte-seconds.
    pub snapshot_byte_seconds: u128,
}

impl SizeUsage {
    /// Snapshot storage in GB-days (10⁹ bytes held for 86 400 s).
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "a display figure; billing uses the exact byte-seconds"
    )]
    pub fn snapshot_gb_days(&self) -> f64 {
        self.snapshot_byte_seconds as f64 / 1e9 / 86_400.0
    }
}

/// What an account used per size, from the records that ended in `[from_ms, to_ms)`.
#[must_use]
pub fn summarize(records: &[UsageRecord], from_ms: u64, to_ms: u64) -> Vec<SizeUsage> {
    let mut by_size: std::collections::BTreeMap<VmSize, SizeUsage> =
        std::collections::BTreeMap::new();
    for record in records
        .iter()
        .filter(|record| (from_ms..to_ms).contains(&record.end_ms))
    {
        let usage = by_size
            .entry(record.identity.size)
            .or_insert_with(|| SizeUsage {
                size: record.identity.size,
                ..SizeUsage::default()
            });
        match record.kind {
            UsageKind::Running => {
                usage.running_seconds = usage.running_seconds.saturating_add(record.seconds);
            }
            UsageKind::Snapshot => {
                let held = u128::from(record.snapshot_bytes.unwrap_or(0))
                    .saturating_mul(u128::from(record.seconds));
                usage.snapshot_byte_seconds = usage.snapshot_byte_seconds.saturating_add(held);
            }
        }
    }
    by_size.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(size: VmSize) -> VmIdentity {
        VmIdentity {
            vm: "ws1/default".into(),
            workspace: "ws1".into(),
            environment: "default".into(),
            account: "acct-1".into(),
            size,
        }
    }

    #[test]
    fn runs_are_billed_per_second_across_suspends() {
        {
            let mut meter = Meter::new(identity(VmSize::Small));
            let mut seconds = Vec::new();
            for wake in 0..10_u64 {
                let start = 1_000_000 + wake * 60_000;
                meter.started(start, true);
                meter.started(start + 200, true);
                let record = meter.stopped(start + 1_500, RunEnd::Suspended).unwrap();
                assert_eq!((record.start_ms, record.millis), (start, 1_500));
                seconds.push(record.seconds);
            }
            assert_eq!(seconds, [1, 2, 1, 2, 1, 2, 1, 2, 1, 2]);
            assert_eq!(seconds.iter().sum::<u64>(), 15);
            assert_eq!(meter.carry_ms, 0);
            assert_eq!(
                meter.stopped(2_000_000, RunEnd::Stopped),
                None,
                "nothing ran"
            );
        }
        {
            let mut meter = Meter::new(identity(VmSize::Default));
            meter.started(10_000, true);
            meter.seen(14_000);
            meter.seen(13_000);
            let saved = serde_json::to_vec(&meter).unwrap();
            let mut restarted: Meter = serde_json::from_slice(&saved).unwrap();
            let record = restarted.recover().unwrap();
            assert_eq!(
                (record.end_ms, record.seconds, record.ended),
                (14_000, 4, Some(RunEnd::Recovered))
            );
            assert!(!restarted.running());
        }
        {
            let mut meter = Meter::new(identity(VmSize::Small));
            meter.started(50_000, true);
            let record = meter.stopped(49_000, RunEnd::Crashed).unwrap();
            assert_eq!((record.millis, record.seconds), (0, 0));
        }
    }

    #[test]
    fn a_run_is_billed_from_its_first_linux_action_and_a_browser_alone_is_not() {
        let mut meter = Meter::new(identity(VmSize::Small));
        meter.started(10_000, false);
        meter.seen(70_000);
        assert_eq!(
            meter.stopped(70_000, RunEnd::Suspended),
            None,
            "a VM that only held Browser bills no time: its page actions are counted"
        );
        meter.started(100_000, false);
        meter.billed(130_000);
        meter.billed(140_000);
        let record = meter.stopped(190_000, RunEnd::Suspended).unwrap();
        assert_eq!(
            (record.start_ms, record.seconds),
            (130_000, 60),
            "from the first Linux action until it slept"
        );
    }

    #[test]
    fn snapshots_are_counted_apart_from_running_time() {
        let mut meter = Meter::new(identity(VmSize::Large));
        assert_eq!(meter.snapshot_taken(0, 2_000_000_000), None);
        let replaced = meter.snapshot_taken(86_400_000, 1_000_000_000).unwrap();
        assert_eq!(replaced.kind, UsageKind::Snapshot);
        assert_eq!(
            (replaced.seconds, replaced.snapshot_bytes),
            (86_400, Some(2_000_000_000))
        );
        let dropped = meter.snapshot_dropped(86_400_000 + 43_200_000).unwrap();
        assert_eq!(dropped.seconds, 43_200);
        assert_eq!(meter.snapshot_dropped(0), None);

        meter.started(0, true);
        let run = meter.stopped(90_500, RunEnd::Suspended).unwrap();
        let usage = summarize(&[replaced, dropped, run], 0, u64::MAX);
        assert_eq!(usage.len(), 1);
        let large = usage.first().unwrap();
        assert_eq!(large.running_seconds, 90);
        assert!((large.snapshot_gb_days() - 2.5).abs() < 1e-9);
    }

    #[test]
    fn the_ledger_keeps_each_accounts_records_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::new(dir.path().join("usage"));
        let mut small = Meter::new(identity(VmSize::Small));
        let mut other = Meter::new(VmIdentity {
            account: "acct-2".into(),
            ..identity(VmSize::Xlarge)
        });
        small.started(0, true);
        other.started(0, true);
        ledger
            .append(&small.stopped(3_000, RunEnd::Suspended).unwrap())
            .unwrap();
        ledger
            .append(&other.stopped(5_000, RunEnd::Stopped).unwrap())
            .unwrap();
        small.started(10_000, true);
        ledger
            .append(&small.stopped(12_000, RunEnd::Suspended).unwrap())
            .unwrap();
        // A torn last line, as a crash mid-write leaves it.
        let path = dir.path().join("usage/acct-1.jsonl");
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"kind\":\"runn").unwrap();

        let records = ledger.records("acct-1").unwrap();
        assert_eq!(
            records.iter().map(|r| r.seconds).collect::<Vec<_>>(),
            [3, 2]
        );
        let line = std::fs::read_to_string(&path).unwrap();
        let first = line.lines().next().unwrap();
        for field in [
            "\"kind\":\"running\"",
            "\"vm\":\"ws1/default\"",
            "\"workspace\":\"ws1\"",
            "\"account\":\"acct-1\"",
            "\"size\":\"small\"",
            "\"seconds\":3",
            "\"ended\":\"suspended\"",
        ] {
            assert!(first.contains(field), "{field} missing from {first}");
        }
        assert_eq!(ledger.records("acct-2").unwrap().len(), 1);
        assert!(ledger.records("nobody").unwrap().is_empty());
        assert!(ledger.records("../etc/passwd").is_err());
        let usage = summarize(&records, 0, 11_000);
        assert_eq!(usage.first().map(|u| u.running_seconds), Some(3));
    }
}
