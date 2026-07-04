//! Target-disk discovery.
//!
//! We list block devices with:
//!
//! ```text
//! lsblk -dnpo NAME,SIZE,MODEL,TYPE
//! ```
//!
//! * `-d` — no partitions / holders (whole devices only).
//! * `-n` — no header row.
//! * `-p` — full device path (`/dev/sda`, not `sda`).
//! * `-o NAME,SIZE,MODEL,TYPE` — the four columns we care about.
//!
//! Only rows whose `TYPE` is exactly `disk` are kept (filters out `rom`,
//! `loop`, etc.). The `MODEL` column may be empty (multiple spaces) or
//! contain spaces — so the parser treats the first token as `NAME`, the
//! second as `SIZE`, the **last** as `TYPE`, and everything in between as
//! `MODEL`. That keeps us robust to multi-word model strings without
//! depending on fixed column widths.

use std::process::Command;

/// One installable target disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    /// Full device path, e.g. `/dev/sda`.
    pub name: String,
    /// Human-readable size as `lsblk` prints it, e.g. `238.5G`.
    pub size: String,
    /// Device model, e.g. `Samsung SSD 860 EVO`. May be empty.
    pub model: String,
}

impl Disk {
    /// Single-line label shown on the disk-selection row.
    pub fn label(&self) -> String {
        if self.model.is_empty() {
            format!("{}  —  {}", self.name, self.size)
        } else {
            format!("{}  —  {}  —  {}", self.name, self.size, self.model)
        }
    }
}

/// The `lsblk` argument vector. Kept as a constant so the call site and
/// any docs/tests reference exactly one source of truth.
pub const LSBLK_ARGS: [&str; 2] = ["-dnpo", "NAME,SIZE,MODEL,TYPE"];

/// Parse the output of `lsblk -dnpo NAME,SIZE,MODEL,TYPE` into the set of
/// whole-disk devices. Rows that aren't `TYPE == "disk"`, blank lines,
/// and malformed lines (fewer than the three mandatory columns) are
/// skipped.
pub fn parse_lsblk(output: &str) -> Vec<Disk> {
    let mut disks = Vec::new();
    for raw in output.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // NAME SIZE [MODEL ...] TYPE — at minimum NAME, SIZE, TYPE.
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() < 3 {
            continue;
        }
        let ty = *tokens.last().unwrap();
        if ty != "disk" {
            continue;
        }
        let name = tokens[0].to_string();
        let size = tokens[1].to_string();
        // Middle tokens (if any) are the model; empty when there are
        // exactly three tokens.
        let model = tokens[2..tokens.len() - 1].join(" ");
        disks.push(Disk { name, size, model });
    }
    disks
}

/// Run `lsblk` and parse its output. Returns an empty vec when `lsblk`
/// is missing or fails — the UI surfaces that as "No disks found".
pub fn read_disks() -> Vec<Disk> {
    match Command::new("lsblk").args(LSBLK_ARGS).output() {
        Ok(out) if out.status.success() => parse_lsblk(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiword_model() {
        let out = "/dev/sda 238.5G Samsung SSD 860 EVO disk\n";
        let disks = parse_lsblk(out);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].name, "/dev/sda");
        assert_eq!(disks[0].size, "238.5G");
        assert_eq!(disks[0].model, "Samsung SSD 860 EVO");
    }

    #[test]
    fn parses_empty_model() {
        // No model column: only NAME, SIZE, TYPE present.
        let out = "/dev/vda 64G disk\n";
        let disks = parse_lsblk(out);
        assert_eq!(disks.len(), 1);
        assert_eq!(disks[0].name, "/dev/vda");
        assert_eq!(disks[0].size, "64G");
        assert_eq!(disks[0].model, "");
    }

    #[test]
    fn filters_non_disk_types() {
        let out = "\
/dev/sda 238.5G Samsung SSD disk
/dev/sr0 1024M QEMU DVD-ROM rom
/dev/loop0 4K  loop
/dev/sdb 16G SanDisk Cruzer disk
";
        let disks = parse_lsblk(out);
        let names: Vec<&str> = disks.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["/dev/sda", "/dev/sdb"]);
    }

    #[test]
    fn skips_blank_and_malformed_lines() {
        let out = "\n/dev/sda 238.5G Samsung disk\n\ngarbage\n/dev/sdb\n/dev/sdc 1T  disk\n";
        let disks = parse_lsblk(out);
        // "garbage" (1 token) and "/dev/sdb" (1 token) are dropped;
        // /dev/sda and /dev/sdc (empty model) survive.
        let names: Vec<&str> = disks.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["/dev/sda", "/dev/sdc"]);
        assert_eq!(disks[1].model, "");
    }

    #[test]
    fn empty_output_yields_no_disks() {
        assert!(parse_lsblk("").is_empty());
        assert!(parse_lsblk("\n\n   \n").is_empty());
    }

    #[test]
    fn label_with_and_without_model() {
        let with = Disk {
            name: "/dev/sda".into(),
            size: "238.5G".into(),
            model: "Samsung SSD".into(),
        };
        assert_eq!(with.label(), "/dev/sda  —  238.5G  —  Samsung SSD");
        let without = Disk {
            name: "/dev/vda".into(),
            size: "64G".into(),
            model: String::new(),
        };
        assert_eq!(without.label(), "/dev/vda  —  64G");
    }
}
