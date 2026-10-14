//! The host's footprint, read from `/proc`: its whole process tree, since
//! the relay applies peers' Automerge bytes in worker processes it starts
//! (`tilo-automerge-isolate`) that keep their own copies of the rooms'
//! documents, and a single-process reading leaves their CPU and memory out.

use std::collections::HashMap;

/// `USER_HZ`, 100 on every Linux the builder runs.
const TICKS_PER_SECOND: f64 = 100.0;

/// The fields of `/proc/<pid>/stat` (or a task's) after the command name:
/// state is index 0, ppid 1, utime 11, stime 12, cutime 13, cstime 14.
fn stat_fields(path: &str) -> Option<Vec<u64>> {
    let stat = std::fs::read_to_string(path).ok()?;
    let after = stat.rsplit_once(") ")?.1;
    Some(
        after
            .split_whitespace()
            .map(|field| field.parse().unwrap_or_default())
            .collect(),
    )
}

fn field(fields: &[u64], index: usize) -> u64 {
    fields.get(index).copied().unwrap_or_default()
}

/// CPU seconds (user + system) `pid` itself has used.
pub fn process_cpu(pid: u32) -> Option<f64> {
    let fields = stat_fields(&format!("/proc/{pid}/stat"))?;
    Some((field(&fields, 11) + field(&fields, 12)) as f64 / TICKS_PER_SECOND)
}

/// Resident bytes of `pid` with shared pages split among their sharers
/// (`Pss`), so a tree's sum counts each page once; `VmRSS` where the kernel
/// has no `smaps_rollup`.
pub fn proportional_bytes(pid: u32) -> Option<u64> {
    let (path, key) = if std::path::Path::new(&format!("/proc/{pid}/smaps_rollup")).exists() {
        (format!("/proc/{pid}/smaps_rollup"), "Pss:")
    } else {
        (format!("/proc/{pid}/status"), "VmRSS:")
    };
    let text = std::fs::read_to_string(path).ok()?;
    let kib: u64 = text
        .lines()
        .find_map(|line| line.strip_prefix(key))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(kib * 1024)
}

/// `root` and every live process descended from it.
fn tree(root: u32) -> Vec<u32> {
    let mut parents = HashMap::<u32, Vec<u32>>::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if let Some(fields) = stat_fields(&format!("/proc/{pid}/stat")) {
            let parent = u32::try_from(field(&fields, 1)).unwrap_or_default();
            parents.entry(parent).or_default().push(pid);
        }
    }
    let mut out = vec![root];
    let mut index = 0;
    while let Some(pid) = out.get(index).copied() {
        out.extend(parents.get(&pid).into_iter().flatten().copied());
        index += 1;
    }
    out
}

/// One reading of a process tree.
#[derive(Default)]
pub struct Sample {
    /// CPU seconds of every live process in the tree, plus the root's
    /// reaped children (workers that already exited).
    pub cpu_s: f64,
    /// Proportional resident bytes of the tree.
    pub bytes: u64,
    /// Processes in the tree.
    pub processes: usize,
    /// CPU seconds per thread, keyed by (process, thread).
    threads: HashMap<(u32, u32), f64>,
}

impl Sample {
    /// The tree under `root` now, or `None` once `root` is gone.
    pub fn of(root: u32) -> Option<Self> {
        let root_fields = stat_fields(&format!("/proc/{root}/stat"))?;
        let mut sample = Self {
            cpu_s: (field(&root_fields, 13) + field(&root_fields, 14)) as f64 / TICKS_PER_SECOND,
            ..Self::default()
        };
        for pid in tree(root) {
            let Some(cpu) = process_cpu(pid) else {
                continue;
            };
            sample.cpu_s += cpu;
            sample.bytes += proportional_bytes(pid).unwrap_or_default();
            sample.processes += 1;
            for task in std::fs::read_dir(format!("/proc/{pid}/task"))
                .into_iter()
                .flatten()
                .flatten()
            {
                let Some(tid) = task
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse::<u32>().ok())
                else {
                    continue;
                };
                if let Some(fields) = stat_fields(&format!("/proc/{pid}/task/{tid}/stat")) {
                    let cpu = (field(&fields, 11) + field(&fields, 12)) as f64 / TICKS_PER_SECOND;
                    sample.threads.insert((pid, tid), cpu);
                }
            }
        }
        Some(sample)
    }

    /// The cores the busiest single thread used between `earlier` and this
    /// reading, `secs` apart: near 1.0 says one thread is what everything
    /// waits on, however idle the other cores are.
    pub fn busiest_thread_cores(&self, earlier: &Self, secs: f64) -> f64 {
        self.threads
            .iter()
            .map(|(key, cpu)| cpu - earlier.threads.get(key).copied().unwrap_or_default())
            .fold(0.0, f64::max)
            / secs.max(1e-3)
    }
}
