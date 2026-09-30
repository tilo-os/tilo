//! See task-4-probe-report.md for the external controller contract.
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Write},
    path::{Component, Path, PathBuf},
};
use tilo_files::native::atomic::directory_probe::{
    self, AnchorEvidence, Candidate, ParentAccess, Request,
};

const HELP: &str = "directory_commit_probe --root ABSOLUTE_TEST_ROOT --profile local-ntfs --anchor-evidence EXTERNAL_JSON --report EXTERNAL_NEW_JSON --case direct|staged|collision|replay|reparse|denied --access minimal|writable [--component NAME ...]\nAll objects are retained. stdout is flushed JSONL for the external controller. A successful probe is never crash-durability qualification. Replay accepts 1..16 existing components. Reparse/denied require one controller-provisioned fixture component.";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    volume_serial: u64,
    file_id: String,
    establishment_record: String,
    storage_profile: String,
}
struct Arguments {
    root: PathBuf,
    profile: String,
    evidence: PathBuf,
    report: PathBuf,
    candidate: Candidate,
    access: ParentAccess,
    components: Vec<String>,
}
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
fn parse(args: impl IntoIterator<Item = String>) -> io::Result<Arguments> {
    let mut args = args.into_iter();
    let mut values = BTreeMap::new();
    let mut components = Vec::new();
    while let Some(key) = args.next() {
        if !matches!(
            key.as_str(),
            "--root"
                | "--profile"
                | "--anchor-evidence"
                | "--report"
                | "--case"
                | "--access"
                | "--component"
        ) {
            return Err(invalid(format!("unknown argument {key}")));
        }
        let value = args
            .next()
            .ok_or_else(|| invalid(format!("missing value for {key}")))?;
        if key == "--component" {
            components.push(value);
        } else if values.insert(key, value).is_some() {
            return Err(invalid("duplicate argument"));
        }
    }
    let mut get = |key: &str| {
        values
            .remove(key)
            .ok_or_else(|| invalid(format!("required {key}")))
    };
    let root = PathBuf::from(get("--root")?);
    let report = PathBuf::from(get("--report")?);
    let evidence = PathBuf::from(get("--anchor-evidence")?);
    if !root.is_absolute() || !report.is_absolute() || !evidence.is_absolute() {
        return Err(invalid(
            "root, report and anchor evidence paths must be absolute",
        ));
    }
    if root
        .components()
        .chain(report.components())
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid("dot traversal in paths refused"));
    }
    let profile = get("--profile")?;
    if profile != "local-ntfs" {
        return Err(invalid("explicit local-ntfs profile required"));
    }
    let candidate = match get("--case")?.as_str() {
        "direct" => Candidate::Direct,
        "staged" => Candidate::Staged,
        "collision" => Candidate::Collision,
        "replay" => Candidate::Replay,
        "reparse" => Candidate::Reparse,
        "denied" => Candidate::Denied,
        _ => return Err(invalid("unknown case")),
    };
    let access = match get("--access")?.as_str() {
        "minimal" => ParentAccess::Minimal,
        "writable" => ParentAccess::Writable,
        _ => return Err(invalid("unknown access mask")),
    };
    Ok(Arguments {
        root,
        profile,
        evidence,
        report,
        candidate,
        access,
        components,
    })
}
fn evidence(text: &str) -> io::Result<AnchorEvidence> {
    let value: Evidence = serde_json::from_str(text).map_err(|error| invalid(error.to_string()))?;
    if value.file_id.len() != 32 || !value.file_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid(
            "file_id must contain exactly 32 hexadecimal digits",
        ));
    }
    let mut id = [0u8; 16];
    for (slot, pair) in id.iter_mut().zip(value.file_id.as_bytes().chunks_exact(2)) {
        let pair = std::str::from_utf8(pair).map_err(|_| invalid("invalid identity"))?;
        *slot = u8::from_str_radix(pair, 16).map_err(|_| invalid("invalid identity"))?;
    }
    if value.establishment_record.trim().is_empty() || value.storage_profile.trim().is_empty() {
        return Err(invalid(
            "external establishment record and storage profile required",
        ));
    }
    Ok(AnchorEvidence {
        volume_serial: value.volume_serial,
        file_id: id,
        establishment_record: value.establishment_record,
        storage_profile: value.storage_profile,
    })
}
fn under(path: &Path, root: &Path) -> bool {
    // Windows names are case-insensitive in the admitted fixture profile. Prefix
    // comparison remains component-based, so test-root-other is not test-root.
    let p: Vec<_> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let r: Vec<_> = root
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    p.starts_with(&r)
}
fn external_report(path: &Path, root: &Path) -> io::Result<File> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("report parent required"))?;
    // Controller-owned output directory must already exist and contain no link
    // components. create_new refuses an existing destination (including links).
    for ancestor in parent.ancestors() {
        let meta = fs::symlink_metadata(ancestor)?;
        if meta.file_type().is_symlink() {
            return Err(invalid("report ancestor link refused"));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(invalid("report ancestor reparse point refused"));
            }
        }
    }
    let parent = fs::canonicalize(parent)?;
    let root = fs::canonicalize(root)?;
    if under(&parent, &root) {
        return Err(invalid("report must be outside test namespace"));
    }
    File::options().write(true).create_new(true).open(path)
}
fn execute(args: Arguments) -> io::Result<bool> {
    let evidence_path = fs::canonicalize(&args.evidence)?;
    let root_path = fs::canonicalize(&args.root)?;
    if under(&evidence_path, &root_path) {
        return Err(invalid(
            "controller evidence must be stored outside the test namespace",
        ));
    }
    let metadata = fs::metadata(&evidence_path)?;
    if !metadata.is_file() || metadata.len() > 65536 {
        return Err(invalid("bounded regular evidence file required"));
    }
    let request = Request {
        root: args.root,
        profile: args.profile,
        evidence: evidence(&fs::read_to_string(&evidence_path)?)?,
        candidate: args.candidate,
        parent_access: args.access,
        components: args.components,
    };
    request.validate()?;
    let mut report = external_report(&args.report, &request.root)?;
    let mut events = Vec::<Value>::new();
    let stdout = io::stdout();
    let mut stream = stdout.lock();
    let outcome = directory_probe::run(&request, &mut |event| {
        let value =
            json!({"sequence":event.sequence,"operation":event.operation,"fields":event.fields});
        serde_json::to_writer(&mut stream, &value).map_err(io::Error::other)?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        events.push(value);
        Ok(())
    });
    let result = json!({"schema":1,"candidate":format!("{:?}",request.candidate),"parent_access":format!("{:?}",request.parent_access),"root":request.root,"events":events,"fully_observed":outcome.fully_observed,"candidate_calls_succeeded":outcome.candidate_calls_succeeded,"terminal":outcome.terminal,"qualification":outcome.qualification,"outstanding_prerequisites":["external abrupt guest/storage power-loss matrix and cache-loss evidence","production initial anchor protocol","native OS/architecture/storage/filter qualification beyond this selected case"],"fixture_anchor":"external controller premise, not proven by this process","cleanup_owner":"external disposable controller"});
    serde_json::to_writer_pretty(&mut report, &result).map_err(io::Error::other)?;
    report.write_all(b"\n")?;
    report.flush()?;
    Ok(outcome.fully_observed && outcome.candidate_calls_succeeded)
}
fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--help"] {
        println!("{HELP}");
        return;
    }
    let code = match parse(args).and_then(execute) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            eprintln!(
                "{}",
                json!({"terminal":"input_or_report_failure","error":e.to_string(),"qualification":"unqualified"})
            );
            2
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(root: &Path, evidence: &Path, report: &Path) -> Arguments {
        Arguments {
            root: root.into(),
            profile: "local-ntfs".into(),
            evidence: evidence.into(),
            report: report.into(),
            candidate: Candidate::Direct,
            access: ParentAccess::Writable,
            components: vec![],
        }
    }
    #[test]
    fn controller_evidence_inside_test_namespace_refuses_without_report_effect() {
        let fixture = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(fixture.path()).unwrap();
        let external = fs::canonicalize(external.path()).unwrap();
        let evidence = root.join("anchor.json");
        let report = external.join("report.json");
        fs::write(&evidence,r#"{"volume_serial":1,"file_id":"000102030405060708090a0b0c0d0e0f","establishment_record":"controller-record","storage_profile":"disposable-vm"}"#).unwrap();
        assert!(execute(args(&root, &evidence, &report)).is_err());
        assert!(!report.exists());
    }
    #[test]
    fn missing_and_malformed_arguments_or_evidence_are_refused() {
        assert!(parse(Vec::new()).is_err());
        assert!(parse(["--root".into()]).is_err());
        assert!(parse(["--unknown".into(), "value".into()]).is_err());
        for text in [
            "{}",
            "not json",
            r#"{"volume_serial":1,"file_id":"0","establishment_record":"x","storage_profile":"x"}"#,
            r#"{"volume_serial":1,"file_id":"000102030405060708090a0b0c0d0e0f","establishment_record":"","storage_profile":"x"}"#,
        ] {
            assert!(evidence(text).is_err());
        }
    }
    #[test]
    fn report_refuses_existing_file_and_under_test_namespace() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let root_path = fs::canonicalize(root.path()).unwrap();
        let external_path = fs::canonicalize(external.path()).unwrap();
        let report = external_path.join("existing");
        let fresh = external_path.join("fresh");
        assert!(external_report(&fresh, &root_path).is_ok());
        assert!(fresh.exists());
        fs::write(&report, b"preserve").unwrap();
        assert!(external_report(&report, &root_path).is_err());
        assert_eq!(fs::read(&report).unwrap(), b"preserve");
        let inside = root_path.join("report");
        assert!(external_report(&inside, &root_path).is_err());
        assert!(!inside.exists());
    }
}
