//! Resolving a request against a runtime, without installing anything.
//!
//! `dnf install --assumeno` solves the transaction against the runtime's
//! repositories and rpm database and prints it; nothing is downloaded. The
//! printed table is the cache key's input. A transaction that would upgrade,
//! downgrade, replace or remove anything is refused: a tool may only add to
//! its runtime, never alter it.

use crate::key::normalize_nevra;

/// dnf's transaction-table sections that only add packages.
const ADDING: &[&str] = &[
    "Installing:",
    "Installing dependencies:",
    "Installing weak dependencies:",
    "Installing group/module packages:",
];

const CHANGING: &[&str] = &[
    "Upgrading:",
    "Downgrading:",
    "Reinstalling:",
    "Replacing:",
    "Removing:",
    "Removing dependent packages:",
    "Removing unused dependencies:",
    "Changing reason:",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub install: Vec<String>,
    /// Requested names the runtime already provides.
    pub already_installed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("no package matches {0:?}")]
    NoMatch(Vec<String>),
    #[error(
        "installing these would change the runtime ({section} {packages:?}); a tool can only add packages"
    )]
    ChangesRuntime {
        section: String,
        packages: Vec<String>,
    },
    #[error("dnf could not resolve the request:\n{0}")]
    Unsolvable(String),
}

/// Parse `dnf5 --assumeno install …` output (stdout and stderr together,
/// `LC_ALL=C`).
///
/// # Errors
///
/// [`ResolveError::NoMatch`] for requested names no repository has,
/// [`ResolveError::ChangesRuntime`] for a transaction that alters installed
/// packages, [`ResolveError::Unsolvable`] when dnf reports a dependency
/// problem.
pub fn parse_assumeno(output: &str) -> Result<Resolution, ResolveError> {
    let mut install = Vec::new();
    let mut already_installed = Vec::new();
    let mut no_match = Vec::new();
    let mut section: Option<&str> = None;
    let mut changing: Option<(String, Vec<String>)> = None;
    let mut problems = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(name) = quoted_after(trimmed, "No match for argument:") {
            no_match.push(name);
            continue;
        }
        if let Some(name) = already_installed_name(trimmed) {
            already_installed.push(name);
            continue;
        }
        if trimmed.starts_with("Problem") || (trimmed.starts_with("- ") && !problems.is_empty()) {
            problems.push(trimmed.to_owned());
            continue;
        }
        if !line.starts_with(' ') {
            section = ADDING
                .iter()
                .chain(CHANGING)
                .find(|s| trimmed == **s)
                .copied();
            if let Some(s) = section
                && CHANGING.contains(&s)
                && changing.is_none()
            {
                changing = Some((s.trim_end_matches(':').to_owned(), Vec::new()));
            }
            continue;
        }
        let Some(current) = section else { continue };
        let Some(nevra) = table_row(trimmed) else {
            continue;
        };
        if ADDING.contains(&current) {
            install.push(nevra);
        } else if let Some((_, packages)) = changing.as_mut() {
            packages.push(nevra);
        }
    }

    if !no_match.is_empty() {
        return Err(ResolveError::NoMatch(no_match));
    }
    if !problems.is_empty() {
        return Err(ResolveError::Unsolvable(problems.join("\n")));
    }
    if let Some((section, packages)) = changing {
        return Err(ResolveError::ChangesRuntime { section, packages });
    }
    install.sort();
    install.dedup();
    already_installed.sort();
    already_installed.dedup();
    Ok(Resolution {
        install,
        already_installed,
    })
}

/// A table row `name arch [epoch:]version-release repo size unit` as a NEVRA.
/// Sub-rows (`replacing foo …`) and anything else don't match.
fn table_row(row: &str) -> Option<String> {
    let cols: Vec<&str> = row.split_whitespace().collect();
    let [name, arch, evr, _repo, _size, _unit] = cols.as_slice() else {
        return None;
    };
    if *name == "replacing" || !evr.contains('-') {
        return None;
    }
    Some(normalize_nevra(&format!("{name}-{evr}.{arch}")))
}

/// `Package "jq-1.8.1-3.fc44.x86_64" is already installed.` → `jq-…`.
fn already_installed_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix("Package \"")?;
    let (name, tail) = rest.split_once('"')?;
    tail.contains("already installed").then(|| name.to_owned())
}

fn quoted_after(line: &str, prefix: &str) -> Option<String> {
    let rest = line.strip_prefix(prefix)?.trim();
    Some(rest.trim_matches('"').to_owned())
}

pub fn parse_rpm_list(output: &str) -> Vec<String> {
    let mut list: Vec<String> = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("gpg-pubkey-"))
        .map(normalize_nevra)
        .collect();
    list.sort();
    list.dedup();
    list
}

/// The query format [`parse_rpm_list`] reads: a NEVRA with the epoch only
/// when it is set, as dnf's table normalises to.
pub const RPM_QUERY_FORMAT: &str =
    "%{NAME}-%|EPOCH?{%{EPOCH}:}:{}|%{VERSION}-%{RELEASE}.%{ARCH}\\n";
