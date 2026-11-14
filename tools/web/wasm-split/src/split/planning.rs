//! The plan a split is made by: the reachability sets, the shared symbols,
//! the chunks every module is emitted from, and the plan moved back through
//! the renumbering the bind patch wrote (split.rs).
//!
//! A child module of `split`, so it reads the splitter's own fields.

use std::collections::HashSet;

use anyhow::Result;

use crate::plan::{Plan, PlanCache};

use super::*;

impl<'a> Splitter<'a> {
    /// Everything a split decides before it writes a module, in the module's
    /// index spaces (plan.rs).
    pub(crate) fn plan(&self) -> Plan {
        Plan {
            main_graph: self
                .main_graph
                .iter()
                .map(|node| self.plan_node(node))
                .collect(),
            reachable: self
                .split_points
                .iter()
                .map(|split| {
                    split
                        .reachable_graph
                        .iter()
                        .map(|node| self.plan_node(node))
                        .collect()
                })
                .collect(),
            chunks: self
                .chunks
                .iter()
                .map(|chunk| chunk.iter().map(|node| self.plan_node(node)).collect())
                .collect(),
            chunk_imports: self
                .chunk_imports
                .iter()
                .map(|imports| imports.iter().map(|node| self.plan_node(node)).collect())
                .collect(),
            shared_symbols: self
                .shared_symbols
                .iter()
                .map(|node| self.plan_node(node))
                .collect(),
            data_sizes: self.data_sizes.clone(),
            unused: self
                .planned_unused
                .iter()
                .flatten()
                .map(|node| self.plan_node(node))
                .collect(),
            graph: self
                .call_graph
                .iter()
                .map(|(node, children)| {
                    (
                        self.plan_node(node),
                        children.iter().map(|child| self.plan_node(child)).collect(),
                    )
                })
                .collect(),
            data_names: self
                .data_names
                .iter()
                .map(|(index, name)| (*index, name.clone()))
                .collect(),
        }
    }

    /// Take `plan` as this split's, as [`Self::plan`] would have made it.
    pub(crate) fn use_plan(&mut self, plan: Plan) {
        let ids = &self._ids_to_fns;
        let node = |node| from_plan_node(ids, node);
        self.main_graph = plan.main_graph.into_iter().map(node).collect();
        for (split, reachable) in self.split_points.iter_mut().zip(plan.reachable) {
            split.reachable_graph = reachable.into_iter().map(node).collect();
        }
        self.chunks = plan
            .chunks
            .into_iter()
            .map(|chunk| chunk.into_iter().map(node).collect())
            .collect();
        self.chunk_imports = plan
            .chunk_imports
            .into_iter()
            .map(|imports| imports.into_iter().map(node).collect())
            .collect();
        self.shared_symbols = plan.shared_symbols.into_iter().map(node).collect();
        self.data_sizes = plan.data_sizes;
        self.planned_unused = Some(plan.unused.into_iter().map(node).collect());
    }

    /// A node as a plan holds it: the function's index in the bound module,
    /// or the data symbol's own.
    fn plan_node(&self, node: &Node) -> crate::plan::Node {
        match node {
            Node::Function(id) => {
                crate::plan::function_node(self.fns_to_ids.get(id).copied().unwrap_or(usize::MAX))
            }
            Node::DataSymbol(index) => crate::plan::data_node(*index),
        }
    }

    /// Every split module's entries: what only the table reaches.
    pub(crate) fn entry_funcs(&self) -> HashSet<FunctionId> {
        self.split_points
            .iter()
            .flat_map(|split| split.entries.iter().map(|entry| entry.func))
            .collect()
    }

    pub(crate) fn unused_main_symbols(&self) -> HashSet<Node> {
        let entries = self.entry_funcs();
        self.split_points
            .iter()
            .flat_map(|split| split.reachable_graph.iter())
            .filter(|sym| {
                // Make sure the symbol isn't in the main graph
                if self.main_graph.contains(sym) {
                    return false;
                }

                // And ensure we aren't also exporting it, but as a lazy
                // export, whose export a stub takes over
                match sym {
                    Node::Function(u) => {
                        entries.contains(u)
                            || self.source_module.exports.get_exported_func(*u).is_none()
                    }
                    _ => true,
                }
            })
            .cloned()
            .collect()
    }

    /// The plan the last run made, moved to this run's module through the
    /// renumbering the bind patch wrote: every function and data symbol by
    /// what it is, the changed and new functions' edges made again from
    /// their bodies (and, for what a body reaches through the static data,
    /// the linked module's relocations for those functions alone).
    ///
    /// `None` when there is no renumbering, no plan to move, or nothing
    /// ties the two together; the caller then makes the plan again, which
    /// costs what it always did and is never a different plan.
    pub(crate) fn read_plan_back(&mut self, plans: &PlanCache) -> Result<Option<Plan>> {
        let Some(renumber) = &self.renumber else {
            return Ok(None);
        };
        let Some(old) = plans.load_latest() else {
            return Ok(None);
        };
        if old.graph.is_empty() {
            return Ok(None);
        }
        // The data symbols by name: their indices move with the functions'.
        let mut old_names: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, name) in &old.data_names {
            old_names.entry(name.as_str()).or_default().push(*index);
        }
        let new_names = crate::relocations::data_names(self.original)?;
        let mut data_map: HashMap<usize, usize> = HashMap::new();
        for (new_index, name) in &new_names {
            let Some(candidates) = old_names.get(name.as_str()) else {
                continue;
            };
            if candidates.len() != 1 {
                return Ok(None);
            }
            data_map.insert(candidates[0], *new_index);
        }
        let function = |old: usize| -> Option<usize> {
            if old < renumber.old_imports as usize {
                return Some(old);
            }
            let local = renumber.map.get(old - renumber.old_imports as usize)?;
            local.map(|new| renumber.new_imports as usize + new as usize)
        };
        let remap = |node: &crate::plan::Node| -> Result<Option<crate::plan::Node>> {
            let (index, data) = crate::plan::node_index(*node);
            if data {
                // A data symbol is its name: one this tool cannot name is one
                // it cannot follow, and the plan is made again.
                let Some(new) = data_map.get(&index) else {
                    return Ok(None);
                };
                Ok(Some(crate::plan::data_node(*new)))
            } else {
                Ok(function(index).map(crate::plan::function_node))
            }
        };
        let mut graph: HashMap<Node, HashSet<Node>> = HashMap::with_capacity(old.graph.len());
        for (node, children) in &old.graph {
            let Some(node) = remap(node)? else { continue };
            let mut set = HashSet::with_capacity(children.len());
            for child in children {
                let Some(child) = remap(child)? else { continue };
                set.insert(from_plan_node(&self._ids_to_fns, child));
            }
            graph.insert(from_plan_node(&self._ids_to_fns, node), set);
        }
        // What a symbol is called does not move; its index does.
        self.data_sizes = old
            .data_sizes
            .iter()
            .filter_map(|(index, size)| Some((*data_map.get(index)?, *size)))
            .collect();
        // The start function's edges are not all its body's: wasm-bindgen's
        // dissolved functions are anchored to it, and this tool does not
        // know which of them the edges were. A change to it makes the plan
        // again.
        let anchor = self
            .source_module
            .funcs
            .by_name("main")
            .or_else(|| self.source_module.exports.get_func("__wbindgen_start").ok());
        if let Some(anchor) = anchor {
            if let Some(local) = self
                .fns_to_ids
                .get(&anchor)
                .map(|index| index - self.imported)
            {
                if self.bodies_changed.contains(&local) {
                    return Ok(None);
                }
            }
        }
        // The bodies this run walked: their functions' edges are this
        // module's, not the last one's.
        let wanted: HashSet<usize> = self
            .bodies_changed
            .iter()
            .filter_map(|local| renumber.linked_of(*local))
            .collect();
        let data_edges = crate::relocations::code_data_edges(self.original, &wanted)?;
        for local in &self.bodies_changed {
            let function = self._ids_to_fns[self.imported + local];
            let mut children: HashSet<Node> = self.bodies[*local]
                .callees
                .iter()
                .map(|index| Node::Function(self._ids_to_fns[*index as usize]))
                .collect();
            if let Some(linked) = renumber.linked_of(*local) {
                for data in data_edges.get(&linked).into_iter().flatten() {
                    children.insert(Node::DataSymbol(*data));
                }
            }
            graph.insert(Node::Function(function), children);
        }
        self.call_graph = graph;
        self.derive_from_graph()?;
        // What the start module keeps but does not reach, as `finish` reads
        // it after making the plan.
        let mut unused = self.unused_main_symbols();
        self.keep_referenced(&mut unused);
        self.planned_unused = Some(unused);
        Ok(Some(self.plan()))
    }

    /// The reachability sets, the shared symbols and the chunks every module
    /// is emitted from, over `self.call_graph`: what a plan holds and what an
    /// emission reads. Made again by every plan, including one moved through
    /// a renumbering with the changed functions' edges replaced.
    pub(crate) fn derive_from_graph(&mut self) -> Result<()> {
        let mut step = std::time::Instant::now();
        self.parent_graph.clear();
        for (parent, children) in self.call_graph.iter() {
            for child in children {
                self.parent_graph.entry(*child).or_default().insert(*parent);
            }
        }

        // Now go fill in the reachability graph for each of the split points
        // We want to be as narrow as possible since we've reparented any new symbols to the main module
        // Each point's walk reads the graph and writes its own set: walked
        // one point a thread.
        let roots = self.main_roots();
        let (call_graph, split_points) = (&self.call_graph, &mut self.split_points);
        let (main, ()) = rayon::join(
            || reachable_graph(call_graph, &roots),
            || {
                split_points.par_iter_mut().for_each(|split| {
                    let roots: HashSet<_> = split
                        .entries
                        .iter()
                        .map(|entry| Node::Function(entry.func))
                        .collect();
                    split.reachable_graph = reachable_graph(call_graph, &roots);
                });
            },
        );
        // And then the reachability graph for main
        self.main_graph = main;
        took("walked the call graph from every split point", step);
        step = std::time::Instant::now();
        // WASM_SPLIT_WHY=name,name prints why each function stays in the main
        // module (its path from a root) and stops: the way to find the next cut.
        if let Ok(targets) = std::env::var("WASM_SPLIT_WHY") {
            for target in targets.split(',') {
                eprintln!("{target}:");
                self.explain_main_path(target);
            }
            std::process::exit(0);
        }

        // And then the symbols shared between all
        self.shared_symbols = {
            let mut shared_funcs = HashSet::new();

            for split in self.split_points.iter() {
                shared_funcs.extend(self.main_graph.intersection(&split.reachable_graph));
            }

            // And then all our imports will be callabale via the ifunc table too
            for import in self.source_module.imports.iter() {
                if let ImportKind::Function(id) = import.kind {
                    shared_funcs.insert(Node::Function(id));
                }
            }

            // Make sure to make this *ordered*
            shared_funcs.into_iter().collect()
        };

        took("found the shared symbols", step);
        Ok(())
    }

    pub(crate) fn main_roots(&self) -> HashSet<Node> {
        // Accumulate all the split entrypoints
        // This will include wasm_bindgen functions too
        let exported_splits = self.entry_funcs();

        // And only return the functions that are reachable from the main module's start function
        let mut roots = self
            .source_module
            .exports
            .iter()
            .filter_map(|e| match e.item {
                ExportItem::Function(id) if !exported_splits.contains(&id) => {
                    Some(Node::Function(id))
                }
                _ => None,
            })
            .chain(self.source_module.start.map(Node::Function))
            .collect::<HashSet<Node>>();

        // Also add "imports" to the roots
        for import in self.source_module.imports.iter() {
            if let ImportKind::Function(id) = import.kind {
                roots.insert(Node::Function(id));
            }
        }

        roots
    }

    /// Keep in the main module every function that code staying there calls
    /// or takes the address of. The relocation call graph can miss an edge
    /// (wasm-bindgen rewrites and renames functions); deleting a function
    /// that main still references breaks the module, while keeping one only
    /// costs its bytes.
    pub(crate) fn keep_referenced(&self, unused: &mut HashSet<Node>) {
        let module = &self.source_module;
        let split_exports = self.entry_funcs();
        // Walk from everything main keeps, through both the code's own
        // references and the relocation graph: a function kept only by
        // this walk still reaches vtables (data symbols) whose methods are
        // called through the table, and those must stay too, or the call
        // lands on the hole left for a split module.
        let mut queue: Vec<Node> = module
            .funcs
            .iter()
            .map(|f| Node::Function(f.id()))
            .filter(|node| !unused.contains(node))
            .filter(|node| !matches!(node, Node::Function(id) if split_exports.contains(id)))
            .collect();
        let mut seen: HashSet<Node> = queue.iter().copied().collect();
        let mut kept = 0usize;
        while let Some(node) = queue.pop() {
            let mut next: Vec<Node> = self
                .call_graph
                .get(&node)
                .into_iter()
                .flatten()
                .copied()
                .collect();
            if let Node::Function(id) = node {
                // What the body calls, as the walk that built the call graph
                // read it (analysis.rs).
                if let Some(body) = self
                    .fns_to_ids
                    .get(&id)
                    .and_then(|index| index.checked_sub(self.imported))
                    .and_then(|index| self.bodies.get(index))
                {
                    next.extend(
                        body.callees
                            .iter()
                            .map(|index| Node::Function(self._ids_to_fns[*index as usize])),
                    );
                }
            }
            for child in next {
                if seen.insert(child) {
                    if unused.remove(&child) && matches!(child, Node::Function(_)) {
                        kept += 1;
                    }
                    queue.push(child);
                }
            }
        }
        if kept > 0 {
            tracing::warn!("kept {kept} functions in the main module the call graph had moved out");
            eprintln!(
                "wasm-split: kept {kept} functions in the start module that the call graph missed"
            );
        }
    }
}
