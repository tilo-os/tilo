//! Flatpak's local `deploy/<ref>` GC roots are aliases, not signed logical names.
//! Check their logical binding without deleting or rewriting repository refs.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::Path,
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub(super) fn verify(repository: &Path, cancel: Option<&AtomicBool>) -> Result<(), String> {
    let normal = run(repository, &["refs", "--revision"], cancel)?;
    let collections = run(repository, &["refs", "--collections", "--revision"], cancel)?;
    let mut bindings = BTreeMap::new();
    for line in normal.lines() {
        let (name, checksum) = row(line)?;
        let logical = logical_ref(name)?;
        verify_ref(repository, logical, checksum, &mut bindings, cancel)?;
    }
    for line in collections.lines() {
        let (name, checksum) = row(line)?;
        let (collection, logical) = name
            .strip_prefix('(')
            .and_then(|value| value.strip_suffix(')'))
            .and_then(|value| value.split_once(", "))
            .ok_or("Invalid collection ref response")?;
        if !safe_name(collection) || !safe_name(logical) {
            return Err("Invalid collection ref name".into());
        }
        verify_ref(repository, logical, checksum, &mut bindings, cancel)?;
        let actual = run(
            repository,
            &[
                "show",
                "--print-metadata-key=ostree.collection-binding",
                checksum,
            ],
            cancel,
        )?;
        if quoted(actual.trim())? != collection {
            return Err("Repository collection binding differs".into());
        }
    }
    if run(repository, &["refs", "--revision"], cancel)? != normal
        || run(repository, &["refs", "--collections", "--revision"], cancel)? != collections
    {
        return Err("Repository refs changed during verification".into());
    }
    Ok(())
}

fn verify_ref(
    repository: &Path,
    logical: &str,
    checksum: &str,
    cache: &mut BTreeMap<String, Vec<String>>,
    cancel: Option<&AtomicBool>,
) -> Result<(), String> {
    if !cache.contains_key(checksum) {
        let metadata = run(
            repository,
            &["show", "--print-metadata-key=ostree.ref-binding", checksum],
            cancel,
        )?;
        cache.insert(checksum.into(), string_array(&metadata)?);
    }
    if !cache
        .get(checksum)
        .is_some_and(|values| values.iter().any(|value| value == logical))
    {
        return Err("Repository commit is not bound to its logical ref".into());
    }
    Ok(())
}

fn logical_ref(name: &str) -> Result<&str, String> {
    let (remote, logical) = match name.split_once(':') {
        Some((remote, logical)) if super::super::token(remote) => (Some(remote), logical),
        Some(_) => return Err("Invalid repository remote ref".into()),
        None => (None, name),
    };
    if let Some(alias) = logical.strip_prefix("deploy/") {
        if remote.is_some() || !super::super::reference(alias) {
            return Err("Invalid Flatpak deployment alias".into());
        }
        return Ok(alias);
    }
    if !safe_name(logical) {
        return Err("Invalid repository logical ref".into());
    }
    Ok(logical)
}
fn row(line: &str) -> Result<(&str, &str), String> {
    let (name, checksum) = line
        .split_once('\t')
        .ok_or("Invalid repository ref response")?;
    if checksum.len() != 64 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Invalid repository commit checksum".into());
    }
    Ok((name, checksum))
}
fn safe_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 1024 && value.split('/').all(super::super::token)
}
fn quoted(value: &str) -> Result<&str, String> {
    let value = value
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
        .ok_or("Invalid repository string metadata")?;
    if !safe_name(value) {
        return Err("Unsupported repository string metadata".into());
    }
    Ok(value)
}
pub(in crate::setup::flatpak_transfer) fn string_array(value: &str) -> Result<Vec<String>, String> {
    let value = value.trim();
    let value = value.strip_prefix("@as ").unwrap_or(value);
    let contents = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .ok_or("Invalid repository ref binding metadata")?;
    if contents.trim().is_empty() {
        return Ok(Vec::new());
    }
    contents
        .split(',')
        .map(|value| quoted(value.trim()).map(str::to_owned))
        .collect()
}
fn run(
    repository: &Path,
    arguments: &[&str],
    cancel: Option<&AtomicBool>,
) -> Result<String, String> {
    let mut option = OsString::from("--repo=");
    option.push(repository.as_os_str());
    let mut command = Command::new("/usr/bin/ostree");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .arg(option)
        .args(arguments);
    let mut worker =
        bounded_command::spawn_prepared(command, None, 4 * 1024 * 1024, Duration::from_secs(30))
            .map_err(|_| "Repository binding verification could not start")?;
    loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err("Repository binding verification cancelled".into());
        }
        if let Some(result) = worker.try_wait() {
            let output = result.map_err(|_| "Repository binding response exceeded bounds")?;
            if !output.status.success() {
                #[cfg(test)]
                if std::env::var("TILO_NATIVE_FLATPAK_FIXTURE_DIAGNOSTICS").as_deref() == Ok("1") {
                    eprintln!(
                        "Native binding {arguments:?}: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                return Err("Repository commit binding verification failed".into());
            }
            return String::from_utf8(output.stdout)
                .map_err(|_| "Invalid repository response".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deployment_aliases_require_exact_canonical_logical_names() {
        let logical = "runtime/org.tilo.Fixture/x86_64/stable";
        assert_eq!(logical_ref(&format!("deploy/{logical}")).unwrap(), logical);
        assert_eq!(logical_ref(&format!("trusted:{logical}")).unwrap(), logical);
        for invalid in [
            "deploy/unknown",
            "trusted:deploy/app/a/x/b",
            "deploy/app/a/x/b/extra",
            "deploy/../runtime/a/x/b",
        ] {
            assert!(logical_ref(invalid).is_err(), "{invalid}");
        }
        assert_eq!(
            string_array("['runtime/org.tilo.Fixture/x86_64/stable']\n").unwrap(),
            vec![logical]
        );
        for invalid in [
            "'runtime/a/x/b'",
            "['runtime/a/x/b', 1]",
            "['runtime/a/x/b\\\\']",
        ] {
            assert!(string_array(invalid).is_err());
        }
    }
}
