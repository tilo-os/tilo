//! Whose socket a caller's is.
//!
//! An endpoint of a served run listens on the machine's loopback, which every
//! process in the machine can reach. It answers only the run's own: for each
//! connection the launcher finds the caller's end of it in the kernel's table
//! of sockets, then looks for that socket among the files the processes it
//! started hold. A caller that is no descendant of the launcher holds its
//! socket somewhere else, is found nowhere in the run, and is turned away.
//!
//! This binds an endpoint to its run without a secret the run would have to
//! carry. It is a rule among the guest's ordinary processes, not a wall
//! against the machine's own administrator: whoever is root in the machine
//! can enter any process of it. What an endpoint gives is bounded on the
//! host, where the run's person and their limits are known.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// The machine's processes and sockets, as its kernel lists them.
#[derive(Clone, Debug)]
pub struct Processes {
    root: PathBuf,
}

/// IPv4 loopback as the kernel's socket table writes it, also at the end of
/// its IPv4-mapped form.
const LOOPBACK: &str = "0100007F";

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

    /// Every process descended from `ancestor`, itself not among them.
    #[must_use]
    pub fn descendants(&self, ancestor: u32) -> Vec<u32> {
        let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        for entry in entries.flatten() {
            let Some(process) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse().ok())
            else {
                continue;
            };
            if let Some(parent) = self.parent(process) {
                children.entry(parent).or_default().push(process);
            }
        }
        let mut found = Vec::new();
        let mut next = vec![ancestor];
        while let Some(process) = next.pop() {
            for child in children.remove(&process).unwrap_or_default() {
                found.push(child);
                next.push(child);
            }
        }
        found
    }

    /// The socket a caller holds from loopback port `from` to loopback port
    /// `to`: its number among the kernel's files.
    #[must_use]
    pub fn socket_between(&self, from: u16, to: u16) -> Option<u64> {
        ["net/tcp", "net/tcp6"].into_iter().find_map(|table| {
            let table = std::fs::read_to_string(self.root.join(table)).ok()?;
            table.lines().skip(1).find_map(|row| {
                let mut fields = row.split_whitespace();
                let local = fields.nth(1)?;
                let remote = fields.next()?;
                let inode = fields.nth(6)?.parse::<u64>().ok()?;
                (loopback_port(local) == Some(from) && loopback_port(remote) == Some(to))
                    .then_some(inode)
                    .filter(|inode| *inode != 0)
            })
        })
    }

    /// Whether one of `processes` holds the socket numbered `inode`.
    #[must_use]
    pub fn holds(&self, processes: &[u32], inode: u64) -> bool {
        let wanted = format!("socket:[{inode}]");
        processes.iter().any(|process| {
            let Ok(files) = std::fs::read_dir(self.root.join(process.to_string()).join("fd"))
            else {
                return false;
            };
            files.flatten().any(|file| {
                std::fs::read_link(file.path())
                    .is_ok_and(|target| target.to_str() == Some(wanted.as_str()))
            })
        })
    }

    /// Whether the caller that connected from loopback port `from` to this
    /// run's port `to` is a process descended from `launcher`.
    #[must_use]
    pub fn of_the_run(&self, launcher: u32, from: u16, to: u16) -> bool {
        self.socket_between(from, to)
            .is_some_and(|inode| self.holds(&self.descendants(launcher), inode))
    }
}

/// The port of a socket table's `ADDRESS:PORT` field when its address is the
/// IPv4 loopback.
fn loopback_port(field: &str) -> Option<u16> {
    let (address, port) = field.rsplit_once(':')?;
    address
        .ends_with(LOOPBACK)
        .then(|| u16::from_str_radix(port, 16).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// A process `process` started by `parent`, holding the sockets `sockets`.
    fn process(root: &Path, process: u32, name: &str, parent: u32, sockets: &[u64]) {
        let at = root.join(process.to_string());
        std::fs::create_dir_all(at.join("fd")).unwrap();
        std::fs::write(
            at.join("stat"),
            format!("{process} ({name}) S {parent} {process} {process} 0 -1 4194560 100\n"),
        )
        .unwrap();
        for (number, socket) in sockets.iter().enumerate() {
            std::os::unix::fs::symlink(
                format!("socket:[{socket}]"),
                at.join("fd").join(number.to_string()),
            )
            .unwrap();
        }
        std::os::unix::fs::symlink("/dev/null", at.join("fd").join("90")).unwrap();
    }

    const TABLE: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:9C40 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 500 1 0000000000000000 100 0 0 10 0
   1: 0100007F:9C40 0100007F:C350 01 00000000:00000000 00:00000000 00000000  1000        0 501 1 0000000000000000 20 4 30 10 -1
   2: 0100007F:C350 0100007F:9C40 01 00000000:00000000 00:00000000 00000000  1000        0 777 1 0000000000000000 20 4 30 10 -1
   3: 0100007F:C351 0100007F:9C40 01 00000000:00000000 00:00000000 00000000  1000        0 888 1 0000000000000000 20 4 30 10 -1
   4: 0A00020F:C352 0100007F:9C40 01 00000000:00000000 00:00000000 00000000  1000        0 999 1 0000000000000000 20 4 30 10 -1
";

    fn machine() -> (tempfile::TempDir, Processes) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("net")).unwrap();
        std::fs::write(root.join("net/tcp"), TABLE).unwrap();
        std::fs::create_dir_all(root.join("self")).unwrap();
        process(root, 1, "init", 0, &[]);
        process(root, 100, "tilo-legacy-agent", 1, &[500, 501]);
        process(root, 101, "claude", 100, &[]);
        process(root, 102, "node (mcp) x", 101, &[777]);
        process(root, 200, "curl", 1, &[888]);
        let processes = Processes::at(root);
        (dir, processes)
    }

    tilo_test_env::scenario! {
        fn an_endpoint_knows_its_runs_own_processes;

        phase the_run_is_everything_the_launcher_started_however_deep;
        {
            let (_dir, processes) = machine();
            let mut run = processes.descendants(100);
            run.sort_unstable();
            assert_eq!(run, [101, 102], "a name with brackets and spaces is read to its end");
            assert!(processes.descendants(102).is_empty());
        }

        phase a_callers_socket_is_its_own_end_of_the_connection;
        {
            let (_dir, processes) = machine();
            assert_eq!(processes.socket_between(0xC350, 0x9C40), Some(777));
            assert_eq!(
                processes.socket_between(0x9C40, 0xC350),
                Some(501),
                "the other direction is the listener's end, another socket"
            );
            assert_eq!(processes.socket_between(0xC352, 0x9C40), None, "not from loopback");
            assert_eq!(processes.socket_between(0xC399, 0x9C40), None, "no such connection");
        }

        phase a_process_of_the_run_is_answered_and_any_other_is_not;
        {
            let (_dir, processes) = machine();
            assert!(processes.of_the_run(100, 0xC350, 0x9C40), "a grandchild of the launcher");
            assert!(
                !processes.of_the_run(100, 0xC351, 0x9C40),
                "another process in the machine holds its socket outside the run"
            );
            assert!(
                !processes.of_the_run(101, 0xC351, 0x9C40),
                "and is of no other run either"
            );
            assert!(!processes.of_the_run(100, 0xC399, 0x9C40), "a caller that cannot be found");
        }

        phase a_machine_that_cannot_be_read_answers_nobody;
        {
            let dir = tempfile::tempdir().unwrap();
            let processes = Processes::at(dir.path().join("absent"));
            assert!(processes.descendants(1).is_empty());
            assert!(!processes.of_the_run(1, 0xC350, 0x9C40));
        }
    }
}
