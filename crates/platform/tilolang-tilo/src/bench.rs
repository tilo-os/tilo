//! `tilolang bench <package>`: the runtime's share of the performance budgets
//! (docs/designs/app-language/native-apps.md section 4), measured headlessly.
//!
//! A room is seeded with N items in the package's root collection, and each scenario runs the
//! path a frame takes through the runtime: the change reaches the store, the views that read it
//! run again, and the patches for the host come out. Drawing is the host's share and is not
//! measured here. Each scenario prints one JSON record per item count in the format of
//! `tools/perf/record.py`.
//!
//! * `open`: a host over the seeded room loads the compiled module and opens the root view,
//!   to its first tree.
//! * `keystroke`: one character typed into the title of an item in the item's own window; the
//!   change goes through the executor into the room and back, and the list view and the item's
//!   view are brought up to date.
//! * `peer_edit`: another replica retitles an item; the change merges into the room and the
//!   list view is brought up to date.
//! * `scroll`: the list shows the next page of rows.
//! * `idle`: ten seconds with nothing happening: how often the process was woken, and how
//!   many views ran.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value as Json, json};
use tilo_actions::{ActionContext, ActionInvoker};
use tilo_resource::{Collection, Edit, EditOp, Id, Segment};
use tilolang::source::{compile_dirs, dependency_order};
use tilolang::{Compiled, SeededEntropy};
use tilolang_runtime::meta::Meta;
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{Decision, Principal, UiNode, ViewInstance};

use crate::action::{Authorship, DynAction, DynEdit};
use crate::host::Host;
use crate::record::{DynRecord, DynValue};
use crate::schema::{MergeKind, lookup};

type Doc = tilo_resource::Doc;

/// What to measure.
#[derive(Clone, Debug)]
pub struct BenchConfig {
    /// The package directory (`apps/tl/notes`).
    pub package: PathBuf,
    /// Item counts to seed.
    pub sizes: Vec<usize>,
    /// Samples per timed scenario.
    pub samples: usize,
    /// How long the idle scenario waits.
    pub idle: Duration,
    pub env: Env,
}

/// The environment fields of a record.
#[derive(Clone, Debug)]
pub struct Env {
    pub commit: String,
    pub virtualization: String,
    pub runner: String,
}

/// One result, in the format of `tools/perf/record.py`.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub kpi: String,
    pub unit: &'static str,
    pub samples: Vec<f64>,
    pub meta: Json,
}

impl Record {
    fn nearest_rank(ordered: &[f64], percentile: usize) -> f64 {
        let n = ordered.len();
        let rank = (n * percentile).div_ceil(100).max(1);
        ordered.get(rank - 1).copied().unwrap_or(0.0)
    }

    /// The value at the nearest-rank percentile `p` of the samples.
    #[must_use]
    pub fn percentile(&self, p: usize) -> f64 {
        let mut ordered = self.samples.clone();
        ordered.sort_by(f64::total_cmp);
        Self::nearest_rank(&ordered, p)
    }

    /// The record as one JSON line of `tools/perf/record.py`.
    #[must_use]
    pub fn to_json(&self, env: &Env) -> Json {
        let mut ordered = self.samples.clone();
        ordered.sort_by(f64::total_cmp);
        json!({
            "schema": 1,
            "kpi": self.kpi,
            "unit": self.unit,
            "n": ordered.len(),
            "p50": Self::nearest_rank(&ordered, 50),
            "p95": Self::nearest_rank(&ordered, 95),
            "p99": Self::nearest_rank(&ordered, 99),
            "max": ordered.last().copied().unwrap_or(0.0),
            "commit": env.commit,
            "profile": if cfg!(debug_assertions) { "dev" } else { "release" },
            "renderer": "none",
            "virtualization": env.virtualization,
            "runner": env.runner,
            "hardware": hardware(),
            "started_at": now_utc(),
            "meta": self.meta,
        })
    }
}

fn hardware() -> String {
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("model name"))
                .and_then(|line| line.split_once(':'))
                .map(|(_, name)| name.trim().to_owned())
        })
        .unwrap_or_else(|| std::env::consts::ARCH.to_owned());
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    format!("{cpu}, {cores} threads")
}

/// The time now as UTC ISO 8601, to the second.
fn now_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// The packages `dir` needs, compiled, the package itself last.
///
/// # Errors
/// A package does not read or compile.
pub fn compile(dir: &Path) -> Result<Vec<Compiled>, String> {
    let search: Vec<PathBuf> = dir.parent().map(Path::to_path_buf).into_iter().collect();
    let dirs = dependency_order(dir, &search)?;
    let built = compile_dirs(&dirs, &[], &mut SeededEntropy(1)).map_err(|e| e.to_string())?;
    Ok(built.into_iter().map(|b| b.compiled).collect())
}

/// The root type of a package, its list field and the list's item type.
struct Shape {
    app: String,
    root_ty: String,
    list_field: String,
    item_ty: String,
    /// The item's text fields, the first being its title.
    texts: Vec<String>,
}

fn shape_of(meta: &Meta) -> Result<Shape, String> {
    let root = meta.roots.first().ok_or("the package declares no root")?;
    let root_ty = meta
        .types
        .iter()
        .find(|t| &t.id == root || &t.name == root)
        .ok_or("the root type is not declared")?;
    let schema = lookup(&root_ty.id).ok_or("the root type is not registered")?;
    let list = schema
        .fields
        .iter()
        .find(|f| f.kind == MergeKind::Collection)
        .ok_or("the root type has no collection to fill")?;
    let item_ty = list
        .record
        .clone()
        .ok_or("the collection has no item type")?;
    let item = lookup(&item_ty).ok_or("the item type is not registered")?;
    let texts = item
        .fields
        .iter()
        .filter(|f| f.kind == MergeKind::Text)
        .map(|f| f.id.clone())
        .collect::<Vec<_>>();
    if texts.is_empty() {
        return Err("the item type has no text field to type into".into());
    }
    Ok(Shape {
        app: meta.app.clone(),
        root_ty: root_ty.id.clone(),
        list_field: list.id.clone(),
        item_ty,
        texts,
    })
}

/// The id the seeded item number `i` has.
fn item_id(i: usize) -> String {
    format!("seed-{i:06}")
}

/// The root record with `n` items, each with its number in every text field.
fn seeded_root(shape: &Shape, n: usize) -> Result<DynRecord, String> {
    let root_schema = lookup(&shape.root_ty).ok_or("unregistered root type")?;
    let item_schema = lookup(&shape.item_ty).ok_or("unregistered item type")?;
    let mut root = DynRecord::empty(root_schema).map_err(|e| e.to_string())?;
    let mut items = Collection::default();
    for i in 0..n {
        let mut item = DynRecord::empty(item_schema).map_err(|e| e.to_string())?;
        for (k, field) in shape.texts.iter().enumerate() {
            if let Some(DynValue::Text(text)) = item.get_mut(field) {
                *text = if k == 0 {
                    format!("Item {i}")
                } else {
                    format!("Text of item {i}")
                };
            }
        }
        items.push(Id::from_raw(item_id(i)), item);
    }
    match root.get_mut(&shape.list_field) {
        Some(DynValue::Collection(slot)) => *slot = items,
        _ => return Err("the list field is not a collection".into()),
    }
    Ok(root)
}

/// Commit `action` on `doc` through the executor, as a window's host does.
fn execute(doc: &mut Doc, action: &DynAction, who: &str) -> Result<(), String> {
    let context = ActionContext::for_resource(
        action.resources().first().copied().unwrap_or("room"),
        &action.kind,
        &action.kind,
    )
    .with_app("so.tilo.lang", "tilolang")
    .with_invoker(ActionInvoker::actor(who.to_owned()));
    tilo_actions::apply_local_with_context(
        doc,
        action,
        action.encode().map_err(|e| e.to_string())?,
        Some(who.as_bytes()),
        context,
    )
    .map_err(|e| e.to_string())?;
    doc.commit();
    Ok(())
}

/// A room seeded with `n` items: the record, ready for a host.
fn seeded_room(shape: &Shape, n: usize) -> Result<Doc, String> {
    let mut doc = Doc::new();
    let root = seeded_root(shape, n)?;
    let action = DynAction {
        app: shape.app.clone(),
        kind: format!("tl.{}.seed", shape.app),
        action_id: "$create".into(),
        args: Json::Null,
        authorship: Authorship::default(),
        edits: vec![DynEdit::Create {
            id: root_id(shape),
            record: tilo_resource::Model::to_json(&root),
        }],
    };
    execute(&mut doc, &action, "seed")?;
    Ok(doc)
}

fn root_id(shape: &Shape) -> String {
    format!("root-{}", shape.root_ty)
}

/// One machine's window onto the room: the record, the host and the open views.
struct Room {
    doc: Doc,
    host: Host,
    list: ViewInstance,
    phases: Phases,
}

/// Where a frame's time went, per sample.
#[derive(Default)]
struct Phases {
    /// The runtime's own transaction: the edit through the bound control.
    edit: Vec<f64>,
    /// The executor committing the runtime's action into the room record.
    execute: Vec<f64>,
    /// The store catching up with the record.
    sync: Vec<f64>,
    /// The views re-running what changed.
    poll: Vec<f64>,
}

impl Phases {
    fn take(&mut self) -> Json {
        let p50 = |v: &mut Vec<f64>| {
            let samples = std::mem::take(v);
            if samples.is_empty() {
                return Json::Null;
            }
            let record = Record {
                kpi: String::new(),
                unit: "ms",
                samples,
                meta: Json::Null,
            };
            json!(record.percentile(50))
        };
        json!({
            "edit_p50_ms": p50(&mut self.edit),
            "execute_p50_ms": p50(&mut self.execute),
            "sync_p50_ms": p50(&mut self.sync),
            "poll_p50_ms": p50(&mut self.poll),
        })
    }
}

impl Room {
    fn open(doc: Doc, packages: &[Compiled], app: &str) -> Result<Self, String> {
        let host = Host::new(&doc, Principal::new("bench"), |_, _| Decision::Allow)
            .map_err(|e| e.to_string())?;
        for compiled in packages {
            host.load_compiled(compiled).map_err(|e| e.to_string())?;
        }
        let mut list = host.open_main(app).map_err(|e| e.to_string())?;
        // A virtual list shows what fits, as the host does before its first frame.
        if let Some(key) = list_key(list.tree()) {
            for i in 0..PAGE {
                list.row(&key, i).map_err(|e| e.to_string())?;
            }
        }
        Ok(Self {
            doc,
            host,
            list,
            phases: Phases::default(),
        })
    }

    /// What a frame does after a change: commit what the runtime committed through the
    /// executor, bring the store up to date with the record, and run what went stale.
    fn frame(&mut self, others: &mut [&mut ViewInstance]) -> Result<usize, String> {
        let start = Instant::now();
        for committed in self.host.take_committed() {
            execute(&mut self.doc, &committed.action, "bench")?;
        }
        let executed = Instant::now();
        self.host.sync_from(&self.doc);
        let synced = Instant::now();
        let mut patches = self.list.poll_patches().map_err(|e| e.to_string())?.len();
        for view in others {
            patches += view.poll_patches().map_err(|e| e.to_string())?.len();
        }
        self.phases.execute.push(ms(executed - start));
        self.phases.sync.push(ms(synced - executed));
        self.phases.poll.push(ms(synced.elapsed()));
        Ok(patches)
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn size_name(n: usize) -> String {
    if n >= 1000 && n % 1000 == 0 {
        format!("{}k", n / 1000)
    } else {
        n.to_string()
    }
}

/// The key of the first input bound to `field` in `node`.
fn bound_input(node: &UiNode, field: &str) -> Option<String> {
    if let Some(binding) = &node.binding
        && binding.field().is_some_and(|(_, f)| f == field)
    {
        return Some(node.key.clone());
    }
    node.children.iter().find_map(|c| bound_input(c, field))
}

/// Rows a window shows at once, overscan included.
const PAGE: usize = 40;

/// Rows one scroll frame brings into view.
const SCROLL_STEP: usize = 3;

/// The key of the first `list` in `node`.
fn list_key(node: &UiNode) -> Option<String> {
    if node.kind == "list" {
        return Some(node.key.clone());
    }
    node.children.iter().find_map(list_key)
}

/// Context switches so far, voluntary and not: of the calling thread, and of every other
/// thread of this process.
fn context_switches() -> (u64, u64) {
    let switches = |status: &str| {
        status
            .lines()
            .filter(|l| l.contains("ctxt_switches"))
            .filter_map(|l| l.split(':').nth(1)?.trim().parse::<u64>().ok())
            .sum::<u64>()
    };
    let own = std::fs::read_to_string("/proc/thread-self/status")
        .map(|s| switches(&s))
        .unwrap_or(0);
    let all: u64 = std::fs::read_dir("/proc/self/task")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|task| std::fs::read_to_string(task.path().join("status")).ok())
        .map(|s| switches(&s))
        .sum();
    (own, all.saturating_sub(own))
}

/// Run every scenario at every size.
///
/// # Errors
/// The package does not compile or a scenario fails.
pub fn run(config: &BenchConfig, emit: &mut dyn FnMut(&Record)) -> Result<Vec<Record>, String> {
    let packages = compile(&config.package)?;
    let main = packages.last().ok_or("no package")?;
    for compiled in &packages {
        crate::meta::MetaExt::register_schemas(&compiled.meta);
    }
    let meta = main.meta.clone();
    let shape = shape_of(&meta)?;
    let short = meta.app.rsplit('.').next().unwrap_or(&meta.app).to_owned();
    let mut out = Recorder {
        records: Vec::new(),
        emit,
    };
    for &n in &config.sizes {
        let size = size_name(n);
        let kpi = |scenario: &str| format!("tilolang.{scenario}.{short}.{size}");
        let base = seeded_room(&shape, n)?;
        let meta_json = |extra: Json| {
            let mut m = json!({"items": n, "package": meta.app});
            if let (Some(m), Json::Object(extra)) = (m.as_object_mut(), extra) {
                m.extend(extra);
            }
            m
        };

        // open
        let mut samples = Vec::new();
        let mut nodes = 0;
        for _ in 0..config.samples {
            let doc = base.clone();
            let start = Instant::now();
            let room = Room::open(doc, &packages, &meta.app)?;
            samples.push(ms(start.elapsed()));
            nodes = room.list.tree().count();
        }
        out.push(Record {
            kpi: kpi("open"),
            unit: "ms",
            samples,
            meta: meta_json(json!({"nodes": nodes})),
        });

        // keystroke
        let mut room = Room::open(base.clone(), &packages, &meta.app)?;
        room.frame(&mut [])?;
        room.phases.take();
        // An item on screen: the middle of the list, or of the page a virtual list shows.
        let target_index = if list_key(room.list.tree()).is_some() {
            PAGE.min(n) / 2
        } else {
            n / 2
        };
        let root = ResourceRef::root(root_id(&shape), shape.root_ty.clone());
        let item = root.item(
            &shape.list_field,
            &item_id(target_index),
            shape.item_ty.clone(),
        );
        let mut editor = room
            .host
            .open_resource(&item, None)
            .map_err(|e| e.to_string())?;
        let title = shape.texts.first().cloned().unwrap_or_default();
        let input = bound_input(editor.tree(), &title)
            .ok_or("the item's view has no input bound to its title")?;
        let mut samples = Vec::new();
        let mut text = format!("Item {target_index}");
        for _ in 0..config.samples {
            text.push('x');
            let start = Instant::now();
            editor
                .edit(&input, &Value::text(text.clone()))
                .map_err(|e| e.to_string())?;
            room.phases.edit.push(ms(start.elapsed()));
            room.frame(&mut [&mut editor])?;
            samples.push(ms(start.elapsed()));
        }
        out.push(Record {
            kpi: kpi("keystroke"),
            unit: "ms",
            samples,
            meta: meta_json(room.phases.take()),
        });
        drop(editor);

        // peer edit
        let mut peer = room.doc.fork();
        let mut samples = Vec::new();
        for s in 0..config.samples {
            let edit = Edit::new(
                vec![
                    Segment::Field(shape.list_field.clone()),
                    Segment::Item(Id::from_raw(item_id(target_index))),
                    Segment::Field(title.clone()),
                ],
                EditOp::Replace {
                    text: format!("Peer title {s}"),
                },
            );
            let action = DynAction {
                app: shape.app.clone(),
                kind: format!("tl.{}.edit", shape.app),
                action_id: "$edit".into(),
                args: Json::Null,
                authorship: Authorship::default(),
                edits: vec![DynEdit::Edit {
                    id: root_id(&shape),
                    edit,
                }],
            };
            execute(&mut peer, &action, "peer")?;
            let start = Instant::now();
            room.doc.merge(&mut peer).map_err(|e| e.to_string())?;
            room.phases.edit.push(ms(start.elapsed()));
            room.frame(&mut [])?;
            samples.push(ms(start.elapsed()));
        }
        out.push(Record {
            kpi: kpi("peer_edit"),
            unit: "ms",
            samples,
            meta: meta_json(room.phases.take()),
        });

        // scroll: a virtual list shows the rows that came into view and forgets those that left
        if let Some(list) = list_key(room.list.tree()) {
            let len = room.list.list_len(&list).unwrap_or(0);
            let mut samples = Vec::new();
            let mut first = 0;
            for _ in 0..config.samples {
                if first + PAGE + SCROLL_STEP > len {
                    first = 0;
                }
                let start = Instant::now();
                let next = first + SCROLL_STEP;
                room.list.hide_rows(&list, next..next + PAGE);
                for i in next..(next + PAGE).min(len) {
                    room.list.row(&list, i).map_err(|e| e.to_string())?;
                }
                samples.push(ms(start.elapsed()));
                first = next;
            }
            let stats = room.list.stats();
            out.push(Record {
                kpi: kpi("scroll"),
                unit: "ms",
                samples,
                meta: meta_json(json!({
                    "rows_per_frame": SCROLL_STEP,
                    "shown": PAGE,
                    "boundaries": stats.boundaries,
                    "unrendered": stats.unrendered,
                })),
            });
        }

        // idle: after a frame, nothing runs until something changes
        room.frame(&mut [])?;
        let (own_before, others_before) = context_switches();
        let start = Instant::now();
        std::thread::sleep(config.idle);
        let (own_after, others_after) = context_switches();
        let secs = start.elapsed().as_secs_f64();
        // A frame the loop would run now does nothing: nothing changed.
        let patches = room.frame(&mut [])?;
        // The sleep itself switches the calling thread out once.
        let wakeups = own_after.saturating_sub(own_before).saturating_sub(1)
            + others_after.saturating_sub(others_before);
        out.push(Record {
            kpi: kpi("idle_wakeups"),
            unit: "count",
            samples: vec![wakeups as f64],
            meta: meta_json(json!({"seconds": secs, "patches": patches})),
        });
    }
    Ok(out.records)
}

struct Recorder<'a> {
    records: Vec<Record>,
    emit: &'a mut dyn FnMut(&Record),
}

impl Recorder<'_> {
    fn push(&mut self, record: Record) {
        (self.emit)(&record);
        self.records.push(record);
    }
}

/// `tilolang bench <package> [--sizes 10,1000,10000] [--samples N] [--idle SECONDS]`.
/// Prints one JSON record per line.
#[must_use]
pub fn main(args: &[String]) -> std::process::ExitCode {
    match parse(args).and_then(|config| {
        let env = config.env.clone();
        run(&config, &mut |record| println!("{}", record.to_json(&env)))?;
        Ok(())
    }) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilolang bench: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn parse(args: &[String]) -> Result<BenchConfig, String> {
    let mut config = BenchConfig {
        package: PathBuf::from("."),
        sizes: vec![10, 1000, 10_000],
        samples: 20,
        idle: Duration::from_secs(10),
        env: Env {
            commit: std::env::var("TILO_COMMIT").unwrap_or_else(|_| "unknown".into()),
            virtualization: "none".into(),
            runner: "local".into(),
        },
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--sizes" => {
                config.sizes = value()?
                    .split(',')
                    .map(|s| s.trim().parse::<usize>().map_err(|e| e.to_string()))
                    .collect::<Result<_, _>>()?;
            }
            "--samples" => config.samples = value()?.parse().map_err(|e| format!("{e}"))?,
            "--idle" => {
                config.idle =
                    Duration::from_secs_f64(value()?.parse().map_err(|e| format!("{e}"))?);
            }
            "--commit" => config.env.commit = value()?,
            "--runner" => config.env.runner = value()?,
            "--virtualization" => config.env.virtualization = value()?,
            other if other.starts_with("--") => return Err(format!("unknown option {other}")),
            dir => config.package = PathBuf::from(dir),
        }
    }
    Ok(config)
}
