//! Effect classes and what each declaration can reach, computed from the IR facts.
//!
//! An action is *Read* if it changes nothing, *Outside* if it reaches outside Tilo (itself or
//! through an action it calls), and *Undoable* otherwise. For every action and view the
//! metadata lists the actions it
//! can reach, its own and other packages', each with its class. Granting, keeping and updating
//! code are decided against that list (authority.md).
//!
//! [`Effect`] is the other axis: what a body reads that a replay cannot compute again, which its
//! `uses` clause declares and its invocation records.

use crate::ir::{ActionRef, FuncKind, Module};
use crate::meta::Reach;
use crate::types::EffectClass;
use std::collections::{BTreeMap, BTreeSet};

/// What an action reads that a replay cannot compute again: what its `uses` clause
/// declares and what its invocation records (convergence.md, "Actions").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Effect {
    /// A service outside Tilo: a request and the answer it got.
    Web,
    /// A draw: a number, a choice, an order.
    Random,
    /// The clock and the person's zone: `now`, a day of a moment, a wall time of day.
    Time,
}

impl Effect {
    /// Every effect, in the order a message lists them.
    pub const ALL: &'static [Self] = &[Self::Web, Self::Random, Self::Time];

    pub const fn word(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Random => "random",
            Self::Time => "time",
        }
    }

    /// What reading it is, in a message: what a replay cannot compute again.
    pub const fn what(self) -> &'static str {
        match self {
            Self::Web => "a service outside Tilo",
            Self::Random => "a draw",
            Self::Time => "the clock and the person's zone",
        }
    }

    /// The effect a `uses` clause names, if it names one.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|e| e.word() == name)
    }
}

/// The effects `funcs` reach: each function's own, and those of the code it calls, iterated to
/// a fixed point. An action that calls another runs it as its own change, so its effects are
/// not this action's; what it calls is decided on its own (see [`analyze`]).
pub fn reached(funcs: &[crate::ir::Func]) -> Vec<BTreeSet<Effect>> {
    let mut out: Vec<BTreeSet<Effect>> = funcs.iter().map(|f| f.facts.effects.clone()).collect();
    loop {
        let mut changed = false;
        for (i, f) in funcs.iter().enumerate() {
            let callees = f.facts.calls.iter();
            for callee in callees {
                let Some(there) = out.get(*callee).cloned() else {
                    continue;
                };
                for e in there {
                    if let Some(slot) = out.get_mut(i)
                        && slot.insert(e)
                    {
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            return out;
        }
    }
}

/// The result of the analysis, indexed like [`Module::funcs`].
pub struct Effects {
    /// Class of each action; `Read` for the rest.
    pub class: Vec<EffectClass>,
    /// Every action each function can reach.
    pub reaches: Vec<Vec<Reach>>,
}

type ReachMap = BTreeMap<(String, String), Reach>;

/// The id of the declaration behind a function export (`a_<id>` and so on).
fn func_id(export: &str) -> &str {
    export.get(2..).unwrap_or(export)
}

pub fn analyze(m: &Module, app: &str) -> Effects {
    let n = m.funcs.len();
    let mut class = vec![EffectClass::Read; n];
    loop {
        let mut changed = false;
        for (i, f) in m.funcs.iter().enumerate() {
            if f.kind != FuncKind::Action {
                continue;
            }
            let mut c = if f.facts.outside {
                EffectClass::Outside
            } else if f.facts.mutates {
                EffectClass::Undoable
            } else {
                EffectClass::Read
            };
            // An action reaches what it calls: the invocations are what its change makes of
            // them, so its class is at least theirs.
            for a in &f.facts.invokes {
                c = c.max(ref_class(a, &class));
            }
            if class.get(i).copied() != Some(c) {
                if let Some(slot) = class.get_mut(i) {
                    *slot = c;
                }
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut reaches: Vec<ReachMap> = vec![ReachMap::new(); n];
    // Actions reach themselves and what they call; views reach what their buttons apply and
    // what the views they call reach. Iterate to a fixed point.
    loop {
        let mut changed = false;
        for (i, f) in m.funcs.iter().enumerate() {
            let mut add: Vec<Reach> = Vec::new();
            match f.kind {
                FuncKind::Action => {
                    add.push(entry_of_func(
                        app,
                        f.name.as_str(),
                        &f.export,
                        class.get(i).copied().unwrap_or(EffectClass::Read),
                    ));
                    // What an action calls runs as its own action, decided on its own: it is a
                    // step of the run, not of this action's change.
                    for a in &f.facts.invokes {
                        add.extend(reach_of(a, &class, &reaches, app));
                    }
                }
                FuncKind::View | FuncKind::Block => {
                    for a in &f.facts.action_values {
                        add.extend(reach_of(a, &class, &reaches, app));
                    }
                    for v in &f.facts.views {
                        add.extend(
                            reaches
                                .get(*v)
                                .map(|r| r.values().cloned().collect::<Vec<_>>())
                                .unwrap_or_default(),
                        );
                    }
                }
                FuncKind::Fn | FuncKind::Guard | FuncKind::Applies => {}
            }
            let Some(map) = reaches.get_mut(i) else {
                continue;
            };
            for r in add {
                if map.insert((r.app.clone(), r.action.clone()), r).is_none() {
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    Effects {
        class,
        reaches: reaches
            .into_iter()
            .map(|m| m.into_values().collect())
            .collect(),
    }
}

fn ref_class(a: &ActionRef, class: &[EffectClass]) -> EffectClass {
    match (a.own, a.func) {
        (true, Some(f)) => class.get(f).copied().unwrap_or(EffectClass::Read),
        _ => a.foreign_class.unwrap_or(EffectClass::Undoable),
    }
}

fn entry_of_func(app: &str, name: &str, export: &str, class: EffectClass) -> Reach {
    Reach {
        app: app.to_string(),
        action: func_id(export).to_string(),
        name: name.to_string(),
        class,
        own: true,
    }
}

/// The action itself, plus everything an own one reaches.
fn reach_of(a: &ActionRef, class: &[EffectClass], reaches: &[ReachMap], app: &str) -> Vec<Reach> {
    let mut out = vec![Reach {
        app: a.app.clone(),
        action: a.id.clone(),
        name: a.name.clone(),
        class: ref_class(a, class),
        own: a.own && a.app == app,
    }];
    if let (true, Some(f)) = (a.own, a.func) {
        out.extend(
            reaches
                .get(f)
                .map(|r| r.values().cloned().collect::<Vec<_>>())
                .unwrap_or_default(),
        );
    }
    out
}
