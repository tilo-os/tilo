//! Process **tree** model: parent→child hierarchy + subtree roll-ups.
//!
//! The Processes tab can render the flat list as a tree keyed on each
//! process's PPID, with disclosure chevrons. Two roll-up modes drive
//! what a parent row *displays*:
//!
//! * **Sum subtree** (default) — a collapsed parent shows the CPU% and
//!   memory of *itself plus every descendant*, so a single `browser` row
//!   answers "everything the browser is doing". Expanding it still lists
//!   each child with its **own** usage.
//! * **Per-process** — every row shows only its own usage, whether the tree
//!   is collapsed or expanded.
//!
//! Everything here is pure arithmetic over [`ProcessInfo`] — no Vello, no
//! CRDT, no `sysinfo`. That keeps the sums and the flatten/filter rules
//! unit-testable against the exact data the renderer consumes (the project's
//! verification covenant: assert on the real path, not a mirror).

use std::collections::HashMap;

use crate::app::SortKey;
use crate::model::ProcessInfo;

/// Which usage a row reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SumMode {
    /// A parent shows the summed CPU%/memory of itself + all descendants.
    SumSubtree,
    /// Every row shows only its own usage.
    PerProcess,
}

/// One node in the process forest. `children` indexes back into the node
/// arena; `cpu_subtree`/`mem_subtree` are the post-order roll-ups of this
/// node + every descendant.
#[derive(Clone, Debug)]
pub struct ProcNode {
    pub proc: ProcessInfo,
    pub children: Vec<usize>,
    /// Own CPU% + the CPU% of every descendant.
    pub cpu_subtree: f32,
    /// Own memory + the memory of every descendant.
    pub mem_subtree: u64,
    /// Number of descendants (transitive child count), for the row caption.
    pub descendants: usize,
}

/// A built process forest: a flat arena of [`ProcNode`]s plus the indices of
/// the root nodes (in arena order). Subtree roll-ups are already computed.
#[derive(Clone, Debug)]
pub struct Forest {
    pub nodes: Vec<ProcNode>,
    pub roots: Vec<usize>,
}

impl Forest {
    /// Build the forest from a flat process list. A process whose `ppid` is
    /// absent from the set (or `None`, or points at itself) becomes a root,
    /// so every process appears exactly once and nothing is dropped. Children
    /// keep the input order; the caller sorts siblings at flatten time.
    pub fn build(procs: &[ProcessInfo]) -> Forest {
        let index: HashMap<u32, usize> =
            procs.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();

        let mut nodes: Vec<ProcNode> = procs
            .iter()
            .map(|p| ProcNode {
                proc: p.clone(),
                children: Vec::new(),
                cpu_subtree: p.cpu,
                mem_subtree: p.mem_bytes,
                descendants: 0,
            })
            .collect();

        // `systemd` (PID 1 and every per-session `systemd --user`) is the
        // ancestor of nearly everything, so nesting the whole system under it is
        // technically correct but useless — the user just sees one "systemd"
        // row. Treat every systemd instance as a forest boundary: its direct
        // children (each service, the login/user session, the kthreadd
        // kernel-thread group) surface as their own top-level roots instead of
        // collapsing under a single giant systemd tree.
        let is_boundary: Vec<bool> = nodes
            .iter()
            .map(|n| n.proc.pid == 1 || n.proc.name == "systemd")
            .collect();

        let mut roots: Vec<usize> = Vec::new();
        for i in 0..nodes.len() {
            let Some(node_i) = nodes.get(i) else {
                continue;
            };
            let parent = node_i
                .proc
                .ppid
                .filter(|pp| *pp != node_i.proc.pid) // never self-parent
                .and_then(|pp| index.get(&pp).copied())
                .filter(|&pi| pi != i);
            match parent {
                // A child of a systemd boundary is promoted to a top-level root.
                Some(pi) if is_boundary.get(pi).copied().unwrap_or(false) => roots.push(i),
                Some(pi) => {
                    if let Some(node_pi) = nodes.get_mut(pi) {
                        node_pi.children.push(i);
                    }
                }
                None => roots.push(i),
            }
        }

        // Promote any node not reachable from a root to a root of its own.
        // This only fires for ppid *cycles* (a corrupted reading where every
        // member claims a parent), guaranteeing the forest covers every
        // process and `roll_up`'s root-seeded traversal visits all of them.
        let mut reachable = vec![false; nodes.len()];
        let mut stack: Vec<usize> = roots.clone();
        while let Some(i) = stack.pop() {
            match reachable.get_mut(i) {
                Some(seen) if !*seen => *seen = true,
                _ => continue,
            }
            if let Some(node_i) = nodes.get(i) {
                stack.extend(node_i.children.iter().copied());
            }
        }
        for i in 0..nodes.len() {
            if !reachable.get(i).copied().unwrap_or(true) {
                // Detach from its (cyclic) parent so the tree stays acyclic,
                // then root it.
                let parent_pid = nodes.get(i).and_then(|n| n.proc.ppid);
                if let Some(pp) = parent_pid.and_then(|pp| index.get(&pp).copied())
                    && let Some(node_pp) = nodes.get_mut(pp)
                {
                    node_pp.children.retain(|&c| c != i);
                }
                roots.push(i);
                // Mark its now-reachable subtree so we don't double-root.
                let mut s = vec![i];
                while let Some(j) = s.pop() {
                    match reachable.get_mut(j) {
                        Some(seen) if !*seen => *seen = true,
                        _ => continue,
                    }
                    if let Some(node_j) = nodes.get(j) {
                        s.extend(node_j.children.iter().copied());
                    }
                }
            }
        }

        let mut forest = Forest { nodes, roots };
        forest.roll_up();
        forest
    }

    /// Post-order accumulate each node's own usage + all descendants into
    /// `cpu_subtree`/`mem_subtree`/`descendants`. Iterative (explicit stack)
    /// so a pathological deep tree can't blow the call stack, and cycle-safe
    /// via a visited guard (a corrupted ppid loop can't hang the UI).
    fn roll_up(&mut self) {
        let n = self.nodes.len();
        let mut visited = vec![false; n];
        // (node, returning?) — first visit pushes children, second folds them.
        let mut stack: Vec<(usize, bool)> = self.roots.iter().map(|&r| (r, false)).collect();
        while let Some((i, returning)) = stack.pop() {
            if returning {
                let Some(node_i) = self.nodes.get(i) else {
                    continue;
                };
                let kids = node_i.children.clone();
                let mut cpu = node_i.proc.cpu;
                let mut mem = node_i.proc.mem_bytes;
                let mut desc = 0usize;
                for k in kids {
                    let Some(node_k) = self.nodes.get(k) else {
                        continue;
                    };
                    cpu += node_k.cpu_subtree;
                    mem += node_k.mem_subtree;
                    desc += 1 + node_k.descendants;
                }
                if let Some(node_i) = self.nodes.get_mut(i) {
                    node_i.cpu_subtree = cpu;
                    node_i.mem_subtree = mem;
                    node_i.descendants = desc;
                }
            } else {
                match visited.get_mut(i) {
                    Some(seen) if !*seen => *seen = true,
                    _ => continue,
                }
                stack.push((i, true));
                if let Some(node_i) = self.nodes.get(i) {
                    for &c in &node_i.children {
                        if !visited.get(c).copied().unwrap_or(true) {
                            stack.push((c, false));
                        }
                    }
                }
            }
        }
    }

    /// Every pid that has at least one child — the set the renderer draws a
    /// disclosure chevron for.
    pub fn parents(&self) -> Vec<u32> {
        self.nodes
            .iter()
            .filter(|n| !n.children.is_empty())
            .map(|n| n.proc.pid)
            .collect()
    }
}

/// Comparison order for siblings, mirroring the flat table's sort so the tree
/// reads the same way (busiest-first by default). Numeric keys compare the
/// *displayed* value (subtree sum in Sum mode), so a heavy-but-idle parent of
/// busy children still sorts to the top when summing.
fn cmp_nodes(a: &Row, b: &Row, key: SortKey, desc: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let o = match key {
        SortKey::Pid => a.pid.cmp(&b.pid),
        SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        SortKey::User => a.user.to_lowercase().cmp(&b.user.to_lowercase()),
        SortKey::Cpu => a.cpu.total_cmp(&b.cpu),
        SortKey::Mem => a.mem_bytes.cmp(&b.mem_bytes),
        SortKey::Threads => a.threads.cmp(&b.threads),
    };
    let o = if o == Ordering::Equal {
        a.pid.cmp(&b.pid)
    } else {
        o
    };
    if desc { o.reverse() } else { o }
}

/// One flattened, ready-to-draw tree row. `cpu`/`mem_bytes` are already the
/// *displayed* values for the active [`SumMode`] (own usage, or subtree sum
/// for a parent). `threads` is always the row's own thread count.
#[derive(Clone, Debug)]
pub struct Row {
    pub pid: u32,
    pub name: String,
    pub user: String,
    pub cpu: f32,
    pub mem_bytes: u64,
    pub threads: usize,
    pub status: String,
    /// Indentation level (0 = root).
    pub depth: usize,
    /// Whether this node has children (it shows a disclosure chevron).
    pub has_children: bool,
    /// Whether this node is currently expanded (chevron direction + whether
    /// children follow). Always `false` when `has_children` is false.
    pub expanded: bool,
    /// Transitive descendant count, for the collapsed-parent caption.
    pub descendants: usize,
}

/// Options driving `flatten`.
pub struct FlattenOpts<'a> {
    pub sum_mode: SumMode,
    pub sort_key: SortKey,
    pub sort_desc: bool,
    /// Lowercased filter; empty = no filter. A node is kept when it matches
    /// *or* any descendant matches (so ancestors of a hit stay visible), and
    /// when a node only survives because a descendant matched it is forced
    /// open so the match is reachable.
    pub filter: &'a str,
    /// pids the user has expanded. A parent not in this set is collapsed.
    pub expanded: &'a std::collections::HashSet<u32>,
    /// When false, kernel-thread roots/nodes are pruned from the tree.
    pub show_kernel: bool,
}

impl Forest {
    /// Does a node (or any descendant) match the filter? Used to decide
    /// whether to keep an ancestor whose own name doesn't match.
    fn matches_subtree(&self, i: usize, filter: &str) -> bool {
        if self.node_matches(i, filter) {
            return true;
        }
        self.nodes
            .get(i)
            .is_some_and(|n| n.children.iter().any(|&c| self.matches_subtree(c, filter)))
    }

    fn node_matches(&self, i: usize, filter: &str) -> bool {
        if filter.is_empty() {
            return true;
        }
        let Some(node) = self.nodes.get(i) else {
            return false;
        };
        let p = &node.proc;
        p.name.to_lowercase().contains(filter)
            || p.user.to_lowercase().contains(filter)
            || p.pid.to_string().contains(filter)
    }

    /// Flatten the forest into the visible, ordered, indented rows the table
    /// draws. Honors collapse state, the sum/per-process mode, the active
    /// sort (applied per sibling-group), the kernel-thread pref, and the
    /// search filter (keep matches + their ancestors; force-open ancestors of
    /// a deep match).
    pub fn flatten(&self, opts: &FlattenOpts) -> Vec<Row> {
        let mut out = Vec::new();
        // Filtering changes which nodes survive: a node is kept when it or a
        // descendant matches. When a node's own name does NOT match but it
        // only survives because of a descendant, we force it open so the user
        // can see the matched child without first expanding.
        let filtering = !opts.filter.is_empty();

        // Seed with roots, in sorted order.
        let mut root_rows: Vec<usize> = self
            .roots
            .iter()
            .copied()
            .filter(|&i| {
                self.nodes
                    .get(i)
                    .is_some_and(|n| opts.show_kernel || !n.proc.kernel)
            })
            .filter(|&i| !filtering || self.matches_subtree(i, opts.filter))
            .collect();
        self.sort_indices(&mut root_rows, opts);
        for r in root_rows {
            self.push_row(r, 0, opts, filtering, &mut out);
        }
        out
    }

    fn sort_indices(&self, indices: &mut [usize], opts: &FlattenOpts) {
        indices.sort_by(|&a, &b| {
            let ra = self.row_for(a, opts);
            let rb = self.row_for(b, opts);
            cmp_nodes(&ra, &rb, opts.sort_key, opts.sort_desc)
        });
    }

    /// Build the display [`Row`] for node `i` (without recursing). `cpu`/`mem`
    /// reflect the sum mode; `expanded`/`has_children` reflect collapse state.
    fn row_for(&self, i: usize, opts: &FlattenOpts) -> Row {
        let Some(n) = self.nodes.get(i) else {
            // An out-of-range arena index would be an internal bug (every
            // index reaching here comes from `roots`/`children`, populated
            // only with valid indices at build time) — degrade to an empty
            // row rather than panic.
            return Row {
                pid: 0,
                name: String::new(),
                user: String::new(),
                cpu: 0.0,
                mem_bytes: 0,
                threads: 0,
                status: String::new(),
                depth: 0,
                has_children: false,
                expanded: false,
                descendants: 0,
            };
        };
        let (cpu, mem) = match opts.sum_mode {
            SumMode::SumSubtree => (n.cpu_subtree, n.mem_subtree),
            SumMode::PerProcess => (n.proc.cpu, n.proc.mem_bytes),
        };
        let has_children = self.visible_children(i, opts).next().is_some();
        Row {
            pid: n.proc.pid,
            name: n.proc.name.clone(),
            user: n.proc.user.clone(),
            cpu,
            mem_bytes: mem,
            threads: n.proc.threads,
            status: n.proc.status.clone(),
            depth: 0,
            has_children,
            expanded: false,
            descendants: n.descendants,
        }
    }

    /// Children of `i` that survive the kernel-thread pref + filter.
    fn visible_children<'a>(
        &'a self,
        i: usize,
        opts: &'a FlattenOpts<'a>,
    ) -> impl Iterator<Item = usize> + 'a {
        let filtering = !opts.filter.is_empty();
        self.nodes
            .get(i)
            .into_iter()
            .flat_map(|n| n.children.iter().copied())
            .filter(move |&c| {
                self.nodes.get(c).is_some_and(|node| {
                    (opts.show_kernel || !node.proc.kernel)
                        && (!filtering || self.matches_subtree(c, opts.filter))
                })
            })
    }

    fn push_row(
        &self,
        i: usize,
        depth: usize,
        opts: &FlattenOpts,
        filtering: bool,
        out: &mut Vec<Row>,
    ) {
        // Iterative pre-order DFS (explicit stack, not recursion) so a very
        // large / deep subtree — e.g. expanding a node with thousands of
        // descendants — can never overflow the call stack.
        let mut stack: Vec<(usize, usize)> = vec![(i, depth)];
        while let Some((i, depth)) = stack.pop() {
            let mut row = self.row_for(i, opts);
            row.depth = depth;
            // Force-open a node that only survived the filter via a descendant
            // (its own name doesn't match) so the matched child is reachable.
            let forced_open = filtering && !self.node_matches(i, opts.filter);
            let expanded = row.has_children && (opts.expanded.contains(&row.pid) || forced_open);
            row.expanded = expanded;
            out.push(row);
            if expanded {
                let mut kids: Vec<usize> = self.visible_children(i, opts).collect();
                self.sort_indices(&mut kids, opts);
                // Push reversed so children pop (and render) in sorted order.
                for c in kids.into_iter().rev() {
                    stack.push((c, depth + 1));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[allow(
        clippy::similar_names,
        reason = "pid/ppid are the standard process-table terms; renaming either would read worse"
    )]
    fn p(pid: u32, ppid: Option<u32>, name: &str, cpu: f32, mem_mib: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            started_at: u64::from(pid) * 10,
            ppid,
            name: name.into(),
            user: "ada".into(),
            cpu,
            mem_bytes: mem_mib * 1024 * 1024,
            virt_bytes: mem_mib * 3 * 1024 * 1024,
            threads: 1,
            status: "Running".into(),
            cmd: name.into(),
            run_secs: 120,
            open_files: Some(8),
            kernel: false,
        }
    }

    /// A small Discord-style tree: a parent that's quiet itself but whose
    /// children do all the work.
    fn discord_tree() -> Vec<ProcessInfo> {
        vec![
            p(10, None, "discord", 2.0, 100),       // parent, light itself
            p(11, Some(10), "Renderer", 40.0, 500), // heavy child
            p(12, Some(10), "GPU", 18.0, 300),      // heavy child
            p(13, Some(11), "Audio", 5.0, 60),      // grandchild under Renderer
        ]
    }

    fn opts<'a>(sum: SumMode, expanded: &'a HashSet<u32>, filter: &'a str) -> FlattenOpts<'a> {
        FlattenOpts {
            sum_mode: sum,
            sort_key: SortKey::Cpu,
            sort_desc: true,
            filter,
            expanded,
            show_kernel: true,
        }
    }

    #[test]
    fn forest_build_promotes_boundaries_keeps_orphans_and_survives_cycles() {
        // Phase `forest_roots_and_orphans`.
        {
            // pid 1 is the init boundary, so its child is promoted to a top-level
            // root rather than nested under init; pid 99 points at a missing parent
            // → also a root, not dropped.
            let procs = vec![
                p(1, None, "init", 0.0, 10),
                p(2, Some(1), "child", 0.0, 10),
                p(99, Some(404), "orphan", 0.0, 10),
            ];
            let f = Forest::build(&procs);
            // init (pid 1), the promoted child (pid 2), and the orphan (pid 99).
            assert_eq!(f.roots.len(), 3, "init + promoted child + orphan are roots");
            assert_eq!(f.nodes.len(), 3, "no process dropped");
            let root_pids: std::collections::HashSet<u32> =
                f.roots.iter().map(|&i| f.nodes[i].proc.pid).collect();
            assert!(
                root_pids.contains(&2),
                "a child of init is a top-level root"
            );
            // init keeps no children — they were all promoted.
            let init = f.nodes.iter().find(|n| n.proc.pid == 1).unwrap();
            assert!(init.children.is_empty(), "init does not nest the world");
        }

        // Phase `systemd_user_is_also_a_boundary`.
        {
            // A per-session `systemd --user` (name "systemd", not pid 1) must also
            // be a boundary so the user's whole session doesn't collapse under it.
            let procs = vec![
                p(1, None, "systemd", 0.0, 10),
                p(1000, Some(1), "systemd", 0.0, 10), // systemd --user
                p(1200, Some(1000), "shell", 0.0, 10),
                p(1300, Some(1200), "editor", 0.0, 10),
            ];
            let f = Forest::build(&procs);
            let root_pids: std::collections::HashSet<u32> =
                f.roots.iter().map(|&i| f.nodes[i].proc.pid).collect();
            // Both systemds are roots, and the session leader (shell) is promoted
            // to a top-level root instead of hiding under systemd --user.
            assert!(
                root_pids.contains(&1200),
                "shell promoted out of systemd --user"
            );
            // The editor stays nested under its real parent (the shell).
            let shell = f.nodes.iter().find(|n| n.proc.pid == 1200).unwrap();
            assert_eq!(shell.children.len(), 1, "editor stays under the shell");
        }

        // Phase `cycle_in_ppid_does_not_hang`.
        {
            // Two processes that claim each other as parent — a corrupted reading.
            // build()/roll_up() must terminate and keep both.
            let procs = vec![p(1, Some(2), "a", 1.0, 10), p(2, Some(1), "b", 1.0, 10)];
            let f = Forest::build(&procs);
            assert_eq!(f.nodes.len(), 2);
            // One becomes a root (whichever is processed first attaches the other).
            assert!(!f.roots.is_empty());
        }
    }

    #[test]
    fn subtree_sums_drive_collapsed_rows_per_sum_mode() {
        // Phase `subtree_sum_rolls_up_every_descendant`.
        {
            let f = Forest::build(&discord_tree());
            // discord (idx 0) subtree = 2 + 40 + 18 + 5 = 65 CPU.
            let discord = f.nodes.iter().find(|n| n.proc.pid == 10).unwrap();
            assert!((discord.cpu_subtree - 65.0).abs() < 1e-3, "cpu sum");
            let mem_mib = discord.mem_subtree / (1024 * 1024);
            assert_eq!(mem_mib, 100 + 500 + 300 + 60, "mem sum");
            assert_eq!(discord.descendants, 3, "renderer+gpu+audio");
            // The grandchild rolls into the Renderer subtree too: 40 + 5 = 45.
            let renderer = f.nodes.iter().find(|n| n.proc.pid == 11).unwrap();
            assert!((renderer.cpu_subtree - 45.0).abs() < 1e-3);
        }

        // Phase `collapsed_parent_shows_summed_total_in_sum_mode`.
        {
            let f = Forest::build(&discord_tree());
            let expanded = HashSet::new(); // all collapsed
            let rows = f.flatten(&opts(SumMode::SumSubtree, &expanded, ""));
            // Collapsed: only the single root row shows, with the FULL subtree sum.
            assert_eq!(rows.len(), 1, "collapsed root only");
            assert_eq!(rows[0].pid, 10);
            assert!(
                (rows[0].cpu - 65.0).abs() < 1e-3,
                "collapsed parent = subtree sum"
            );
            assert!(rows[0].has_children && !rows[0].expanded);
        }

        // Phase `per_process_mode_shows_own_usage_even_collapsed`.
        {
            let f = Forest::build(&discord_tree());
            let expanded = HashSet::new();
            let rows = f.flatten(&opts(SumMode::PerProcess, &expanded, ""));
            assert_eq!(rows.len(), 1);
            assert!(
                (rows[0].cpu - 2.0).abs() < 1e-3,
                "per-process = own usage only"
            );
        }

        // Phase `summed_collapsed_total_changes_vs_per_process`.
        {
            // The headline assertion behind the toggle: the parent's displayed
            // CPU differs between the two modes (sum 65 vs own 2).
            let f = Forest::build(&discord_tree());
            let expanded = HashSet::new();
            let summed = f.flatten(&opts(SumMode::SumSubtree, &expanded, ""))[0].cpu;
            let per = f.flatten(&opts(SumMode::PerProcess, &expanded, ""))[0].cpu;
            assert!(
                summed > per + 1.0,
                "sum {summed} must exceed per-process {per}"
            );
        }
    }

    #[test]
    fn flatten_expands_filters_with_ancestors_and_prunes_kernel_nodes() {
        // Phase `expanding_reveals_children_with_their_own_usage`.
        {
            let f = Forest::build(&discord_tree());
            let mut expanded = HashSet::new();
            expanded.insert(10); // open discord, but not Renderer
            let rows = f.flatten(&opts(SumMode::SumSubtree, &expanded, ""));
            // discord + Renderer + GPU visible (Renderer's child still collapsed).
            assert_eq!(rows.len(), 3);
            let renderer = rows.iter().find(|r| r.pid == 11).unwrap();
            // Even in Sum mode, an EXPANDED child still shows ITS OWN subtree sum
            // for its own descendants — Renderer = 40 (own) + 5 (Audio) = 45.
            assert!((renderer.cpu - 45.0).abs() < 1e-3);
            assert_eq!(renderer.depth, 1, "child is indented");
            // The leaf GPU shows its own usage (no descendants to sum).
            let gpu = rows.iter().find(|r| r.pid == 12).unwrap();
            assert!((gpu.cpu - 18.0).abs() < 1e-3);
        }

        // Phase `filter_keeps_match_and_ancestors_and_forces_open`.
        {
            let f = Forest::build(&discord_tree());
            let expanded = HashSet::new(); // nothing user-expanded
            let rows = f.flatten(&opts(SumMode::SumSubtree, &expanded, "audio"));
            // "Audio" matches; its ancestors discord→Renderer must appear and be
            // force-opened even though the user collapsed everything.
            let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
            assert!(names.contains(&"discord"), "ancestor kept: {names:?}");
            assert!(names.contains(&"Renderer"), "ancestor kept: {names:?}");
            assert!(names.contains(&"Audio"), "match kept: {names:?}");
            assert!(
                !names.contains(&"GPU"),
                "non-matching sibling pruned: {names:?}"
            );
        }

        // Phase `kernel_pref_prunes_kernel_nodes`.
        {
            let mut procs = discord_tree();
            let mut kthread = p(50, None, "kworker/0", 1.0, 0);
            kthread.kernel = true;
            procs.push(kthread);
            let f = Forest::build(&procs);
            let expanded = HashSet::new();
            let hidden = FlattenOpts {
                show_kernel: false,
                ..opts(SumMode::SumSubtree, &expanded, "")
            };
            let rows = f.flatten(&hidden);
            assert!(rows.iter().all(|r| r.name != "kworker/0"), "kernel pruned");
            let shown = f.flatten(&opts(SumMode::SumSubtree, &expanded, ""));
            assert!(
                shown.iter().any(|r| r.name == "kworker/0"),
                "kernel shown when on"
            );
        }
    }
}
