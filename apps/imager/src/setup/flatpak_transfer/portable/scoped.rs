//! Package-local metadata adapter for the existing durable app-only transaction.
//! The original system roots are represented by authenticated offline repositories;
//! only copied user refs are queried natively, always with explicit --user scope.
use super::{ApplicationsManifest, Deployment, Installation, InstalledRef, OfflineRunner, Runner};
use std::{collections::BTreeMap, path::Path, sync::atomic::AtomicBool};
pub(super) struct PackageRunner<'a, R> {
    native: &'a R,
    root: &'a Path,
    source: &'a Path,
    manifest: &'a ApplicationsManifest,
    cancel: &'a AtomicBool,
}
impl<'a, R: OfflineRunner> PackageRunner<'a, R> {
    pub fn new(
        native: &'a R,
        root: &'a Path,
        source: &'a Path,
        manifest: &'a ApplicationsManifest,
        cancel: &'a AtomicBool,
    ) -> Self {
        Self {
            native,
            root,
            source,
            manifest,
            cancel,
        }
    }
    fn user_refs(&self) -> Result<BTreeMap<String, String>, String> {
        let mut refs = BTreeMap::new();
        for (kind, flag) in [("app", "--app"), ("runtime", "--runtime")] {
            let output = self.native.run(
                self.source,
                &[
                    "list".into(),
                    "--user".into(),
                    flag.into(),
                    "--columns=ref:f,origin:f,installation:f,name:f".into(),
                ],
            )?;
            for line in output.lines() {
                let reference = line.split('\t').next().ok_or("Invalid copied user ref")?;
                let reference = format!("{kind}/{reference}");
                if !super::super::reference(&reference) {
                    return Err("Invalid copied user ref".into());
                }
                let value = self.native.run(
                    self.source,
                    &[
                        "info".into(),
                        "--user".into(),
                        "--show-commit".into(),
                        reference.clone(),
                    ],
                )?;
                refs.insert(reference, value.trim().into());
            }
        }
        Ok(refs)
    }
    pub fn selected_refs(&self) -> Result<Vec<InstalledRef>, String> {
        let users = self.user_refs()?;
        let mut selected = Vec::new();
        for deployment in &self.manifest.deployments {
            match users.get(&deployment.installed.reference) {
                Some(commit) if commit == &deployment.commit => {}
                Some(_) => {
                    return Err(
                        "Copied user app/runtime differs from the selected system version".into(),
                    );
                }
                None => selected.push(deployment.installed.clone()),
            }
        }
        Ok(selected)
    }
    fn source_query(&self, arguments: &[String]) -> Result<String, String> {
        match arguments.first().map(String::as_str) {
            Some("list") => {
                let kind = if arguments.contains(&"--app".into()) {
                    "app/"
                } else {
                    "runtime/"
                };
                let mut native = arguments.to_vec();
                native.retain(|arg| arg != "--all");
                native.push("--user".into());
                let mut result = self.native.run(self.source, &native)?;
                for item in self
                    .manifest
                    .deployments
                    .iter()
                    .filter(|item| item.installed.reference.starts_with(kind))
                {
                    let reference = item
                        .installed
                        .reference
                        .strip_prefix(kind)
                        .ok_or("Invalid authenticated ref")?;
                    let scope = match &item.installed.installation {
                        Installation::System(name) if name == "default" => "system",
                        Installation::System(name) => name,
                        Installation::User => return Err("Invalid portable system scope".into()),
                    };
                    result.push_str(&format!(
                        "{reference}\t{}\t{scope}\t{}\n",
                        item.installed.origin, item.installed.name
                    ));
                }
                Ok(result)
            }
            Some("remotes" | "info") if arguments.contains(&"--user".into()) => {
                self.native.run(self.source, arguments)
            }
            Some("remotes") => {
                let mut output = String::new();
                for trust in self
                    .manifest
                    .trust
                    .iter()
                    .filter(|trust| arguments.contains(&trust.source.installation.argument()))
                {
                    output.push_str(&format!(
                        "{}\t{}\t{}\n",
                        trust.source.name, trust.source.url, trust.source.options
                    ));
                }
                Ok(output)
            }
            Some("info") => {
                let item = self.lookup(arguments)?;
                if arguments.contains(&"--show-location".into()) {
                    let path = self
                        .root
                        .join(&item.repository)
                        .join(".ostree")
                        .join(&item.installed.reference)
                        .join(&item.commit);
                    return path
                        .to_str()
                        .map(str::to_owned)
                        .ok_or("Offline metadata path is not UTF8".into());
                }
                if arguments.contains(&"--show-commit".into()) {
                    return Ok(item.commit.clone());
                }
                Err("Unsupported authenticated system metadata query".into())
            }
            _ => Err("Offline source metadata cannot mutate the source".into()),
        }
    }
    fn lookup(&self, arguments: &[String]) -> Result<&Deployment, String> {
        let reference = arguments.last().ok_or("Missing portable ref")?;
        self.manifest
            .deployments
            .iter()
            .find(|item| &item.installed.reference == reference)
            .ok_or("Requested ref is outside the authenticated portable selection".into())
    }
}
impl<R: OfflineRunner> Runner for PackageRunner<'_, R> {
    fn run(&self, home: &Path, arguments: &[String]) -> Result<String, String> {
        if home == self.source {
            return self.source_query(arguments);
        }
        if arguments.first().is_some_and(|verb| verb == "install") {
            let item = self.lookup(arguments)?;
            let remote = arguments
                .get(
                    arguments
                        .len()
                        .checked_sub(2)
                        .ok_or("Missing target authority")?,
                )
                .ok_or("Missing target authority")?;
            super::restore::deploy(self.native, self.root, item, home, remote, self.cancel)
                .map_err(|e| e.to_string())?;
            return Ok(String::new());
        }
        self.native.run(home, arguments)
    }
    fn verify_repository(&self, home: &Path) -> Result<(), String> {
        self.native.verify_repository(home)
    }
}
