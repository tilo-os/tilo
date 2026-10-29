//! Pathname expansion: `*.rs`, `src/*/mod.rs`, read through the host, so a
//! pattern only ever sees what the environment may read.

use std::path::PathBuf;

use crate::pattern::{PatChar, Pattern};
use crate::shell::Shell;

/// How many paths one pattern may name before expansion stops looking. A
/// pattern is a person's or a script's request, so this only stops a
/// mistaken `/**/*/*/*` from reading the whole tree.
const MAX_MATCHES: usize = 100_000;

/// The paths `pattern` names, sorted; none when nothing matches.
pub(crate) fn glob(shell: &Shell, pattern: &[PatChar]) -> Vec<String> {
    let absolute = pattern.first().map(|p| p.0) == Some('/');
    // Split at unquoted and quoted slashes alike: a slash is never part of
    // a name.
    let mut parts: Vec<Vec<PatChar>> = vec![Vec::new()];
    for &pc in pattern {
        if pc.0 == '/' {
            parts.push(Vec::new());
        } else if let Some(last) = parts.last_mut() {
            last.push(pc);
        }
    }
    // (the text as written so far, the path on the host)
    let start_text = if absolute {
        String::from("/")
    } else {
        String::new()
    };
    let start_path = if absolute {
        PathBuf::from("/")
    } else {
        shell.cwd.clone()
    };
    let mut current = vec![(start_text, start_path)];
    let count = parts.len();
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            // `a//b` or the leading `/`.
            continue;
        }
        let last = i.saturating_add(1) == count;
        let pat = Pattern::new(part);
        let mut next = Vec::new();
        for (text, path) in &current {
            let join = |name: &str| {
                let shown = if text.is_empty() || text.ends_with('/') {
                    format!("{text}{name}")
                } else {
                    format!("{text}/{name}")
                };
                (shown, path.join(name))
            };
            if pat.is_literal() {
                let name = pat.literal();
                let candidate = join(&name);
                if shell.host.symlink_metadata(&candidate.1).is_ok() {
                    next.push(candidate);
                }
                continue;
            }
            let Ok(entries) = shell.host.read_dir(path) else {
                continue;
            };
            let mut names: Vec<String> = entries
                .into_iter()
                .map(|e| e.name)
                .filter(|n| {
                    (!n.starts_with('.')
                        || pat.starts_with_dot()
                        || (shell.options.dotglob && n != "." && n != ".."))
                        && pat.matches(n)
                })
                .collect();
            names.sort();
            for name in names {
                let candidate = join(&name);
                if !last && !shell.host.metadata(&candidate.1).is_ok_and(|m| m.is_dir()) {
                    continue;
                }
                next.push(candidate);
                if next.len() >= MAX_MATCHES {
                    break;
                }
            }
        }
        current = next;
        if current.is_empty() {
            return Vec::new();
        }
    }
    let trailing_slash = pattern.last().map(|p| p.0) == Some('/');
    let mut out: Vec<String> = current
        .into_iter()
        .map(|(text, _)| {
            if trailing_slash {
                format!("{text}/")
            } else {
                text
            }
        })
        .collect();
    out.sort();
    out
}
