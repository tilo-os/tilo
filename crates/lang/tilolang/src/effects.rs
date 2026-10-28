//! Effect classes (spec 3.2) and capability sets (spec 4.4), computed from the IR facts.

use crate::ir::{ActionRef, FuncKind, Module};
use crate::meta::Reach;
use crate::types::EffectClass;
use std::collections::BTreeMap;

/// The result of the analysis, indexed like [`Module::funcs`].
pub struct Effects {
    /// Class of each action and flow; `Read` for the rest.
    pub class: Vec<EffectClass>,
    /// Every action or flow each function can reach.
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
            for callee in &f.facts.inline_actions {
                c = c.max(class.get(*callee).copied().unwrap_or(EffectClass::Read));
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
    for (i, f) in m.funcs.iter().enumerate() {
        if f.kind != FuncKind::Flow {
            continue;
        }
        let c = f
            .facts
            .invokes
            .iter()
            .map(|a| ref_class(a, &class))
            .max()
            .unwrap_or(EffectClass::Read);
        if let Some(slot) = class.get_mut(i) {
            *slot = c;
        }
    }
    let mut reaches: Vec<ReachMap> = vec![ReachMap::new(); n];
    // Actions reach themselves and what their inlined callees reach; views reach what their
    // buttons apply and what the views they call reach. Iterate to a fixed point.
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
                    for callee in &f.facts.inline_actions {
                        add.extend(
                            reaches
                                .get(*callee)
                                .map(|r| r.values().cloned().collect::<Vec<_>>())
                                .unwrap_or_default(),
                        );
                    }
                }
                FuncKind::Flow => {
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
                FuncKind::Fn => {}
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

/// The action or flow itself, plus everything an own one reaches.
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
