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

/// Sections that would change the runtime.
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
    /// NEVRAs the transaction installs, in [`normalize_nevra`] form.
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

/// Parse `rpm -qa --qf '%{NEVRA}\n'`-style output into a sorted set.
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

#[cfg(test)]
mod tests {
    use super::*;

    const PANDOC_JQ: &str = "\
Updating and loading repositories:
 Fedora 44 - x86_64                     100% |   2.4 MiB/s |  36.7 MiB |  00m15s
Repositories loaded.
Package        Arch   Version           Repository      Size
Installing:
 jq            x86_64 0:1.8.1-3.fc44    updates    465.4 KiB
 pandoc-cli    x86_64 0:3.7.0.2-40.fc44 fedora     204.2 MiB
Installing dependencies:
 numactl-libs  x86_64 0:2.0.19-4.fc44   fedora      56.8 KiB
 oniguruma     x86_64 0:6.9.10-4.fc44   fedora     770.9 KiB
 pandoc-common noarch 0:3.7.0.2-39.fc44 fedora       2.0 MiB

Transaction Summary:
 Installing:         5 packages

Total size of inbound packages is 30 MiB. Need to download 30 MiB.
After this operation, 207 MiB extra will be used (install 207 MiB, remove 0 B).
Operation aborted by the user.
";

    #[test]
    fn a_transaction_table_resolves_to_normalized_nevras() {
        let r = parse_assumeno(PANDOC_JQ).unwrap();
        assert_eq!(
            r.install,
            [
                "jq-1.8.1-3.fc44.x86_64",
                "numactl-libs-2.0.19-4.fc44.x86_64",
                "oniguruma-6.9.10-4.fc44.x86_64",
                "pandoc-cli-3.7.0.2-40.fc44.x86_64",
                "pandoc-common-3.7.0.2-39.fc44.noarch",
            ]
        );
        assert!(r.already_installed.is_empty());
    }

    #[test]
    fn the_summary_section_is_not_a_package() {
        // " Installing:         5 packages" is indented under "Transaction
        // Summary:", which is not an adding section.
        let r = parse_assumeno(PANDOC_JQ).unwrap();
        assert!(!r.install.iter().any(|n| n.contains("packages")));
    }

    #[test]
    fn already_installed_names_are_reported_not_installed() {
        let out = "\
Package \"bash-5.3.0-2.fc44.x86_64\" is already installed.

Nothing to do.
";
        let r = parse_assumeno(out).unwrap();
        assert!(r.install.is_empty());
        assert_eq!(r.already_installed, ["bash-5.3.0-2.fc44.x86_64"]);
    }

    #[test]
    fn unknown_names_fail() {
        let out = "Failed to resolve the transaction:\nNo match for argument: nosuchpkg\n";
        assert_eq!(
            parse_assumeno(out),
            Err(ResolveError::NoMatch(vec!["nosuchpkg".into()]))
        );
    }

    #[test]
    fn upgrades_of_runtime_packages_are_refused() {
        let out = "\
Package        Arch   Version           Repository      Size
Upgrading:
 glibc         x86_64 0:2.43-5.fc44     updates      6.6 MiB
   replacing   glibc  x86_64 0:2.43-4.fc44 @System   6.6 MiB
Installing:
 tree          x86_64 0:2.2.1-4.fc44    fedora     121.4 KiB
";
        match parse_assumeno(out) {
            Err(ResolveError::ChangesRuntime { section, packages }) => {
                assert_eq!(section, "Upgrading");
                assert_eq!(packages, ["glibc-2.43-5.fc44.x86_64"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn dependency_problems_fail_with_dnfs_explanation() {
        let out = "\
Failed to resolve the transaction:
Problem: conflicting requests
  - nothing provides libfoo.so.1()(64bit) needed by bar-1-1.fc44.x86_64
";
        assert!(matches!(
            parse_assumeno(out),
            Err(ResolveError::Unsolvable(text)) if text.contains("libfoo")
        ));
    }

    #[test]
    fn rpm_lists_drop_keys_and_normalize() {
        let out =
            "bash-5.3.0-2.fc44.x86_64\ngpg-pubkey-abc-def\nvim-enhanced-2:9.1-1.fc44.x86_64\n\n";
        assert_eq!(
            parse_rpm_list(out),
            [
                "bash-5.3.0-2.fc44.x86_64",
                "vim-enhanced-2:9.1-1.fc44.x86_64"
            ]
        );
    }
}
