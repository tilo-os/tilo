//! Paths as the shell takes them: relative to its working directory, with
//! `.` and `..` resolved by name, as `cd` and the GNU tools' messages do.

use std::path::{Component, Path, PathBuf};

/// `path` made absolute against `cwd`, with `.` and `..` resolved by name.
pub fn join(cwd: &Path, path: &str) -> PathBuf {
    normalize(&cwd.join(path))
}

/// `path` without `.` segments, and with each `..` taking the segment before
/// it away. `..` at the root stays at the root.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let at_root = out.as_os_str() == "/";
                let climbing =
                    out.as_os_str().is_empty() || out.file_name().is_none() || out.ends_with("..");
                // `..` at the root stays at the root.
                if !at_root {
                    if climbing {
                        out.push("..");
                    } else {
                        out.pop();
                    }
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_dot_is_taken_by_name() {
        assert_eq!(join(Path::new("/a/b"), "../c"), PathBuf::from("/a/c"));
        assert_eq!(join(Path::new("/a"), "/x/./y"), PathBuf::from("/x/y"));
        assert_eq!(join(Path::new("/"), ".."), PathBuf::from("/"));
    }
}
