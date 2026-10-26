//! `tilolang bench <package>`: the runtime's share of the performance budgets
//! (docs/designs/app-language/native-apps.md section 4), measured headlessly.
//!
//! A room is seeded with N items in the package's root collection (or, for a root that refers to
//! other packages' roots, as a board does, in theirs), and each scenario runs the path a frame
//! takes through the runtime: the change reaches the store, the views that read it run again,
//! and the patches for the host come out. Drawing is the host's share and is not measured here.
//! Each scenario prints one JSON record per item count in the format of `tools/perf/record.py`.
//!
//! * `open`: a host over the seeded room loads the compiled module and opens the root view,
//!   to its first tree; `ready`, until search and publication have read the room.
//! * `keystroke`: one character typed where a person types: into the title of an item in the
//!   item's own window, or, when items have none, into the main window's first text input. The
//!   change goes through the executor into the room and back, and the views are brought up to
//!   date. A package with no text to type into (a counter) gets `press`: its main window's
//!   first button.
//! * `peer_edit`: another replica retitles an item, or bumps the root's counter; the change
//!   merges into the room and the main view is brought up to date.
//! * `scroll`: the list shows the next page of rows.
//! * `memory_anon`: an app process's anonymous memory with the room open, measured in a
//!   process of its own.
//! * `idle`: ten seconds with nothing happening: how often the process was woken, and how
//!   many views ran.
//! * `thread_open`, `thread_keystroke`, `thread_post`, `thread_scroll`: for a package whose
//!   items hold a collection of their own (a conversation's messages), one item holding N of
//!   them, in its own window ([`thread`]).

mod thread;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value as Json, json};
use tilo_actions::{ActionContext, ActionInvoker};
use tilo_automerge::ApplyIncoming as _;
use tilo_resource::{Collection, Edit, EditOp, Id, Segment};
use tilolang::source::{compile_dirs, dependency_order};
use tilolang::{Compiled, SeededEntropy};
use tilolang_runtime::meta::Meta;
use tilolang_runtime::value::{ResourceRef, Value};
use tilolang_runtime::{Decision, EngineProfile, Principal, Translation, UiNode, ViewInstance};

use crate::action::{Authorship, DynAction, DynEdit};
use crate::host::Host;
use crate::publish::{Planner, Publications};
use crate::record::{DynRecord, DynValue};
use crate::schema::{MergeKind, lookup};
use crate::search::SearchIndex;

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
    /// How the process's wasm engine translates modules.
    pub translation: Translation,
    /// Whether to count where the time of calls into wasm goes (it costs a clock read per
    /// import).
    pub profile: bool,
    /// How to measure memory in a process of its own; not measured without.
    pub memory: Option<Isolation>,
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
        let rank = ordered
            .len()
            .saturating_mul(percentile)
            .div_ceil(100)
            .max(1);
        ordered.get(rank.saturating_sub(1)).copied().unwrap_or(0.0)
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
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    chrono::DateTime::from_timestamp(secs, 0)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
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

/// What a package's root holds that the scenarios fill and edit.
struct Shape {
    app: String,
    root_ty: String,
    /// The root's collection, seeded with the items.
    list: Option<List>,
    /// The root's first counter.
    counter: Option<String>,
    /// The roots of other packages the root refers to, by the field that holds the reference:
    /// a board over notes and todos. Each is seeded too.
    links: Vec<(String, Shape)>,
}

/// A collection of a root and its items.
struct List {
    field: String,
    item_ty: String,
    /// The item's text fields, the first being its title.
    texts: Vec<String>,
    /// The item's own collection, when it has one: a conversation's messages.
    nested: Option<Nested>,
}

/// The collection an item holds.
struct Nested {
    field: String,
    item_ty: String,
}

impl Shape {
    /// The root and collection the edits go to: the root's own, or that of the first root it
    /// refers to that has one.
    fn subject(&self) -> Option<(&Self, &List)> {
        self.list.as_ref().map(|l| (self, l)).or_else(|| {
            self.links
                .iter()
                .find_map(|(_, s)| s.list.as_ref().map(|l| (s, l)))
        })
    }
}

/// The root type of `meta`, and its id.
fn root_type(meta: &Meta) -> Result<&tilolang_runtime::meta::TypeMeta, String> {
    let root = meta.roots.first().ok_or("the package declares no root")?;
    meta.types
        .iter()
        .find(|t| &t.id == root || &t.name == root)
        .ok_or_else(|| "the root type is not declared".to_owned())
}

/// The shape of `meta`'s root; `others` are the packages its root may refer to.
fn shape_of(meta: &Meta, others: &[&Meta]) -> Result<Shape, String> {
    let root_ty = root_type(meta)?;
    let schema = lookup(&root_ty.id).ok_or("the root type is not registered")?;
    let list = schema
        .fields
        .iter()
        .filter(|f| f.kind == MergeKind::Collection)
        .find_map(|f| {
            let item_ty = f.record.clone()?;
            let texts = lookup(&item_ty)?
                .fields
                .iter()
                .filter(|f| f.kind == MergeKind::Text)
                .map(|f| f.id.clone())
                .collect::<Vec<_>>();
            let nested = lookup(&item_ty)?
                .fields
                .iter()
                .find(|f| f.kind == MergeKind::Collection)
                .and_then(|f| {
                    Some(Nested {
                        field: f.id.clone(),
                        item_ty: f.record.clone()?,
                    })
                });
            (!texts.is_empty()).then(|| List {
                field: f.id.clone(),
                item_ty,
                texts,
                nested,
            })
        });
    let counter = schema
        .fields
        .iter()
        .find(|f| f.kind == MergeKind::Counter)
        .map(|f| f.id.clone());
    let mut links = Vec::new();
    for field in &root_ty.fields {
        let ty = field.ty.trim();
        let ty = ty
            .strip_prefix("Option<")
            .and_then(|t| t.strip_suffix('>'))
            .unwrap_or(ty);
        for other in others {
            if let Ok(theirs) = root_type(other)
                && ty == format!("{}.{}", other.app, theirs.name)
            {
                links.push((field.id.clone(), shape_of(other, &[])?));
            }
        }
    }
    if list.is_none() && counter.is_none() && links.is_empty() {
        return Err("the root has no collection, counter or other root to edit".into());
    }
    Ok(Shape {
        app: meta.app.clone(),
        root_ty: root_ty.id.clone(),
        list,
        counter,
        links,
    })
}

/// The id the seeded item number `i` has.
fn item_id(i: usize) -> String {
    format!("seed-{i:06}")
}

/// The root record with `n` items in its collection, each with its number in every text
/// field, and referring to the roots it links.
fn seeded_root(shape: &Shape, n: usize) -> Result<DynRecord, String> {
    let root_schema = lookup(&shape.root_ty).ok_or("unregistered root type")?;
    let mut root = DynRecord::empty(root_schema).map_err(|e| e.to_string())?;
    if let Some(list) = &shape.list {
        let item_schema = lookup(&list.item_ty).ok_or("unregistered item type")?;
        let mut items = Collection::default();
        for i in 0..n {
            let mut item = DynRecord::empty(item_schema).map_err(|e| e.to_string())?;
            for (k, field) in list.texts.iter().enumerate() {
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
        match root.get_mut(&list.field) {
            Some(DynValue::Collection(slot)) => *slot = items,
            _ => return Err("the list field is not a collection".into()),
        }
    }
    for (field, linked) in &shape.links {
        let target = ResourceRef::root(root_id(linked), linked.root_ty.clone());
        let value = tilolang_runtime::value::to_json(&Value::some(Value::resource(&target)))?;
        match root.get_mut(field) {
            Some(DynValue::Register(slot)) => *slot = value,
            _ => return Err(format!("the field {field} does not hold a reference")),
        }
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

/// Create the root of `shape`, seeded with `n` items, in `doc`: the roots it links first.
fn seed(doc: &mut Doc, shape: &Shape, n: usize) -> Result<(), String> {
    for (_, linked) in &shape.links {
        seed(doc, linked, n)?;
    }
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
    execute(doc, &action, "seed")
}

/// A room seeded with `n` items: the record, ready for a host.
fn seeded_room(shape: &Shape, n: usize) -> Result<Doc, String> {
    let mut doc = Doc::new();
    seed(&mut doc, shape, n)?;
    // A window opens a room it loads, from storage or a peer: the seeded record, saved and
    // loaded again.
    Doc::load(&doc.save()).map_err(|e| e.to_string())
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
    search: SearchIndex,
    published: Publications,
    planner: std::sync::Arc<dyn Planner>,
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
    /// Search reindexing what changed.
    search: Vec<f64>,
    /// Publication republishing what changed.
    publish: Vec<f64>,
    /// Time inside wasm, the host's imports excluded, and in the imports.
    wasm: Vec<f64>,
    imports: Vec<f64>,
    /// Time compiling wasm modules.
    compile: Vec<f64>,
}

impl Phases {
    /// What the engine profiled between `before` and `after`, if it profiles.
    fn engine(&mut self, before: Option<EngineProfile>, after: Option<EngineProfile>) {
        if let (Some(before), Some(after)) = (before, after) {
            let calls = after.in_calls.saturating_sub(before.in_calls);
            let imports = after.in_imports.saturating_sub(before.in_imports);
            self.wasm.push(ms(calls.saturating_sub(imports)));
            self.imports.push(ms(imports));
            self.compile
                .push(ms(after.compiling.saturating_sub(before.compiling)));
        }
    }
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
            "search_p50_ms": p50(&mut self.search),
            "publish_p50_ms": p50(&mut self.publish),
            "wasm_p50_ms": p50(&mut self.wasm),
            "imports_p50_ms": p50(&mut self.imports),
            "compile_p50_ms": p50(&mut self.compile),
        })
    }
}

impl Room {
    fn open(doc: Doc, packages: &[Compiled], app: &str, profile: bool) -> Result<Self, String> {
        let host = Host::new(&doc, Principal::new("bench"), |_, _| Decision::Allow)
            .map_err(|e| e.to_string())?;
        host.runtime().engine().profile(profile);
        // As a launcher does: from the modules built with Tilo, compiling no source.
        for compiled in packages {
            host.load_wasm(&compiled.wasm).map_err(|e| e.to_string())?;
        }
        let mut list = host.open_main(app).map_err(|e| e.to_string())?;
        // The window's list frame is a virtual list: it shows what fits, as the host does
        // before its first frame. A list inside content (a card's) is drawn whole.
        if let Some(key) = frame_list(list.tree()) {
            for i in 0..PAGE {
                list.row(&key, i).map_err(|e| e.to_string())?;
            }
        }
        let mut inside = Vec::new();
        content_lists(list.tree(), &mut inside);
        for key in inside {
            for i in 0..list.list_len(&key).unwrap_or(0) {
                list.row(&key, i).map_err(|e| e.to_string())?;
            }
        }
        // The first frame starts search and publication; they read the room off the frame.
        let indexing = Instant::now();
        let mut search = SearchIndex::new(&host);
        search.update(&host);
        let publishing = Instant::now();
        let planner: std::sync::Arc<dyn Planner> = std::sync::Arc::new(host.handle());
        let mut published = Publications::new(&host);
        published.update(&host, &planner);
        let mut phases = Phases::default();
        phases.search.push(ms(publishing.duration_since(indexing)));
        phases.publish.push(ms(publishing.elapsed()));
        let profiled = host.runtime().engine().profiled();
        phases.engine(profiled.map(|_| EngineProfile::default()), profiled);
        Ok(Self {
            doc,
            host,
            list,
            phases,
            search,
            published,
            planner,
        })
    }

    /// Wait for search and publication to have read the room, and take it in.
    fn settle(&mut self) {
        self.search.settle(&self.host);
        self.published.settle(&self.host, &self.planner);
    }

    /// What a frame does after a change: commit what the runtime committed through the
    /// executor, bring the store up to date with the record, and run what went stale.
    fn frame(&mut self, others: &mut [&mut ViewInstance]) -> Result<usize, String> {
        let profiled = self.host.runtime().engine().profiled();
        let start = Instant::now();
        for committed in self.host.take_committed() {
            execute(&mut self.doc, &committed.action, "bench")?;
        }
        let executed = Instant::now();
        self.host.sync_from(&self.doc);
        let synced = Instant::now();
        let mut patches = self.list.poll_patches().map_err(|e| e.to_string())?.len();
        for view in others {
            patches = patches.saturating_add(view.poll_patches().map_err(|e| e.to_string())?.len());
        }
        let polled = Instant::now();
        self.search.update(&self.host);
        let searched = Instant::now();
        self.published.update(&self.host, &self.planner);
        self.phases.execute.push(ms(executed.duration_since(start)));
        self.phases.sync.push(ms(synced.duration_since(executed)));
        self.phases.poll.push(ms(polled.duration_since(synced)));
        self.phases.search.push(ms(searched.duration_since(polled)));
        self.phases.publish.push(ms(searched.elapsed()));
        let after = self.host.runtime().engine().profiled();
        self.phases.engine(profiled, after);
        Ok(patches)
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn size_name(n: usize) -> String {
    if n >= 1000 && n.is_multiple_of(1000) {
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

/// The key of the window's list frame: the view's root when it is a `list`.
fn frame_list(tree: &UiNode) -> Option<String> {
    (tree.kind == "list").then(|| tree.key.clone())
}

/// The keys of the lists inside a view's content: every `list` but the frame.
fn content_lists(node: &UiNode, out: &mut Vec<String>) {
    for child in &node.children {
        if child.kind == "list" {
            out.push(child.key.clone());
        }
        content_lists(child, out);
    }
}

/// Voluntary context switches so far (each a sleep that ended in a wakeup): of the calling
/// thread, and of every other thread of this process.
fn context_switches() -> (u64, u64) {
    let switches = |status: &str| {
        status
            .lines()
            // A wakeup is a voluntary switch; the kernel preempting a running thread is not one.
            .filter(|l| l.starts_with("voluntary_ctxt_switches"))
            .filter_map(|l| l.split(':').nth(1)?.trim().parse::<u64>().ok())
            .sum::<u64>()
    };
    let own = std::fs::read_to_string("/proc/thread-self/status").map_or(0, |s| switches(&s));
    let all: u64 = std::fs::read_dir("/proc/self/task")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|task| std::fs::read_to_string(task.path().join("status")).ok())
        .map(|s| switches(&s))
        .sum();
    (own, all.saturating_sub(own))
}

/// One size of one package: what every scenario needs.
struct Sized<'a> {
    config: &'a BenchConfig,
    packages: &'a [Compiled],
    meta: &'a Meta,
    shape: &'a Shape,
    short: &'a str,
    n: usize,
    base: Doc,
}

impl Sized<'_> {
    fn record(&self, scenario: &str, unit: &'static str, samples: Vec<f64>, extra: Json) -> Record {
        let translation = match self.config.translation {
            Translation::Lazy => "lazy",
            Translation::Eager => "eager",
        };
        let mut meta =
            json!({"items": self.n, "package": self.meta.app, "translation": translation});
        if let (Some(m), Json::Object(extra)) = (meta.as_object_mut(), extra) {
            m.extend(extra.into_iter().filter(|(_, v)| !v.is_null()));
        }
        Record {
            kpi: format!("tilolang.{scenario}.{}.{}", self.short, size_name(self.n)),
            unit,
            samples,
            meta,
        }
    }

    fn open(&self) -> Result<Room, String> {
        Room::open(
            self.base.clone(),
            self.packages,
            &self.meta.app,
            self.config.profile,
        )
    }

    /// The item the edits go to: on screen, in the middle of the list or of the page a
    /// virtual list shows.
    fn target(&self, room: &Room) -> usize {
        if frame_list(room.list.tree()).is_some() {
            PAGE.min(self.n) / 2
        } else {
            self.n / 2
        }
    }
}

/// Run every scenario at every size.
///
/// # Errors
/// The package does not compile or a scenario fails.
pub fn run(config: &BenchConfig, emit: &mut dyn FnMut(&Record)) -> Result<Vec<Record>, String> {
    let translation = tilolang_runtime::set_translation(config.translation);
    if translation != config.translation {
        return Err(format!(
            "the process's engine already translates {translation:?}"
        ));
    }
    let compiling = Instant::now();
    let packages = compile(&config.package)?;
    let compiled_in = ms(compiling.elapsed());
    let main = packages.last().ok_or("no package")?;
    for compiled in &packages {
        crate::meta::MetaExt::register_schemas(&compiled.meta);
    }
    let meta = main.meta.clone();
    let others: Vec<&Meta> = packages
        .iter()
        .map(|p| &p.meta)
        .filter(|m| m.app != meta.app)
        .collect();
    let shape = shape_of(&meta, &others)?;
    // A root without a collection holds no items to count: one room, of none.
    let counts = if shape.subject().is_some() {
        config.sizes.clone()
    } else {
        vec![0]
    };
    let short = meta.app.rsplit('.').next().unwrap_or(&meta.app).to_owned();
    let mut out = Recorder {
        records: Vec::new(),
        emit,
    };
    // What a launcher that compiled the package's source would add to every open; one
    // that loads the module built with Tilo (`Host::load_wasm`, as `open` does) does not.
    out.push(Record {
        kpi: format!("tilolang.compile_source.{short}"),
        unit: "ms",
        samples: vec![compiled_in],
        meta: json!({"package": meta.app, "packages": packages.len()}),
    });
    for n in counts {
        let sized = Sized {
            config,
            packages: &packages,
            meta: &meta,
            shape: &shape,
            short: &short,
            n,
            base: seeded_room(&shape, n)?,
        };
        if let Some(isolation) = &config.memory {
            out.push(memory(&sized, isolation)?);
        }
        for record in open(&sized)? {
            out.push(record);
        }
        let mut room = sized.open()?;
        room.settle();
        for record in index_everything(&sized, &room) {
            out.push(record);
        }
        room.frame(&mut [])?;
        room.phases.take();
        if shape.subject().is_some() || text_input(&room.host, room.list.tree()).is_some() {
            out.push(keystroke(&sized, &mut room)?);
        } else {
            out.push(press(&sized, &mut room)?);
        }
        out.push(peer_edit(&sized, &mut room)?);
        if let Some(record) = scroll(&sized, &mut room)? {
            out.push(record);
        }
        out.push(idle(&sized, &mut room)?);
    }
    if let Some(list) = &shape.list
        && let Some(nested) = &list.nested
        && has_default_view(&packages, &list.item_ty)
    {
        for &n in &config.sizes {
            let sized = Sized {
                config,
                packages: &packages,
                meta: &meta,
                shape: &shape,
                short: &short,
                n,
                base: thread::seeded(&packages, &shape, list, nested, n)?,
            };
            for record in thread::run(&sized, list, nested)? {
                out.push(record);
            }
        }
    }
    Ok(out.records)
}

/// The process's anonymous memory, in MiB.
fn anon_mib() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kib = status
        .lines()
        .find_map(|l| l.strip_prefix("RssAnon:"))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse::<u32>()
        .ok()?;
    Some(f64::from(kib) / 1024.0)
}

/// The room a memory child loads, saved, and the package it opens it with.
const MEMORY_ROOM: &str = "TILOLANG_BENCH_MEMORY_ROOM";
const MEMORY_PACKAGE: &str = "TILOLANG_BENCH_MEMORY_PACKAGE";
/// How a memory child reports.
const MEMORY_LINE: &str = "tilolang-bench-memory-anon-mib=";

/// How the memory scenario gets a process of its own: this program, run again with these
/// arguments, does what [`memory_child`] says when it finds the room in its environment.
#[derive(Clone, Debug)]
pub struct Isolation {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// An app process's anonymous memory with a room of the size open: a process of its own (seeding
/// a room leaves the bench's own process with memory the allocator keeps) loads the saved room,
/// opens the main view and its first page, lets search and publication read the room, and runs a
/// frame. It loads the packages by compiling their source, which a launcher does not.
fn memory(sized: &Sized<'_>, isolation: &Isolation) -> Result<Record, String> {
    let room = std::env::temp_dir().join(format!(
        "tilolang-bench-{}-{}.room",
        std::process::id(),
        sized.n
    ));
    std::fs::write(&room, sized.base.clone().save()).map_err(|e| e.to_string())?;
    let output = std::process::Command::new(&isolation.program)
        .args(&isolation.args)
        .env(MEMORY_ROOM, &room)
        .env(MEMORY_PACKAGE, &sized.config.package)
        .output();
    let _ = std::fs::remove_file(&room);
    let output = output.map_err(|e| format!("the memory process: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mib = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix(MEMORY_LINE))
        .and_then(|v| v.parse::<f64>().ok())
        .ok_or_else(|| {
            format!(
                "the memory process reported nothing: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        })?;
    Ok(sized.record("memory_anon", "MiB", vec![mib], Json::Null))
}

/// In a process [`memory`] started: measure, print the result for it, and say so. `None` in any
/// other process.
#[must_use]
pub fn memory_child() -> Option<Result<(), String>> {
    let room = std::env::var_os(MEMORY_ROOM)?;
    let package = std::env::var_os(MEMORY_PACKAGE)?;
    Some(
        measure_memory(Path::new(&room), Path::new(&package)).map(|mib| {
            println!("{MEMORY_LINE}{mib}");
        }),
    )
}

fn measure_memory(room: &Path, package: &Path) -> Result<f64, String> {
    let packages = compile(package)?;
    for compiled in &packages {
        crate::meta::MetaExt::register_schemas(&compiled.meta);
    }
    let app = packages.last().ok_or("no package")?.meta.app.clone();
    let doc =
        Doc::load(&std::fs::read(room).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let mut room = Room::open(doc, &packages, &app, false)?;
    room.settle();
    room.frame(&mut [])?;
    anon_mib().ok_or_else(|| "the process's memory is not readable".to_owned())
}

/// Opening the package's main view on a room of the size, through the first frame; and until
/// search and publication, which read the room off the frame, have it all.
fn open(sized: &Sized<'_>) -> Result<[Record; 2], String> {
    let mut samples = Vec::new();
    let mut nodes = 0;
    let mut phases = Phases::default();
    let mut ready = Vec::new();
    for _ in 0..sized.config.samples {
        let start = Instant::now();
        let mut room = sized.open()?;
        samples.push(ms(start.elapsed()));
        room.settle();
        ready.push(ms(start.elapsed()));
        nodes = room.list.tree().count();
        phases.search.append(&mut room.phases.search);
        phases.publish.append(&mut room.phases.publish);
        phases.wasm.append(&mut room.phases.wasm);
        phases.imports.append(&mut room.phases.imports);
        phases.compile.append(&mut room.phases.compile);
    }
    let mut extra = phases.take();
    if let Json::Object(m) = &mut extra {
        m.insert("nodes".into(), json!(nodes));
    }
    Ok([
        sized.record("open", "ms", samples, extra),
        sized.record("ready", "ms", ready, Json::Null),
    ])
}

/// What the window did on every change before search and publication were incremental:
/// index and publish every record anew.
fn index_everything(sized: &Sized<'_>, room: &Room) -> [Record; 2] {
    let (mut search, mut publish) = (Vec::new(), Vec::new());
    for _ in 0..sized.config.samples.min(5) {
        let start = Instant::now();
        for (package, _, doc) in room.host.records() {
            if let Ok(record) = room.host.store().record(&doc) {
                std::hint::black_box(crate::search::hits(&package.meta, &record, &doc));
            }
        }
        search.push(ms(start.elapsed()));
        let start = Instant::now();
        for (package, type_name, doc) in room.host.records() {
            let label = room.host.label_of(&doc);
            if let Ok(record) = room.host.store().record(&doc) {
                let types = crate::publish::ActionTypes::new(&package.meta, &room.planner);
                std::hint::black_box(crate::publish::publish_tree(
                    &types, &record, &type_name, &doc, &label,
                ));
            }
        }
        publish.push(ms(start.elapsed()));
    }
    [
        sized.record("reindex_all", "ms", search, json!({})),
        sized.record("republish_all", "ms", publish, json!({})),
    ]
}

/// Where the keystrokes go.
enum Typing {
    /// The input bound to an item's title, in the item's own window.
    Item(Box<ViewInstance>, String),
    /// An input of the main window bound to text: where a person types in an app whose items
    /// have no window of their own (todo's entry for a new item).
    Main(String),
}

/// Whether a loaded package gives `ty` a default view.
fn has_default_view(packages: &[Compiled], ty: &str) -> bool {
    packages.iter().any(|p| {
        p.meta
            .types
            .iter()
            .filter(|t| t.id == ty || t.name == ty)
            .any(|t| p.meta.default_view(&t.id).is_some() || p.meta.default_view(&t.name).is_some())
    })
}

/// The key and text of the first input of `node` bound to a text field.
fn text_input(host: &Host, node: &UiNode) -> Option<(String, String)> {
    if let Some((target, field)) = node.binding.as_ref().and_then(|b| b.field())
        && let Ok(Value::Text(text)) = host.runtime().read(target, field)
    {
        return Some((node.key.clone(), text));
    }
    node.children.iter().find_map(|c| text_input(host, c))
}

fn typing(sized: &Sized<'_>, room: &Room) -> Result<(Typing, String), String> {
    if let Some((root, list)) = sized.shape.subject()
        && has_default_view(sized.packages, &list.item_ty)
    {
        let target = sized.target(room);
        let item = ResourceRef::root(root_id(root), root.root_ty.clone()).item(
            &list.field,
            &item_id(target),
            list.item_ty.clone(),
        );
        let editor = room
            .host
            .open_resource(&item, None)
            .map_err(|e| e.to_string())?;
        // A person types into the item's title where its window edits it (a note), else into
        // the window's first text input (a conversation's draft).
        let title = list.texts.first().cloned().unwrap_or_default();
        let (input, text) = match bound_input(editor.tree(), &title) {
            Some(input) => (input, format!("Item {target}")),
            None => text_input(&room.host, editor.tree())
                .ok_or("the item's window has no text input")?,
        };
        return Ok((Typing::Item(Box::new(editor), input), text));
    }
    let (key, text) = text_input(&room.host, room.list.tree())
        .ok_or("the main window has no text input, and the items no window of their own")?;
    Ok((Typing::Main(key), text))
}

/// Typing one letter a frame where a person types in the package: into the title of an item
/// in the item's own window, or else into the main window's first text input.
fn keystroke(sized: &Sized<'_>, room: &mut Room) -> Result<Record, String> {
    let (mut typing, mut text) = typing(sized, room)?;
    let mut samples = Vec::new();
    for _ in 0..sized.config.samples {
        text.push('x');
        let value = Value::text(text.clone());
        let start = Instant::now();
        match &mut typing {
            Typing::Item(editor, input) => {
                editor.edit(input, &value).map_err(|e| e.to_string())?;
                room.phases.edit.push(ms(start.elapsed()));
                room.frame(&mut [editor.as_mut()])?;
            }
            Typing::Main(input) => {
                room.list.edit(input, &value).map_err(|e| e.to_string())?;
                room.phases.edit.push(ms(start.elapsed()));
                room.frame(&mut [])?;
            }
        }
        samples.push(ms(start.elapsed()));
    }
    let mut extra = room.phases.take();
    if let Json::Object(m) = &mut extra {
        let into = match typing {
            Typing::Item(..) => "item window",
            Typing::Main(_) => "main window",
        };
        m.insert("into".into(), json!(into));
    }
    Ok(sized.record("keystroke", "ms", samples, extra))
}

/// Pressing the main window's first button, one press a frame.
fn press(sized: &Sized<'_>, room: &mut Room) -> Result<Record, String> {
    let button = first_of(room.list.tree(), "button").ok_or("the main window has no button")?;
    let mut samples = Vec::new();
    for _ in 0..sized.config.samples {
        let start = Instant::now();
        room.list.press(&button).map_err(|e| e.to_string())?;
        room.phases.edit.push(ms(start.elapsed()));
        if room.frame(&mut [])? == 0 {
            return Err(format!("pressing {button} changed nothing on screen"));
        }
        samples.push(ms(start.elapsed()));
    }
    Ok(sized.record("press", "ms", samples, room.phases.take()))
}

/// The key of the first node of `kind` in `node`.
fn first_of(node: &UiNode, kind: &str) -> Option<String> {
    if node.kind == kind {
        return Some(node.key.clone());
    }
    node.children.iter().find_map(|c| first_of(c, kind))
}

/// What a peer changes: the title of the item the edits go to, or else the root's counter; and
/// the root it is in.
fn peer_change<'a>(
    sized: &Sized<'a>,
    room: &Room,
    sample: usize,
) -> Result<(&'a Shape, Edit<DynRecord>), String> {
    let shape = sized.shape;
    if let Some((root, list)) = shape.subject() {
        let title = list.texts.first().cloned().unwrap_or_default();
        let edit = Edit::new(
            vec![
                Segment::Field(list.field.clone()),
                Segment::Item(Id::from_raw(item_id(sized.target(room)))),
                Segment::Field(title),
            ],
            EditOp::Replace {
                text: format!("Peer title {sample}"),
            },
        );
        return Ok((root, edit));
    }
    let counter = shape
        .counter
        .clone()
        .ok_or("the root has nothing a peer edits")?;
    let edit = Edit::new(vec![Segment::Field(counter)], EditOp::Add { by: 1 });
    Ok((shape, edit))
}

/// A peer changing what is on screen, its change merged into the room.
fn peer_edit(sized: &Sized<'_>, room: &mut Room) -> Result<Record, String> {
    let mut peer = room.doc.fork();
    let mut samples = Vec::new();
    for sample in 0..sized.config.samples {
        let (subject, edit) = peer_change(sized, room, sample)?;
        let action = DynAction {
            app: subject.app.clone(),
            kind: format!("tl.{}.edit", subject.app),
            action_id: "$edit".into(),
            args: Json::Null,
            authorship: Authorship::default(),
            edits: vec![DynEdit::Edit {
                id: root_id(subject),
                edit,
            }],
        };
        execute(&mut peer, &action, "peer")?;
        let start = Instant::now();
        room.doc
            .merge_incoming(&mut peer)
            .map_err(|e| e.to_string())?;
        room.phases.edit.push(ms(start.elapsed()));
        room.frame(&mut [])?;
        samples.push(ms(start.elapsed()));
    }
    Ok(sized.record("peer_edit", "ms", samples, room.phases.take()))
}

/// A virtual list scrolled a few rows a frame: it shows the rows that came into view and
/// forgets those that left. Only for a package whose main view is a `list` frame.
fn scroll(sized: &Sized<'_>, room: &mut Room) -> Result<Option<Record>, String> {
    let Some(list) = frame_list(room.list.tree()) else {
        return Ok(None);
    };
    let len = room.list.list_len(&list).unwrap_or(0);
    let mut samples = Vec::new();
    let mut first = 0usize;
    for _ in 0..sized.config.samples {
        let mut next = first.saturating_add(SCROLL_STEP);
        if next.saturating_add(PAGE) > len {
            next = 0;
        }
        let shown = next..next.saturating_add(PAGE).min(len);
        let start = Instant::now();
        room.list.hide_rows(&list, shown.clone());
        for i in shown {
            room.list.row(&list, i).map_err(|e| e.to_string())?;
        }
        samples.push(ms(start.elapsed()));
        first = next;
    }
    let stats = room.list.stats();
    Ok(Some(sized.record(
        "scroll",
        "ms",
        samples,
        json!({
            "rows_per_frame": SCROLL_STEP,
            "shown": PAGE,
            "boundaries": stats.boundaries,
            "unrendered": stats.unrendered,
        }),
    )))
}

/// The room left alone: after a frame, nothing runs until something changes.
fn idle(sized: &Sized<'_>, room: &mut Room) -> Result<Record, String> {
    room.frame(&mut [])?;
    let (own_before, others_before) = context_switches();
    let start = Instant::now();
    std::thread::sleep(sized.config.idle);
    let (own_after, others_after) = context_switches();
    let secs = start.elapsed().as_secs_f64();
    // A frame the loop would run now does nothing: nothing changed.
    let patches = room.frame(&mut [])?;
    // The sleep itself switches the calling thread out once.
    let wakeups = own_after
        .saturating_sub(own_before)
        .saturating_sub(1)
        .saturating_add(others_after.saturating_sub(others_before));
    let wakeups = u32::try_from(wakeups).map_or_else(|_| f64::from(u32::MAX), f64::from);
    Ok(sized.record(
        "idle_wakeups",
        "count",
        vec![wakeups],
        json!({"seconds": secs, "patches": patches}),
    ))
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
    if let Some(measured) = memory_child() {
        return match measured {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("tilolang bench: {error}");
                std::process::ExitCode::FAILURE
            }
        };
    }
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
        translation: Translation::default(),
        profile: false,
        memory: std::env::current_exe().ok().map(|program| Isolation {
            program,
            args: Vec::new(),
        }),
        env: Env {
            commit: std::env::var("TILO_COMMIT").unwrap_or_else(|_| "unknown".into()),
            virtualization: "none".into(),
            runner: "local".into(),
        },
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
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
            "--translation" => {
                config.translation = match value()?.as_str() {
                    "lazy" => Translation::Lazy,
                    "eager" => Translation::Eager,
                    other => return Err(format!("unknown translation {other}")),
                };
            }
            "--profile" => config.profile = true,
            "--commit" => config.env.commit = value()?,
            "--runner" => config.env.runner = value()?,
            "--virtualization" => config.env.virtualization = value()?,
            other if other.starts_with("--") => return Err(format!("unknown option {other}")),
            dir => config.package = PathBuf::from(dir),
        }
    }
    Ok(config)
}
