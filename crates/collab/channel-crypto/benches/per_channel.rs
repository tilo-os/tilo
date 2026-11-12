//! What the channels a workspace has cost it: a device joining (minting its
//! member key, its access request, the commit that admits it, and every other
//! member applying that commit), a member leaving, and a device's state file
//! per channel on disk.
//!
//! Three kinds of row: a group per channel — one MLS group per record route,
//! the cost a workspace paid before key scopes — a group per reader set (one
//! key scope carrying `n` units, see `README.md`, "Granularity: per reader
//! set"), and the content work a unit does under its scope's keys. A
//! membership change is the same operation as a device add at this layer —
//! one commit per group, each commit delivered to every member of that group
//! — so both are measured by the join.
//!
//! Bytes are the postcard-encoded governance frames and access requests that
//! leave and reach a device; a time is one whole pass over the channels of a
//! size, in memory (the path above a device's state file).
//!
//! Run with:
//!
//!     cargo bench -p channel-crypto --bench per_channel
//!
//! `--sizes 1,10,100,1000` and `--samples N` change what it measures. One CSV
//! line per scenario and size, on stdout.

#![expect(
    clippy::expect_used,
    reason = "a benchmark fixture must fail rather than measure an incomplete group"
)]
// A measurement is bookkeeping over the numbers it just took, not app logic:
// the indices are into the sample and member vectors the fixture just filled,
// and the arithmetic is over counts and times.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "see the note above"
)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use channel_crypto::{Access, ChannelPeer, VaultKey};
use collab_types::{
    ArtifactId, ChannelId, ContentRetention, Ed25519PublicKey, MlsAccessRequestV1,
    MlsGovernanceFrame,
};
use ed25519_dalek::SigningKey;

/// Existing members every channel of the fixture starts with: the devices the
/// channel's record route already names.
const MEMBERS: usize = 3;

/// One device of a workspace: its signing key and its state for one channel.
struct Device {
    key: SigningKey,
    peer: ChannelPeer,
}

impl Device {
    fn waiting(seed: u8, channel: ChannelId, vault: &VaultKey) -> Self {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let peer =
            ChannelPeer::waiting(None, channel, Box::new(key.clone()), vault).expect("waiting");
        Self { key, peer }
    }

    fn creating(seed: u8, channel: ChannelId, vault: &VaultKey) -> Self {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let peer =
            ChannelPeer::create_now(None, channel, Box::new(key.clone()), vault).expect("creating");
        Self { key, peer }
    }

    fn public(&self) -> Ed25519PublicKey {
        Ed25519PublicKey::from_bytes(self.key.verifying_key().to_bytes())
    }

    /// This device's access request for the channel, and its bytes on the wire.
    fn ask(&mut self, vault: &VaultKey) -> (MlsAccessRequestV1, usize) {
        let request = self
            .peer
            .access_request_now(1, false, vault)
            .expect("access request")
            .expect("a waiting device asks");
        let bytes = postcard::to_allocvec(&request)
            .expect("request bytes")
            .len();
        (request, bytes)
    }

    fn take(&mut self) -> Vec<MlsGovernanceFrame> {
        let frames = self.peer.outbox().to_vec();
        self.peer.published();
        frames
    }

    fn receive(
        &mut self,
        frame: &MlsGovernanceFrame,
        allowed: &BTreeSet<Ed25519PublicKey>,
        vault: &VaultKey,
    ) {
        self.peer
            .receive(frame.clone(), allowed, vault)
            .expect("frame applies");
    }
}

fn allowed(devices: &[&Device]) -> BTreeSet<Ed25519PublicKey> {
    devices.iter().map(|device| device.public()).collect()
}

/// One channel of the fixture.
struct Group {
    channel: ChannelId,
    members: Vec<Device>,
}

/// Every channel of the fixture holds `MEMBERS` devices, the first the one
/// that founded its group. The same devices stand in every channel, as one
/// person's devices do across a workspace's records.
fn groups(n: usize, vault: &VaultKey) -> Vec<Group> {
    let seeds: Vec<u8> = (0..MEMBERS)
        .map(|i| 11 + 17 * u8::try_from(i).unwrap_or(0))
        .collect();
    (0..n)
        .map(|_| {
            let channel = ChannelId::new();
            let mut members = vec![Device::creating(seeds[0], channel, vault)];
            for seed in &seeds[1..] {
                let mut joiner = Device::waiting(*seed, channel, vault);
                let (request, _) = joiner.ask(vault);
                admit(&mut members, &mut joiner, request, vault);
                members.push(joiner);
            }
            Group { channel, members }
        })
        .collect()
}

/// The founder admits `joiner` and every other member applies the commit.
fn admit(
    members: &mut [Device],
    joiner: &mut Device,
    request: MlsAccessRequestV1,
    vault: &VaultKey,
) {
    members[0].peer.learn_request(request).expect("learn");
    let allowed = {
        let mut names = allowed(&members.iter().collect::<Vec<_>>());
        names.insert(joiner.public());
        names
    };
    members[0]
        .peer
        .reconcile_now(&allowed, vault)
        .expect("admit");
    let frames = members[0].take();
    for frame in &frames {
        if frame.added().contains(&joiner.public()) {
            joiner.receive(frame, &allowed, vault);
        }
        for other in members.iter_mut().skip(1) {
            other.receive(frame, &allowed, vault);
        }
    }
}

/// A frame's bytes over the wire.
fn frame_bytes(frame: &MlsGovernanceFrame) -> usize {
    postcard::to_allocvec(frame).expect("frame bytes").len()
}

/// What one pass over every channel moved.
#[derive(Default)]
struct Totals {
    bytes: usize,
    frames: u64,
}

impl Totals {
    fn add(&mut self, bytes: usize, frames: u64) {
        self.bytes = self.bytes.saturating_add(bytes);
        self.frames = self.frames.saturating_add(frames);
    }
}

struct Sizes {
    sizes: Vec<usize>,
    samples: usize,
}

fn parse() -> Sizes {
    let mut sizes = vec![1, 10, 100, 1_000];
    let mut samples = 3;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().expect("a value follows the flag");
        match arg.as_str() {
            "--sizes" => {
                sizes = value()
                    .split(',')
                    .map(|s| s.trim().parse().expect("a size"))
                    .collect();
            }
            "--samples" => samples = value().parse().expect("a sample count"),
            "--bench" => {}
            other => eprintln!("per_channel: ignoring {other}"),
        }
    }
    Sizes { sizes, samples }
}

/// The median and the mean of one scenario's samples, in milliseconds.
fn summarise(mut samples: Vec<f64>) -> (f64, f64) {
    samples.sort_by(|a, b| a.partial_cmp(b).expect("a time"));
    let n = samples.len();
    let mean = samples.iter().sum::<f64>() / n as f64;
    let median = if n % 2 == 1 {
        samples[n / 2]
    } else {
        samples[n / 2 - 1].midpoint(samples[n / 2])
    };
    (median, mean)
}

/// Print one row of the table.
fn row(scenario: &str, n: usize, members: usize, samples: Vec<f64>, frames: u64, bytes: usize) {
    let (median, mean) = summarise(samples);
    println!(
        "{scenario},{n},{members},{median:.2},{mean:.2},{:.1},{frames},{bytes},{:.0}",
        median * 1_000.0 / n as f64,
        bytes as f64 / n as f64
    );
}

fn main() {
    // A test runner executes every target it lists; this one measures for
    // minutes. Only a benchmark run, which cargo marks with `--bench`, does.
    if !std::env::args().skip(1).any(|arg| arg == "--bench") {
        return;
    }
    let sizes = parse();
    let vault = VaultKey::new([7; 32]);
    println!(
        "scenario,channels,members,median_ms,mean_ms,per_channel_us,frames,bytes,bytes_per_channel"
    );

    for &n in &sizes.sizes {
        // A device joining a workspace of `n` channels: it mints a member key
        // and an access request per channel, a member of each admits it, and
        // every other member applies that commit.
        let mut samples = Vec::new();
        let mut totals = Totals::default();
        for _ in 0..sizes.samples {
            let mut groups = groups(n, &vault);
            let started = Instant::now();
            for group in &mut groups {
                // One device joins every channel of the workspace.
                let mut joiner = Device::waiting(0x7a, group.channel, &vault);
                let (request, request_bytes) = joiner.ask(&vault);
                totals.add(request_bytes, 1);
                let members = &mut group.members;
                members[0].peer.learn_request(request).expect("learn");
                let allowed = {
                    let mut names = allowed(&members.iter().collect::<Vec<_>>());
                    names.insert(joiner.public());
                    names
                };
                members[0]
                    .peer
                    .reconcile_now(&allowed, &vault)
                    .expect("admit");
                let frames = members[0].take();
                for frame in &frames {
                    totals.add(frame_bytes(frame), 1);
                    if frame.added().contains(&joiner.public()) {
                        joiner.receive(frame, &allowed, &vault);
                    }
                    for other in members.iter_mut().skip(1) {
                        other.receive(frame, &allowed, &vault);
                    }
                }
            }
            samples.push(ms(started.elapsed()));
        }
        row(
            "join",
            n,
            MEMBERS,
            samples,
            totals.frames / sizes.samples as u64,
            totals.bytes / sizes.samples,
        );

        // A member leaving: one commit per group, each survivor applying it.
        let mut samples = Vec::new();
        let mut totals = Totals::default();
        for _ in 0..sizes.samples {
            let mut groups = groups(n, &vault);
            let started = Instant::now();
            for group in &mut groups {
                let members = &mut group.members;
                let allowed = {
                    let names: Vec<&Device> = members
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| *index != 1)
                        .map(|(_, device)| device)
                        .collect();
                    allowed(&names)
                };
                members[0]
                    .peer
                    .reconcile_now(&allowed, &vault)
                    .expect("remove");
                let frames = members[0].take();
                for frame in &frames {
                    totals.add(frame_bytes(frame), 1);
                    for (index, survivor) in members.iter_mut().enumerate() {
                        if index != 1 {
                            survivor.receive(frame, &allowed, &vault);
                        }
                    }
                }
            }
            samples.push(ms(started.elapsed()));
        }
        row(
            "removal",
            n,
            MEMBERS,
            samples,
            totals.frames / sizes.samples as u64,
            totals.bytes / sizes.samples,
        );

        // A device's state file per channel, written when it first opens a
        // channel and read again when it next starts: the per-unit disk cost of
        // a channel on a device.
        let mut samples = Vec::new();
        let mut file_bytes = 0;
        for _ in 0..sizes.samples {
            let dir = tempdir();
            let started = Instant::now();
            for _ in 0..n {
                let channel = ChannelId::new();
                let key = SigningKey::from_bytes(&[0x41; 32]);
                let states = std::sync::Arc::new(channel_crypto::Files::new(dir.clone()));
                let peer = ChannelPeer::waiting(
                    Some(states.clone()),
                    channel,
                    Box::new(key.clone()),
                    &vault,
                )
                .expect("waiting");
                assert_eq!(peer.access(), Access::Waiting);
                let opened = ChannelPeer::open(Some(states), channel, Box::new(key), &vault)
                    .expect("open")
                    .expect("a state file");
                assert_eq!(opened.access(), Access::Waiting);
            }
            samples.push(ms(started.elapsed()));
            file_bytes = dir_bytes(&dir);
            let _ = std::fs::remove_dir_all(&dir);
        }

        row("state_file", n, 0, samples, 0, file_bytes);

        // A workspace whose `n` units share one reader set: one key scope, so a
        // device joining takes part once for the whole workspace, and a member
        // leaving costs one commit for all of it.
        let mut samples = Vec::new();
        let mut totals = Totals::default();
        for _ in 0..sizes.samples {
            let mut scope = scope_group(&vault);
            let started = Instant::now();
            let mut joiner = Device::waiting(0x7a, scope.channel, &vault);
            let (request, request_bytes) = joiner.ask(&vault);
            totals.add(request_bytes, 1);
            let members = &mut scope.members;
            members[0].peer.learn_request(request).expect("learn");
            let allowed = {
                let mut names = allowed(&members.iter().collect::<Vec<_>>());
                names.insert(joiner.public());
                names
            };
            members[0]
                .peer
                .reconcile_now(&allowed, &vault)
                .expect("admit");
            let frames = members[0].take();
            for frame in &frames {
                totals.add(frame_bytes(frame), 1);
                if frame.added().contains(&joiner.public()) {
                    joiner.receive(frame, &allowed, &vault);
                }
                for other in members.iter_mut().skip(1) {
                    other.receive(frame, &allowed, &vault);
                }
            }
            samples.push(ms(started.elapsed()));
        }
        row(
            "key_scope_join",
            n,
            MEMBERS,
            samples,
            totals.frames / sizes.samples as u64,
            totals.bytes / sizes.samples,
        );

        let mut samples = Vec::new();
        let mut totals = Totals::default();
        for _ in 0..sizes.samples {
            let mut scope = scope_group(&vault);
            let started = Instant::now();
            let members = &mut scope.members;
            let allowed = {
                let names: Vec<&Device> = members
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != 1)
                    .map(|(_, device)| device)
                    .collect();
                allowed(&names)
            };
            members[0]
                .peer
                .reconcile_now(&allowed, &vault)
                .expect("remove");
            let frames = members[0].take();
            for frame in &frames {
                totals.add(frame_bytes(frame), 1);
                for (index, survivor) in members.iter_mut().enumerate() {
                    if index != 1 {
                        survivor.receive(frame, &allowed, &vault);
                    }
                }
            }
            samples.push(ms(started.elapsed()));
        }
        row(
            "key_scope_removal",
            n,
            MEMBERS,
            samples,
            totals.frames / sizes.samples as u64,
            totals.bytes / sizes.samples,
        );

        // What one scope's keys carry: each of `n` units seals under them and
        // opens what it sealed, its content named for the unit. This work is
        // the same per unit with or without scopes; it is here so the join
        // rows above are not read as the whole cost of a unit.
        let scope = scope_group(&vault);
        let keys = scope
            .members
            .first()
            .and_then(|device| device.peer.content_keys())
            .expect("a founded scope has keys");
        let mut samples = Vec::new();
        for _ in 0..sizes.samples {
            let started = Instant::now();
            for _ in 0..n {
                let unit = ChannelId::new();
                let named = keys.for_unit(unit);
                let artifact = ArtifactId::new();
                let (metadata, sealed) = named
                    .seal_content(artifact, ContentRetention::Delta, b"a unit's content")
                    .expect("seal");
                named
                    .open_content(artifact, &metadata, &sealed)
                    .expect("open");
            }
            samples.push(ms(started.elapsed()));
        }
        row("unit_content", n, MEMBERS, samples, 0, 0);
    }
}

/// One key scope with `MEMBERS` devices: the reader set every unit of a
/// workspace shares.
fn scope_group(vault: &VaultKey) -> Group {
    let seeds: Vec<u8> = (0..MEMBERS)
        .map(|i| 11 + 17 * u8::try_from(i).unwrap_or(0))
        .collect();
    let channel = ChannelId::new();
    let mut members = vec![Device::creating(seeds[0], channel, vault)];
    for seed in &seeds[1..] {
        let mut joiner = Device::waiting(*seed, channel, vault);
        let (request, _) = joiner.ask(vault);
        admit(&mut members, &mut joiner, request, vault);
        members.push(joiner);
    }
    Group { channel, members }
}

fn ms(elapsed: Duration) -> f64 {
    elapsed.as_secs_f64() * 1_000.0
}

fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("channel-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn dir_bytes(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .map(|meta| usize::try_from(meta.len()).unwrap_or(0))
        .fold(0, usize::saturating_add)
}
