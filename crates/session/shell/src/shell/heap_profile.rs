//! Where the compositor's live Rust heap was allocated, by call site.
//!
//! Builds with `hosted-test-control` install `pprof_alloc::PprofAlloc` as the
//! global allocator (`src/main.rs`). It hands every call straight to glibc
//! unless `PPROF_ALLOC_SAMPLE_RATE` (bytes) was set when the compositor
//! started; then it records the stack of one allocation per that many
//! allocated bytes on average, as Go's heap profiler does, and keeps it while
//! the allocation lives. [`report`] reads those samples: estimated live bytes
//! per stack, symbolized, the largest first, and per crate that asked for the
//! memory (the first frame outside the standard library and containers). `tilo-control raw '{"op":"heap_profile"}'` asks for it, with
//! `"path"` to also write the gzipped pprof profile for `pprof`.
//!
//! It sees what Rust code allocates; C libraries (lavapipe, LLVM, `GStreamer`)
//! call glibc's `malloc` directly, so glibc's in-use total
//! ([`crate::shell::memory_report`]) minus this profile's total is theirs.

use std::collections::BTreeMap;

use serde_json::{Value, json};

/// Stacks listed one by one, the most live bytes first.
const TOP_STACKS: usize = 30;
/// Frames shown of each listed stack, after the allocator's own.
const FRAMES_SHOWN: usize = 8;

/// The subset of pprof's `profile.proto` this report reads.
mod proto {
    #[derive(Clone, PartialEq, prost::Message)]
    pub(super) struct Profile {
        #[prost(message, repeated, tag = "1")]
        pub sample_type: Vec<ValueType>,
        #[prost(message, repeated, tag = "2")]
        pub sample: Vec<Sample>,
        #[prost(message, repeated, tag = "4")]
        pub location: Vec<Location>,
        #[prost(message, repeated, tag = "5")]
        pub function: Vec<Function>,
        #[prost(string, repeated, tag = "6")]
        pub string_table: Vec<String>,
        #[prost(int64, tag = "12")]
        pub period: i64,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub(super) struct ValueType {
        #[prost(int64, tag = "1")]
        pub r#type: i64,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub(super) struct Sample {
        #[prost(uint64, repeated, tag = "1")]
        pub location_id: Vec<u64>,
        #[prost(int64, repeated, tag = "2")]
        pub value: Vec<i64>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub(super) struct Location {
        #[prost(uint64, tag = "1")]
        pub id: u64,
        #[prost(message, repeated, tag = "4")]
        pub line: Vec<Line>,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub(super) struct Line {
        #[prost(uint64, tag = "1")]
        pub function_id: u64,
        #[prost(int64, tag = "2")]
        pub line: i64,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub(super) struct Function {
        #[prost(uint64, tag = "1")]
        pub id: u64,
        #[prost(int64, tag = "2")]
        pub name: i64,
        #[prost(int64, tag = "4")]
        pub filename: i64,
    }
}

/// One symbolized frame.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    function: String,
    file: String,
    line: i64,
}

/// Crates every stack passes through on its way to the allocator: the
/// standard library's, its hash table, the common containers and the
/// profiler. The first frame outside them is the code that asked.
const PLUMBING: [&str; 8] = [
    "alloc",
    "core",
    "std",
    "hashbrown",
    "allocator_api2",
    "smallvec",
    "indexmap",
    "pprof_alloc",
];

impl Frame {
    /// The crate whose code this frame is: from its symbol's path (`tilo_ui`
    /// for `<tilo_ui::offscreen::OffscreenRenderer>::read_target`, the
    /// trait's crate for a primitive's impl, `<str as alloc::borrow::ToOwned>`),
    /// or, for an inlined frame named without a path (`with_capacity_in<u8>`),
    /// from its source file.
    fn owner(&self) -> String {
        symbol_crate(&self.function).map_or_else(|| source_crate(&self.file), str::to_owned)
    }

    fn is_plumbing(&self) -> bool {
        let owner = self.owner();
        PLUMBING.contains(&owner.as_str()) || self.function.starts_with("__rust")
    }

    fn short(&self) -> String {
        let file = self.file.rsplit('/').next().unwrap_or(&self.file);
        if file.is_empty() {
            self.function.clone()
        } else {
            format!("{} ({file}:{})", self.function, self.line)
        }
    }
}

/// The crate a demangled symbol's path names (see [`Frame::owner`]); `None`
/// for a bare name.
fn symbol_crate(symbol: &str) -> Option<&str> {
    let symbol = symbol.trim_start_matches('<');
    if symbol.starts_with(['[', '&', '(', '*']) {
        return Some("core");
    }
    let end = symbol
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(symbol.len());
    let (first, rest) = symbol.split_at(end);
    if let Some(by_trait) = rest.strip_prefix(" as ") {
        // A primitive's impl, `<u8 as Trait>`: the trait's crate.
        return symbol_crate(by_trait);
    }
    (!first.is_empty() && rest.starts_with("::")).then_some(first)
}

/// The crate a source file belongs to, named as in symbols: `std` for the
/// toolchain's (and the crates it vendors), `fluent_syntax` for
/// `.../registry/src/<index>/fluent-syntax-0.12.0/src/...`, the directory's
/// name for the workspace's own.
fn source_crate(file: &str) -> String {
    if file.is_empty() {
        return "?".into();
    }
    if file.starts_with("/rustc/")
        || file.starts_with("/rust/deps/")
        || file.contains("/lib/rustlib/")
    {
        return "std".into();
    }
    let dir = ["/registry/src/", "/git/checkouts/"]
        .iter()
        .find_map(|marker| file.split_once(marker))
        // `<index>/<crate>-<version>/src/...`
        .and_then(|(_, rest)| rest.split('/').nth(1))
        .map(|versioned| {
            versioned
                .rsplit_once('-')
                .filter(|(_, version)| version.starts_with(|c: char| c.is_ascii_digit()))
                .map_or(versioned, |(name, _)| name)
        })
        .or_else(|| {
            file.rsplit_once("/src/")
                .map(|(dir, _)| dir.rsplit('/').next().unwrap_or(dir))
        })
        .unwrap_or(file);
    dir.replace('-', "_")
}

/// Live bytes and objects of one stack (or of a group of them).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Live {
    bytes: i64,
    objects: i64,
}

impl Live {
    fn add(&mut self, other: Self) {
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.objects = self.objects.saturating_add(other.objects);
    }
}

/// The decoded profile: per stack (leaf first, plumbing dropped), its live
/// estimate.
#[derive(Debug, Default)]
struct Stacks {
    sample_rate: i64,
    stacks: Vec<(Vec<Frame>, Live)>,
}

fn decode(gzipped: &[u8]) -> Result<Stacks, String> {
    use prost::Message as _;
    use std::io::Read as _;
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(gzipped)
        .read_to_end(&mut raw)
        .map_err(|e| format!("gunzip: {e}"))?;
    let profile = proto::Profile::decode(raw.as_slice()).map_err(|e| format!("decode: {e}"))?;
    let string = |index: i64| -> &str {
        usize::try_from(index)
            .ok()
            .and_then(|i| profile.string_table.get(i))
            .map_or("", String::as_str)
    };
    let column = |name: &str| {
        profile
            .sample_type
            .iter()
            .position(|t| string(t.r#type) == name)
    };
    let (Some(bytes_at), Some(objects_at)) = (column("inuse_space"), column("inuse_objects"))
    else {
        return Err("the profile has no inuse_space/inuse_objects".into());
    };
    let functions: BTreeMap<u64, &proto::Function> =
        profile.function.iter().map(|f| (f.id, f)).collect();
    let locations: BTreeMap<u64, Vec<Frame>> = profile
        .location
        .iter()
        .map(|location| {
            let frames = location
                .line
                .iter()
                .map(|line| {
                    let function = functions.get(&line.function_id);
                    Frame {
                        function: function.map_or("?", |f| string(f.name)).to_owned(),
                        file: function.map_or("", |f| string(f.filename)).to_owned(),
                        line: line.line,
                    }
                })
                .collect();
            (location.id, frames)
        })
        .collect();
    let mut stacks = Vec::new();
    for sample in &profile.sample {
        let live = Live {
            bytes: sample.value.get(bytes_at).copied().unwrap_or(0),
            objects: sample.value.get(objects_at).copied().unwrap_or(0),
        };
        if live.bytes <= 0 {
            continue;
        }
        let frames: Vec<Frame> = sample
            .location_id
            .iter()
            .filter_map(|id| locations.get(id))
            .flatten()
            .skip_while(|frame| frame.is_plumbing())
            .cloned()
            .collect();
        stacks.push((frames, live));
    }
    Ok(Stacks {
        sample_rate: profile.period,
        stacks,
    })
}

/// Stacks equal in their shown frames merged, the largest first; and live
/// bytes per crate that asked for them (each stack's first frame).
fn summarize(stacks: &Stacks) -> Value {
    let mut by_stack: BTreeMap<Vec<String>, Live> = BTreeMap::new();
    let mut by_owner: BTreeMap<String, Live> = BTreeMap::new();
    let mut total = Live::default();
    for (frames, live) in &stacks.stacks {
        total.add(*live);
        let owner = frames.first().map_or_else(|| "?".to_owned(), Frame::owner);
        by_owner.entry(owner).or_default().add(*live);
        let shown: Vec<String> = frames.iter().take(FRAMES_SHOWN).map(Frame::short).collect();
        by_stack.entry(shown).or_default().add(*live);
    }
    let mut top: Vec<_> = by_stack.into_iter().collect();
    top.sort_by_key(|(_, live)| std::cmp::Reverse(live.bytes));
    let mut owners: Vec<_> = by_owner.into_iter().collect();
    owners.sort_by_key(|(_, live)| std::cmp::Reverse(live.bytes));
    json!({
        "sample_rate": stacks.sample_rate,
        "live_kib": total.bytes / 1024,
        "live_objects": total.objects,
        "by_crate": owners
            .iter()
            .map(|(owner, live)| json!({"crate": owner, "kib": live.bytes / 1024, "objects": live.objects}))
            .collect::<Vec<_>>(),
        "top_stacks": top
            .iter()
            .take(TOP_STACKS)
            .map(|(frames, live)| json!({"kib": live.bytes / 1024, "objects": live.objects, "frames": frames}))
            .collect::<Vec<_>>(),
    })
}

/// The live heap by call site; with `path`, the raw profile is also written
/// there (gzipped pprof). An error names why there is no profile.
pub fn report(path: Option<&str>) -> Result<Value, String> {
    let rate = std::env::var(pprof_alloc::PPROF_SAMPLE_RATE_ENV).unwrap_or_default();
    if !rate.parse::<u64>().is_ok_and(|rate| rate > 0) {
        return Err(format!(
            "start the compositor with {}=<bytes> to sample its heap",
            pprof_alloc::PPROF_SAMPLE_RATE_ENV
        ));
    }
    let gzipped = pprof_alloc::generate_pprof().map_err(|e| format!("profile: {e}"))?;
    if let Some(path) = path {
        std::fs::write(path, &gzipped).map_err(|e| format!("write {path}: {e}"))?;
    }
    Ok(summarize(&decode(&gzipped)?))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test assertions")]

    use super::*;

    #[test]
    fn frames_are_owned_by_their_symbols_crate() {
        for (symbol, owner) in [
            (
                "<tilo_ui::offscreen::OffscreenRenderer>::read_target",
                Some("tilo_ui"),
            ),
            ("tilo_ui::i18n::make_bundle", Some("tilo_ui")),
            ("<str as alloc::borrow::ToOwned>::to_owned", Some("alloc")),
            (
                "<u8 as <[_]>::to_vec_in::ConvertVec>::to_vec::<alloc::alloc::Global>",
                Some("core"),
            ),
            (
                "<shell::compositor::backend::headless::HeadlessBackend as shell::compositor::backend::Backend>::submit_frame",
                Some("shell"),
            ),
            ("with_capacity_zeroed_in<u8, alloc::alloc::Global>", None),
            ("read_target", None),
        ] {
            assert_eq!(symbol_crate(symbol), owner, "{symbol}");
        }
    }

    #[test]
    fn inlined_frames_are_owned_by_their_source_files_crate() {
        let inlined = |file: &str| Frame {
            function: "with_capacity_in<u8>".into(),
            file: file.into(),
            line: 1,
        };
        for (file, owner) in [
            ("/rustc/abc/library/alloc/src/raw_vec/mod.rs", "std"),
            (
                "/home/u/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/fluent-syntax-0.12.0/src/parser/core.rs",
                "fluent_syntax",
            ),
            ("/src/crates/sdk/ui-core/src/offscreen.rs", "ui_core"),
            ("", "?"),
        ] {
            assert_eq!(inlined(file).owner(), owner, "{file}");
        }
        assert!(inlined("/rustc/abc/library/alloc/src/vec/mod.rs").is_plumbing());
    }

    #[test]
    fn stacks_are_merged_and_ranked_by_live_bytes() {
        let frame = |function: &str, file: &str| Frame {
            function: function.into(),
            file: file.into(),
            line: 1,
        };
        let stacks = Stacks {
            sample_rate: 4096,
            stacks: vec![
                (
                    vec![frame("a::grow", "lib.rs")],
                    Live {
                        bytes: 3 << 20,
                        objects: 3,
                    },
                ),
                (
                    vec![frame("b::keep", "lib.rs")],
                    Live {
                        bytes: 1 << 20,
                        objects: 1,
                    },
                ),
                (
                    vec![frame("a::grow", "lib.rs")],
                    Live {
                        bytes: 1 << 20,
                        objects: 1,
                    },
                ),
            ],
        };
        let summary = summarize(&stacks);
        assert_eq!(summary["live_kib"], 5 * 1024);
        assert_eq!(summary["by_crate"][0]["crate"], "a");
        assert_eq!(summary["by_crate"][0]["kib"], 4 * 1024);
        assert_eq!(summary["top_stacks"][0]["kib"], 4 * 1024);
        assert_eq!(summary["top_stacks"][1]["kib"], 1024);
    }

    #[test]
    fn a_profile_round_trips_through_pprof() {
        use prost::Message as _;
        use std::io::Write as _;
        let strings = [
            "",
            "inuse_objects",
            "inuse_space",
            "a::grow",
            "crates/a/src/lib.rs",
        ];
        let profile = proto::Profile {
            sample_type: vec![
                proto::ValueType { r#type: 1 },
                proto::ValueType { r#type: 2 },
            ],
            sample: vec![proto::Sample {
                location_id: vec![1],
                value: vec![2, 8192],
            }],
            location: vec![proto::Location {
                id: 1,
                line: vec![proto::Line {
                    function_id: 1,
                    line: 7,
                }],
            }],
            function: vec![proto::Function {
                id: 1,
                name: 3,
                filename: 4,
            }],
            string_table: strings.iter().map(|s| (*s).to_owned()).collect(),
            period: 4096,
        };
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&profile.encode_to_vec()).unwrap();
        let stacks = decode(&gz.finish().unwrap()).unwrap();
        assert_eq!(stacks.sample_rate, 4096);
        assert_eq!(stacks.stacks.len(), 1);
        assert_eq!(stacks.stacks[0].0[0].function, "a::grow");
        assert_eq!(
            stacks.stacks[0].1,
            Live {
                bytes: 8192,
                objects: 2
            }
        );
    }
}
