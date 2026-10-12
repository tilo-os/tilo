//! Native Flatpak resolves installed extensions (including conditional and
//! subdirectory extensions); do not replace its policy with ID heuristics.
use super::{Installation, InstalledRef, Runner};
use std::{
    collections::{BTreeSet, VecDeque},
    path::Path,
};

pub(in crate::setup::flatpak_transfer) fn review(
    runner: &impl Runner,
    home: &Path,
    scope: &Installation,
    installed: &[InstalledRef],
    selected: &[InstalledRef],
) -> Result<BTreeSet<String>, String> {
    let mut pending: VecDeque<_> = selected
        .iter()
        .filter(|item| &item.installation == scope)
        .map(|item| item.reference.clone())
        .collect();
    let mut required = BTreeSet::new();
    while let Some(reference) = pending.pop_front() {
        if !required.insert(reference.clone()) {
            continue;
        }
        if required.len() > 100_000 {
            return Err("Application dependency closure exceeds bounds".into());
        }
        require_installed(installed, scope, &reference)?;
        if reference.starts_with("app/") {
            let runtime = runner.run(
                home,
                &[
                    "info".into(),
                    scope.argument(),
                    "--show-runtime".into(),
                    reference.clone(),
                ],
            )?;
            let runtime = format!("runtime/{}", runtime.trim());
            require_installed(installed, scope, &runtime)?;
            pending.push_back(runtime);
        }
        let extensions = runner.run(
            home,
            &[
                "info".into(),
                scope.argument(),
                "--show-ref".into(),
                "--show-extensions".into(),
                reference.clone(),
            ],
        )?;
        for extension in parse_extensions(&extensions, &reference, scope)? {
            require_installed(installed, scope, &extension)?;
            pending.push_back(extension);
        }
    }
    Ok(required)
}
fn require_installed(
    installed: &[InstalledRef],
    scope: &Installation,
    reference: &str,
) -> Result<(), String> {
    if !super::super::reference(reference)
        || !installed
            .iter()
            .any(|item| &item.installation == scope && item.reference == reference)
    {
        return Err("Application runtime/extension needs a reviewed deployment in the same source installation".into());
    }
    Ok(())
}
fn parse_extensions(
    text: &str,
    reference: &str,
    scope: &Installation,
) -> Result<Vec<String>, String> {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    if lines.next() != Some(reference) {
        return Err("Invalid native application dependency response".into());
    }
    let mut result = Vec::new();
    let mut scope_verified = true;
    for line in lines {
        let (field, value) = line
            .split_once(':')
            .ok_or("Invalid native extension response")?;
        if field == "Extension" {
            if !scope_verified {
                return Err("Truncated native extension scope response".into());
            }
            let reference = value.trim();
            if !super::super::reference(reference) || !reference.starts_with("runtime/") {
                return Err(
                    "Unmaintained or invalid application extension cannot be carried offline"
                        .into(),
                );
            }
            result.push(reference.to_owned());
            scope_verified = false;
        } else if field == "Installation" {
            if result.is_empty()
                || scope_verified
                || super::super::installation(value.trim())? != *scope
            {
                return Err(
                    "Application extension needs its own reviewed source installation".into(),
                );
            }
            scope_verified = true;
        } else if !matches!(
            field,
            "ID" | "Origin" | "Commit" | "Installation" | "Installed Size" | "Subpaths"
        ) {
            return Err("Unsupported native extension response".into());
        }
    }
    if !scope_verified {
        return Err("Truncated native extension scope response".into());
    }
    Ok(result)
}
pub(super) fn verify_exported(
    required: &BTreeSet<String>,
    exported: &BTreeSet<String>,
) -> Result<(), String> {
    if let Some(missing) = required.difference(exported).next() {
        return Err(format!(
            "Offline export omitted required deployment {missing}; its repository may lack a collection ID or require extra downloaded data"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Cli;
    impl Runner for Cli {
        fn run(&self, _home: &Path, args: &[String]) -> Result<String, String> {
            let reference = args.last().unwrap();
            if args.contains(&"--show-runtime".into()) {
                return Ok("org.example.Platform/x86_64/stable".into());
            }
            if reference.starts_with("app/") {
                return Ok(format!(
                    "{reference}\n\n Extension: runtime/org.example.Paint.Locale/x86_64/stable\n ID: org.example.Paint.Locale\n Installation: system\n"
                ));
            }
            Ok(format!("{reference}\n"))
        }
    }
    #[test]
    fn native_dependency_closure_refuses_missing_runtime_or_extension_export() {
        let scope = Installation::System("default".into());
        let items: Vec<_> = [
            "app/org.example.Paint/x86_64/stable",
            "runtime/org.example.Platform/x86_64/stable",
            "runtime/org.example.Paint.Locale/x86_64/stable",
        ]
        .into_iter()
        .map(|reference| InstalledRef {
            reference: reference.into(),
            origin: "approved".into(),
            installation: scope.clone(),
            name: "Fixture".into(),
        })
        .collect();
        let required = review(
            &Cli,
            Path::new("/explicit-source"),
            &scope,
            &items,
            items.get(..1).unwrap(),
        )
        .unwrap();
        assert_eq!(required.len(), 3);
        for missing in &required {
            let mut exported = required.clone();
            exported.remove(missing);
            assert!(
                verify_exported(&required, &exported)
                    .unwrap_err()
                    .contains(missing)
            );
        }
        verify_exported(&required, &required).unwrap();
        assert!(
            review(
                &Cli,
                Path::new("/explicit-source"),
                &scope,
                items.get(..2).unwrap(),
                items.get(..1).unwrap()
            )
            .is_err()
        );
    }
    #[test]
    fn native_extension_output_checks_actual_scope_and_truncation() {
        let app = "app/org.example.Paint/x86_64/stable";
        // Exact Flatpak1.18.4 info.c475-485 labels; leading alignment is emitted
        // by print_aligned in builtins-utils.c1111-1121, not part of the value.
        let output = format!(
            "{app}\n\n     Extension: runtime/org.example.Paint.Locale/x86_64/stable\n            ID: org.example.Paint.Locale\n        Origin: approved\n        Commit: {}\n  Installation: system\nInstalled Size: 12.0 kB\n      Subpaths: /en,/fr\n",
            "a".repeat(64)
        );
        assert_eq!(
            parse_extensions(
                &output.replace("Installation: system", "Installation: system (work)"),
                app,
                &Installation::System("work".into())
            )
            .unwrap(),
            vec!["runtime/org.example.Paint.Locale/x86_64/stable"]
        );
        let scope = Installation::System("default".into());
        assert_eq!(
            parse_extensions(&output, app, &scope).unwrap(),
            vec!["runtime/org.example.Paint.Locale/x86_64/stable"]
        );
        assert!(
            parse_extensions(
                &output.replace("Installation: system", "Installation: user"),
                app,
                &scope
            )
            .is_err()
        );
        assert!(
            parse_extensions(&output.replace("  Installation: system\n", ""), app, &scope).is_err()
        );
    }
}
