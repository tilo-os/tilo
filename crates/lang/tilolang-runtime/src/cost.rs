//! What a package's units cost to run, in the language's own terms.
//!
//! A unit is one function the compiler exports for a declaration: a view, a row view, an
//! action, the requirement or the place an action keeps, a fn, the default of an
//! action's parameter, the argument a control computes from view state, a block. Every call
//! into a module is one unit running, and what it costs is counted where it is known: fuel by
//! the engine that meters it, the reads it made of the store by the store, and the offers it
//! published by the host. Counts only, never time: the same window costs the same on a loaded
//! machine, so a budget can hold a builder that is busy.
//!
//! Nothing is counted until [`crate::Runtime::record_costs`] turns it on.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::rc::Rc;

use tilolang::meta::Line;

use crate::Meta;

/// What a store read, in the counts a unit's work is measured in: values and entries it found
/// by walking the record, directories and pages it read from the indexes, and items a filter
/// was evaluated on one by one. Work that does not depend on the machine or its load.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reads {
    /// Of the record: resources found by walking it, field values, collections listed, and
    /// those entries.
    pub record: u64,
    /// Of the indexes: directories and pages read, and field values.
    pub index: u64,
    /// Items a filter was evaluated on one by one (a filter no column answers).
    pub evaluated: u64,
}

impl Reads {
    /// What this and `other` read.
    #[must_use]
    pub fn since(&self, earlier: &Self) -> Self {
        Self {
            record: self.record.saturating_sub(earlier.record),
            index: self.index.saturating_sub(earlier.index),
            evaluated: self.evaluated.saturating_sub(earlier.evaluated),
        }
    }

    fn add(&mut self, other: &Self) {
        self.record = self.record.saturating_add(other.record);
        self.index = self.index.saturating_add(other.index);
        self.evaluated = self.evaluated.saturating_add(other.evaluated);
    }

    /// Every read, of every kind.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.record
            .saturating_add(self.index)
            .saturating_add(self.evaluated)
    }
}

/// What kind of declaration a unit was lowered from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitKind {
    Action,
    /// An action's `require`: whether it can run on its subject now.
    Requirement,
    /// An action's `applies`: whether it can run on the item where it is offered.
    Applies,
    /// A view of a resource, or of a whole window.
    View,
    /// A row a view renders per item.
    RowView,
    Fn,
    /// The default of an action's parameter.
    Default,
    /// An argument a control computes from view state when its event fires.
    AtEvent,
    /// An agent block, or a function the metadata does not name.
    Block,
}

impl UnitKind {
    /// The word a profile and a budget call the unit by.
    #[must_use]
    pub const fn noun(self) -> &'static str {
        match self {
            Self::Action => "action",
            Self::Requirement => "requirement",
            Self::Applies => "applies",
            Self::View => "view",
            Self::RowView => "row",
            Self::Fn => "fn",
            Self::Default => "default",
            Self::AtEvent => "at the event",
            Self::Block => "block",
        }
    }
}

/// One function a package exports, as a profile names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    /// What it is called: its subject's type and its own name, then what kind of declaration
    /// it is (`Task.due_today requirement`, `Task.home view`, `notes.add fn`).
    pub name: String,
    pub kind: UnitKind,
    /// The app the declaration is written in.
    pub app: String,
    /// The file of the package its declaration is in, and the line in it; empty and 0 when it
    /// is written nowhere (a block).
    pub file: String,
    pub line: u32,
}

impl Unit {
    /// A unit no declaration names: a block, or a function loaded without its metadata.
    pub(crate) fn unnamed(export: &str, app: &str) -> Self {
        Self {
            name: format!("{export} {}", UnitKind::Block.noun()),
            kind: UnitKind::Block,
            app: app.to_owned(),
            file: String::new(),
            line: 0,
        }
    }

    /// Where it is written, as a profile prints it: `app.tl:120`, or just the line when the
    /// package was compiled from one source.
    #[must_use]
    pub fn written(&self) -> String {
        match (self.file.as_str(), self.line) {
            ("", 0) => String::new(),
            ("" | "/", line) => format!("{line}"),
            (file, line) => format!("{file}:{line}"),
        }
    }
}

/// What one unit cost, added up over every time it ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitCost {
    pub unit: Unit,
    /// Times it ran.
    pub calls: u64,
    /// Fuel it spent, the calls it made in turn included: what its work costs the person
    /// waiting for it.
    pub fuel: u64,
    /// What it read of the store, the calls it made included.
    pub reads: Reads,
    /// Offers it published: the actions each item it showed can run.
    pub publishes: u64,
}

impl Default for UnitCost {
    fn default() -> Self {
        Self {
            unit: Unit::unnamed("", ""),
            calls: 0,
            fuel: 0,
            reads: Reads::default(),
            publishes: 0,
        }
    }
}

/// What an interaction cost, by unit: the counts a budget is held to.
#[derive(Clone, Debug, Default)]
pub struct Costs {
    /// Every unit that ran, the one that cost the most fuel first.
    pub units: Vec<UnitCost>,
    /// Fuel every call spent, nested calls counted once.
    pub fuel: u64,
    /// Calls into the package's code: one per unit that ran.
    pub calls: u64,
    /// What the store read.
    pub reads: Reads,
    /// Offers published.
    pub publishes: u64,
}

impl Costs {
    /// The unit `name` call what it cost, `None` when it did not run.
    #[must_use]
    pub fn unit(&self, name: &str) -> Option<&UnitCost> {
        self.units.iter().find(|cost| cost.unit.name == name)
    }

    /// Times the unit `name` ran; 0 when it did not.
    #[must_use]
    pub fn calls(&self, name: &str) -> u64 {
        self.unit(name).map_or(0, |cost| cost.calls)
    }

    /// The table a profile prints and a failed budget shows: each unit, where it is written,
    /// how often it ran, the fuel it spent and what it read.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{:>10}  {:<44}  {:>10}  {:>8}  {:>8}  {:>8}",
            "fuel", "unit", "written", "calls", "record", "index"
        );
        for cost in &self.units {
            let _ = write!(
                out,
                "{:>10}  {:<44}  {:>10}  {:>8}  {:>8}  {:>8}",
                cost.fuel,
                cost.unit.name,
                cost.unit.written(),
                cost.calls,
                cost.reads.record,
                cost.reads.index
            );
            if cost.reads.evaluated > 0 {
                let _ = write!(out, "  {:>8} evaluated", cost.reads.evaluated);
            }
            if cost.publishes > 0 {
                let _ = write!(out, "  {:>8} publishes", cost.publishes);
            }
            out.push('\n');
        }
        let _ = writeln!(
            out,
            "{:>10}  {:<44}  {:>10}  {:>8}  {:>8}  {:>8}",
            self.fuel, "everything", "", self.calls, self.reads.record, self.reads.index
        );
        out
    }
}

/// Hold an interaction's costs to what they may take. `hold` answers whether `costs` stay
/// within budget; it is the whole check, in the counts that do not depend on the machine
/// (`Costs::fuel`, `Costs::calls`, `Costs::reads`), so it can gate on a loaded builder. A
/// failure names the interaction, the units that cost the most, and the whole table.
///
/// # Panics
///
/// When `hold` says the costs are over budget.
#[track_caller]
pub fn assert_costs(interaction: &str, costs: &Costs, hold: impl FnOnce(&Costs) -> bool) {
    assert!(
        hold(costs),
        "`{interaction}` costs more than it may:\n{}",
        costs.render()
    );
}

/// One call into a package's code, still running.
struct Frame {
    unit: Rc<Unit>,
    /// Fuel its own instructions spent, the calls it made into other units excluded.
    fuel: u64,
    /// Fuel the calls it made spent.
    children: u64,
    /// What the store had read when it began: [leave](Recorder::leave) reads the difference,
    /// which counts the calls it made too.
    reads: Reads,
}

#[derive(Default)]
struct State {
    units: BTreeMap<String, UnitCost>,
    /// What the store had read when counting began: the reads a table totals are the ones
    /// since, so one interaction's table does not carry the ones before it.
    base: Reads,
    stack: Vec<Frame>,
    fuel: u64,
    calls: u64,
    publishes: u64,
}

/// What each unit costs, while a profile is on: shared by the runtime that names the units and
/// the engine that measures them.
#[derive(Default)]
pub(crate) struct Recorder {
    on: Cell<bool>,
    state: RefCell<State>,
}

impl Recorder {
    /// Whether anything is being counted: everything below it does nothing when not.
    pub(crate) fn on(&self) -> bool {
        self.on.get()
    }

    /// Start counting, or stop.
    pub(crate) fn record(&self, on: bool) {
        if self.on.replace(on) && !on {
            self.state.borrow_mut().stack.clear();
        }
    }

    /// Forget every count, keeping the profile on: what the store reads from `base` on is what
    /// is counted.
    pub(crate) fn reset(&self, base: Reads) {
        let mut state = self.state.borrow_mut();
        *state = State {
            base,
            ..State::default()
        };
    }

    /// A unit began running: its call is counted, and what it costs is charged to it until it
    /// [leaves](Self::leave).
    pub(crate) fn enter(&self, unit: Rc<Unit>, reads: Reads) {
        if !self.on() {
            return;
        }
        let mut state = self.state.borrow_mut();
        state.calls = state.calls.saturating_add(1);
        let cost = state
            .units
            .entry(unit.name.clone())
            .or_insert_with(|| UnitCost {
                unit: (*unit).clone(),
                ..UnitCost::default()
            });
        cost.calls = cost.calls.saturating_add(1);
        state.stack.push(Frame {
            unit,
            fuel: 0,
            children: 0,
            reads,
        });
    }

    /// Fuel the call now running spent.
    pub(crate) fn fuel(&self, used: u64) {
        if !self.on() {
            return;
        }
        let mut state = self.state.borrow_mut();
        state.fuel = state.fuel.saturating_add(used);
        if let Some(frame) = state.stack.last_mut() {
            frame.fuel = frame.fuel.saturating_add(used);
        }
    }

    /// An offer was published while the call now running ran.
    pub(crate) fn publish(&self, offers: u64) {
        if !self.on() || offers == 0 {
            return;
        }
        let mut state = self.state.borrow_mut();
        state.publishes = state.publishes.saturating_add(offers);
        if let Some(frame) = state.stack.last() {
            let name = frame.unit.name.clone();
            if let Some(cost) = state.units.get_mut(&name) {
                cost.publishes = cost.publishes.saturating_add(offers);
            }
        }
    }

    /// The call now running ended; the store has read `reads` so far.
    pub(crate) fn leave(&self, reads: Reads) {
        if !self.on() {
            return;
        }
        let mut state = self.state.borrow_mut();
        let Some(frame) = state.stack.pop() else {
            return;
        };
        let fuel = frame.fuel.saturating_add(frame.children);
        let read = reads.since(&frame.reads);
        if let Some(cost) = state.units.get_mut(&frame.unit.name) {
            cost.fuel = cost.fuel.saturating_add(fuel);
            cost.reads.add(&read);
        }
        if let Some(parent) = state.stack.last_mut() {
            parent.children = parent.children.saturating_add(fuel);
        }
    }

    /// What has been counted, against what the store has read so far.
    pub(crate) fn costs(&self, reads: Reads) -> Costs {
        let state = self.state.borrow();
        let reads = reads.since(&state.base);
        let mut units: Vec<UnitCost> = state.units.values().cloned().collect();
        units.sort_by(|a, b| {
            b.fuel
                .cmp(&a.fuel)
                .then_with(|| b.reads.total().cmp(&a.reads.total()))
                .then_with(|| b.calls.cmp(&a.calls))
                .then_with(|| a.unit.name.cmp(&b.unit.name))
        });
        Costs {
            units,
            fuel: state.fuel,
            calls: state.calls,
            reads,
            publishes: state.publishes,
        }
    }
}

/// The units `meta` declares, each by the export the module runs it as, named as a profile
/// names it and placed at the line its declaration is written on (`lines`).
#[must_use]
pub fn units_of(meta: &Meta, lines: &[Line]) -> BTreeMap<String, Rc<Unit>> {
    let mut units: BTreeMap<String, Rc<Unit>> = BTreeMap::new();
    for action in &meta.actions {
        action_units(&mut units, meta, lines, action);
    }
    for view in &meta.views {
        let subject = view.params.first().map(|p| short_ty(&p.ty));
        let kind = if view.row_of.is_some() {
            UnitKind::RowView
        } else {
            UnitKind::View
        };
        add_unit(
            &mut units,
            meta,
            lines,
            format!("v_{}", view.id),
            kind,
            subject.as_deref(),
            &view.name,
        );
    }
    for function in &meta.fns {
        let subject = function.params.first().map(|p| short_ty(&p.ty));
        add_unit(
            &mut units,
            meta,
            lines,
            format!("n_{}", function.id),
            UnitKind::Fn,
            subject.as_deref(),
            &function.name,
        );
    }
    for event in &meta.at_event {
        add_unit(
            &mut units,
            meta,
            lines,
            tilolang::ir::at_event_export(&event.id),
            UnitKind::AtEvent,
            None,
            &event.name,
        );
    }
    units
}

/// One action's units: the action, its `require`, its `applies`, and the defaults of its
/// parameters the host computes.
fn action_units(
    units: &mut BTreeMap<String, Rc<Unit>>,
    meta: &Meta,
    lines: &[Line],
    action: &tilolang::meta::ActionMeta,
) {
    let subject = action.params.first().map(|p| short_ty(&p.ty));
    let subject = subject.as_deref();
    let name = if action.scenario {
        format!("{} scenario", action.name)
    } else {
        action.name.clone()
    };
    add_unit(
        units,
        meta,
        lines,
        format!("a_{}", action.id),
        UnitKind::Action,
        if action.scenario { None } else { subject },
        &name,
    );
    if action.guard {
        add_unit(
            units,
            meta,
            lines,
            format!("q_{}", action.id),
            UnitKind::Requirement,
            subject,
            &action.name,
        );
    }
    if action.applies {
        add_unit(
            units,
            meta,
            lines,
            format!("p_{}", action.id),
            UnitKind::Applies,
            subject,
            &action.name,
        );
    }
    for param in &action.params {
        if let Some(default) = &param.default
            && !default.export.is_empty()
        {
            add_unit(
                units,
                meta,
                lines,
                default.export.clone(),
                UnitKind::Default,
                subject,
                &format!("{} {}", action.name, param.name),
            );
        }
    }
}

/// Add the unit the export `export` runs as: named `subject.name kind`, placed at the line
/// `lines` has for it.
fn add_unit(
    units: &mut BTreeMap<String, Rc<Unit>>,
    meta: &Meta,
    lines: &[Line],
    export: String,
    kind: UnitKind,
    subject: Option<&str>,
    name: &str,
) {
    let named = match subject {
        Some(subject) => format!("{subject}.{name} {}", kind.noun()),
        None => format!("{name} {}", kind.noun()),
    };
    let (file, line) = lines
        .iter()
        .find(|line| line.export == export)
        .map_or((String::new(), 0), |line| (line.file.clone(), line.line));
    let unit = Unit {
        name: named,
        kind,
        app: meta.app.clone(),
        file,
        line,
    };
    units.insert(export, Rc::new(unit));
}

/// The type a parameter's `ty` names, without its app: `app#Task` is a `Task`.
fn short_ty(ty: &str) -> String {
    ty.rsplit('#').next().unwrap_or(ty).to_owned()
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("on", &self.on.get())
            .field("units", &self.state.borrow().units.len())
            .finish()
    }
}
