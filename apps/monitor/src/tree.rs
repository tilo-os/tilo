//! The parent→child process tree with subtree roll-ups over [`ProcessInfo`], and
//! the flattened rows the table draws. In sum mode a parent shows its own use plus
//! every descendant's; per-process mode shows each row's own.

use std::collections::HashMap;

use crate::app::SortKey;
use crate::model::ProcessInfo;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SumMode {
    /// A parent shows the summed use of itself and every descendant.
    SumSubtree,
    PerProcess,
}

/// One node: its process, its children's indices into the arena, and its subtree
/// sums.
#[derive(Clone, Debug)]
pub struct ProcNode {
    pub proc: ProcessInfo,
    pub children: Vec<usize>,
    pub cpu_subtree: f32,
    pub mem_subtree: u64,
}

/// A built process forest: the node arena, the root indices, and the computed
/// sums.
#[derive(Clone, Debug)]
pub struct Forest {
    pub nodes: Vec<ProcNode>,
    pub roots: Vec<usize>,
}

impl Forest {
    /// Build the forest: a process whose parent is absent, `None` or itself becomes
    /// a root, so nothing is dropped.
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
            })
            .collect();

        // `systemd` (pid 1 and every `systemd --user`) is a forest boundary: nesting
        // the whole system under it hides everything behind one row.
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
                Some(pi) if is_boundary.get(pi).copied().unwrap_or(false) => roots.push(i),
                Some(pi) => {
                    if let Some(node_pi) = nodes.get_mut(pi) {
                        node_pi.children.push(i);
                    }
                }
                None => roots.push(i),
            }
        }

        // A ppid cycle makes a node unreachable from any root: root it, so the forest
        // covers every process.
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
                // Detach from the cyclic parent so the tree stays acyclic.
                let parent_pid = nodes.get(i).and_then(|n| n.proc.ppid);
                if let Some(pp) = parent_pid.and_then(|pp| index.get(&pp).copied())
                    && let Some(node_pp) = nodes.get_mut(pp)
                {
                    node_pp.children.retain(|&c| c != i);
                }
                roots.push(i);
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

    /// Accumulate post-order into the subtree sums, with an explicit stack so no
    /// tree depth can overflow it.
    fn roll_up(&mut self) {
        let n = self.nodes.len();
        let mut visited = vec![false; n];
        // (node, returning?): the first visit pushes the children, the second folds
        // them.
        let mut stack: Vec<(usize, bool)> = self.roots.iter().map(|&r| (r, false)).collect();
        while let Some((i, returning)) = stack.pop() {
            if returning {
                let Some(node_i) = self.nodes.get(i) else {
                    continue;
                };
                let kids = node_i.children.clone();
                let mut cpu = node_i.proc.cpu;
                let mut mem = node_i.proc.mem_bytes;
                for k in kids {
                    let Some(node_k) = self.nodes.get(k) else {
                        continue;
                    };
                    cpu += node_k.cpu_subtree;
                    mem += node_k.mem_subtree;
                }
                if let Some(node_i) = self.nodes.get_mut(i) {
                    node_i.cpu_subtree = cpu;
                    node_i.mem_subtree = mem;
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
}

/// Sibling order, mirroring the flat table's sort; numeric keys compare the
/// displayed value, so a summing parent sorts by its whole subtree.
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

/// A flattened, ready-to-draw row. `cpu` and `mem_bytes` are the values the active
/// [`SumMode`] displays.
#[derive(Clone, Debug)]
pub struct Row {
    pub pid: u32,
    pub name: String,
    pub user: String,
    pub cpu: f32,
    pub mem_bytes: u64,
    pub threads: usize,
    pub depth: usize,
    pub has_children: bool,
    /// Always `false` when `has_children` is false.
    pub expanded: bool,
}

/// What drives `flatten`.
pub struct FlattenOpts<'a> {
    pub sum_mode: SumMode,
    pub sort_key: SortKey,
    pub sort_desc: bool,
    /// Lowercase filter; a node is kept when it or a descendant matches.
    pub filter: &'a str,
    /// The pids the person expanded.
    pub expanded: &'a std::collections::HashSet<u32>,
    pub show_kernel: bool,
}

impl Forest {
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

    pub fn flatten(&self, opts: &FlattenOpts) -> Vec<Row> {
        let mut out = Vec::new();
        // A node kept only because a descendant matched is forced open, so the match
        // is reachable.
        let filtering = !opts.filter.is_empty();

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

    fn row_for(&self, i: usize, opts: &FlattenOpts) -> Row {
        let Some(n) = self.nodes.get(i) else {
            // An out-of-range index would be an internal bug: show an empty row
            // rather than panic.
            return Row {
                pid: 0,
                name: String::new(),
                user: String::new(),
                cpu: 0.0,
                mem_bytes: 0,
                threads: 0,
                depth: 0,
                has_children: false,
                expanded: false,
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
            depth: 0,
            has_children,
            expanded: false,
        }
    }

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
        // An explicit stack, so a very deep subtree cannot overflow the call stack.
        let mut stack: Vec<(usize, usize)> = vec![(i, depth)];
        while let Some((i, depth)) = stack.pop() {
            let mut row = self.row_for(i, opts);
            row.depth = depth;
            // A node kept only by a descendant match opens so the match is reachable.
            let forced_open = filtering && !self.node_matches(i, opts.filter);
            let expanded = row.has_children && (opts.expanded.contains(&row.pid) || forced_open);
            row.expanded = expanded;
            out.push(row);
            if expanded {
                let mut kids: Vec<usize> = self.visible_children(i, opts).collect();
                self.sort_indices(&mut kids, opts);
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
            app: None,
        }
    }

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
        {
            let procs = vec![
                p(1, None, "init", 0.0, 10),
                p(2, Some(1), "child", 0.0, 10),
                p(99, Some(404), "orphan", 0.0, 10),
            ];
            let f = Forest::build(&procs);
            assert_eq!(f.roots.len(), 3, "init + promoted child + orphan are roots");
            assert_eq!(f.nodes.len(), 3, "no process dropped");
            let root_pids: std::collections::HashSet<u32> =
                f.roots.iter().map(|&i| f.nodes[i].proc.pid).collect();
            assert!(
                root_pids.contains(&2),
                "a child of init is a top-level root"
            );
            let init = f.nodes.iter().find(|n| n.proc.pid == 1).unwrap();
            assert!(init.children.is_empty(), "init does not nest the world");
        }

        {
            let procs = vec![
                p(1, None, "systemd", 0.0, 10),
                p(1000, Some(1), "systemd", 0.0, 10), // systemd --user
                p(1200, Some(1000), "shell", 0.0, 10),
                p(1300, Some(1200), "editor", 0.0, 10),
            ];
            let f = Forest::build(&procs);
            let root_pids: std::collections::HashSet<u32> =
                f.roots.iter().map(|&i| f.nodes[i].proc.pid).collect();
            assert!(
                root_pids.contains(&1200),
                "shell promoted out of systemd --user"
            );
            let shell = f.nodes.iter().find(|n| n.proc.pid == 1200).unwrap();
            assert_eq!(shell.children.len(), 1, "editor stays under the shell");
        }

        {
            let procs = vec![p(1, Some(2), "a", 1.0, 10), p(2, Some(1), "b", 1.0, 10)];
            let f = Forest::build(&procs);
            assert_eq!(f.nodes.len(), 2);
            assert!(!f.roots.is_empty());
        }
    }

    #[test]
    fn subtree_sums_drive_collapsed_rows_per_sum_mode() {
        {
            let f = Forest::build(&discord_tree());
            let discord = f.nodes.iter().find(|n| n.proc.pid == 10).unwrap();
            assert!((discord.cpu_subtree - 65.0).abs() < 1e-3, "cpu sum");
            let mem_mib = discord.mem_subtree / (1024 * 1024);
            assert_eq!(mem_mib, 100 + 500 + 300 + 60, "mem sum");
            let renderer = f.nodes.iter().find(|n| n.proc.pid == 11).unwrap();
            assert!((renderer.cpu_subtree - 45.0).abs() < 1e-3);
        }

        {
            let f = Forest::build(&discord_tree());
            let expanded = HashSet::new(); // all collapsed
            let rows = f.flatten(&opts(SumMode::SumSubtree, &expanded, ""));
            assert_eq!(rows.len(), 1, "collapsed root only");
            assert_eq!(rows[0].pid, 10);
            assert!(
                (rows[0].cpu - 65.0).abs() < 1e-3,
                "collapsed parent = subtree sum"
            );
            assert!(rows[0].has_children && !rows[0].expanded);
        }

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

        {
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
        {
            let f = Forest::build(&discord_tree());
            let mut expanded = HashSet::new();
            expanded.insert(10); // open discord, but not Renderer
            let rows = f.flatten(&opts(SumMode::SumSubtree, &expanded, ""));
            assert_eq!(rows.len(), 3);
            let renderer = rows.iter().find(|r| r.pid == 11).unwrap();
            assert!((renderer.cpu - 45.0).abs() < 1e-3);
            assert_eq!(renderer.depth, 1, "child is indented");
            let gpu = rows.iter().find(|r| r.pid == 12).unwrap();
            assert!((gpu.cpu - 18.0).abs() < 1e-3);
        }

        {
            let f = Forest::build(&discord_tree());
            let expanded = HashSet::new(); // nothing user-expanded
            let rows = f.flatten(&opts(SumMode::SumSubtree, &expanded, "audio"));
            let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
            assert!(names.contains(&"discord"), "ancestor kept: {names:?}");
            assert!(names.contains(&"Renderer"), "ancestor kept: {names:?}");
            assert!(names.contains(&"Audio"), "match kept: {names:?}");
            assert!(
                !names.contains(&"GPU"),
                "non-matching sibling pruned: {names:?}"
            );
        }

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
