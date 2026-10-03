//! App sets for `tilo-vm slot apps`: named lists of third-party packages
//! for verifying desktop features, read from `data/app-sets.toml`.

use serde::Deserialize;

const SETS: &str = include_str!("../data/app-sets.toml");

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AppSet {
    pub name: String,
    pub summary: String,
    #[serde(rename = "app")]
    pub apps: Vec<App>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct App {
    /// The distribution package.
    pub package: String,
    /// What it verifies.
    pub checks: String,
    /// How to start it.
    #[serde(rename = "try")]
    pub try_command: String,
}

#[derive(Deserialize)]
struct File {
    #[serde(rename = "set")]
    sets: Vec<AppSet>,
}

/// Every set, in file order.
///
/// # Panics
/// Never at run time: the file is compiled in and a unit test parses it.
#[expect(
    clippy::expect_used,
    reason = "compiled-in data that a unit test parses"
)]
pub fn all() -> Vec<AppSet> {
    toml::from_str::<File>(SETS)
        .map(|file| file.sets)
        .expect("data/app-sets.toml parses")
}

/// The set called `name`.
///
/// # Errors
/// The names that do exist, for the error message.
pub fn find(name: &str) -> Result<AppSet, Vec<String>> {
    let sets = all();
    sets.iter()
        .find(|set| set.name == name)
        .cloned()
        .ok_or_else(|| sets.into_iter().map(|set| set.name).collect())
}

/// A set as `slot apps list` prints it.
pub fn describe(set: &AppSet) -> String {
    let width = set
        .apps
        .iter()
        .map(|app| app.package.len())
        .max()
        .unwrap_or(0);
    let mut lines = vec![format!("{}: {}", set.name, set.summary)];
    for app in &set.apps {
        lines.push(format!(
            "  {:width$}  {}\n  {:width$}  try: {}",
            app.package, app.checks, "", app.try_command
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_parse_and_name_real_package_shapes() {
        let sets = all();
        assert!(!sets.is_empty());
        for set in &sets {
            assert!(!set.apps.is_empty(), "{} is empty", set.name);
            for app in &set.apps {
                assert!(
                    tilo_vm_protocol::valid_package_name(&app.package),
                    "{}",
                    app.package
                );
                assert!(!app.checks.is_empty() && !app.try_command.is_empty());
            }
        }
        let verification = find("verification").unwrap();
        let packages: Vec<&str> = verification
            .apps
            .iter()
            .map(|a| a.package.as_str())
            .collect();
        assert!(packages.contains(&"xterm") && packages.contains(&"zenity"));
        assert_eq!(find("nope").unwrap_err(), ["verification"]);
        assert!(describe(&verification).contains("try: xterm"));
    }
}
