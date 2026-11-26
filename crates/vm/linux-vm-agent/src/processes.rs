//! Whose process a caller's is.
//!
//! The guest's agent socket answers only the processes of a launch that serves
//! agents ([`crate::agent_socket`]): for each connection it reads the
//! connecting process's lineage from the kernel's table of processes and looks
//! for the launched program in it.
//!
//! This is a rule among the guest's ordinary processes, not a wall against the
//! machine's own administrator: whoever is root in the machine can enter any
//! process of it. What a connection may do is bounded on the host, where the
//! person and their limits are known.

use std::path::PathBuf;

/// The machine's processes, as its kernel lists them.
#[derive(Clone, Debug)]
pub struct Processes {
    root: PathBuf,
}

impl Processes {
    #[must_use]
    pub fn of_this_machine() -> Self {
        Self {
            root: PathBuf::from("/proc"),
        }
    }

    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The process that started `process`, from its `stat` line: the field
    /// after the state, which follows the name's closing bracket. A name may
    /// hold brackets and spaces itself, so the last bracket is the one.
    fn parent(&self, process: u32) -> Option<u32> {
        let stat =
            std::fs::read_to_string(self.root.join(process.to_string()).join("stat")).ok()?;
        let (_, after) = stat.rsplit_once(')')?;
        after.split_whitespace().nth(1)?.parse().ok()
    }

    /// `process` and the processes that started it, nearest first, up to the
    /// machine's first: whose process it is.
    #[must_use]
    pub fn lineage(&self, process: u32) -> Vec<u32> {
        /// Deeper than any process tree a program makes.
        const DEEPEST: usize = 256;
        let mut found = vec![process];
        let mut at = process;
        while let Some(parent) = self.parent(at) {
            if parent == 0 || found.contains(&parent) || found.len() >= DEEPEST {
                break;
            }
            found.push(parent);
            at = parent;
        }
        found
    }
}
