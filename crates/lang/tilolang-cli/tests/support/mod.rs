//! What the command's tests share: the packages they build, copied into a scratch directory.

use std::path::{Path, PathBuf};

/// The packages `apps/tl`.
pub fn packages() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl")
}

/// The packages `names` stand on: each one, and every package beside it that it `use`s, however
/// deep, as the `use` lines of its `.tl` files name them — the directories a build of `names`
/// reads.
#[allow(dead_code, reason = "not every test binary copies packages")]
pub fn with_used(names: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut todo: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    while let Some(name) = todo.pop() {
        if out.contains(&name) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(packages().join(&name)) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.path().extension().is_none_or(|ext| ext != "tl") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            for line in source.lines() {
                let Some(rest) = line.trim().strip_prefix("use ") else {
                    continue;
                };
                let Some(id) = rest.split_whitespace().next() else {
                    continue;
                };
                let Some(used) = id.rsplit('.').next() else {
                    continue;
                };
                todo.push(used.to_owned());
            }
        }
        out.push(name);
    }
    out.sort();
    out
}

/// Copy the package at `from` into `to`, created.
pub fn copy_package(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_package(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
