//! Headless request builder for harnesses and development.

use serde_json::json;
use std::path::{Path, PathBuf};

use super::helper::{self, Action, ComponentPaths, Request};
use super::record::ReleaseRef;

pub const STAGING_REQUEST_ARG: &str = "--tilo-staging-request";

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    action: Option<Action>,
    disk: Option<String>,
    manifest: Option<String>,
    esp: Option<String>,
    root: Option<String>,
    target_bytes: Option<u64>,
    profile: Option<String>,
    record: Option<String>,
    plan: Option<String>,
    token_file: Option<String>,
    development_layers: Option<String>,
    out: Option<String>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{flag} needs a value"));
        let once = |slot: &mut Option<String>, v: String| {
            if slot.replace(v).is_some() {
                Err(format!("{flag} given twice"))
            } else {
                Ok(())
            }
        };
        match flag.as_str() {
            "--action" => {
                let action = match value()?.as_str() {
                    "stage" => Action::Stage,
                    "arm" => Action::Arm,
                    "remove" => Action::Remove,
                    other => return Err(format!("unknown action {other:?}")),
                };
                if out.action.replace(action).is_some() {
                    return Err("--action given twice".into());
                }
            }
            "--manifest" => once(&mut out.manifest, value()?)?,
            "--disk" => once(&mut out.disk, value()?)?,
            "--component" => {
                let v = value()?;
                match v.split_once('=') {
                    Some(("staging-esp", p)) => once(&mut out.esp, p.into())?,
                    Some(("staging-root", p)) => once(&mut out.root, p.into())?,
                    _ => {
                        return Err(format!(
                            "--component expects staging-esp=PATH or staging-root=PATH, got {v:?}"
                        ));
                    }
                }
            }
            "--target-bytes" => {
                let v = value()?;
                let n = v
                    .parse()
                    .map_err(|_| format!("invalid --target-bytes {v:?}"))?;
                if out.target_bytes.replace(n).is_some() {
                    return Err("--target-bytes given twice".into());
                }
            }
            "--profile-json" => once(&mut out.profile, value()?)?,
            "--record" => once(&mut out.record, value()?)?,
            "--plan" => once(&mut out.plan, value()?)?,
            "--token-file" => once(&mut out.token_file, value()?)?,
            "--development-layers" => once(&mut out.development_layers, value()?)?,
            "--out" => once(&mut out.out, value()?)?,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(out)
}

fn absolute(path: &str) -> Result<String, String> {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| format!("{path}: {e}"))
}

fn read_profile(path: &str) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("{path}: {e}"))?;
    if let Some(map) = value.as_object() {
        if super::handoff::validate_profile(map).is_ok() {
            return Ok(map.clone());
        }
    }
    let profile: crate::model::InstallProfile = serde_json::from_value(value)
        .map_err(|e| format!("{path}: not a media or install profile: {e}"))?;
    super::handoff::profile_from_install(&profile)
}

fn write_request(path: &Path, request: &Request) -> Result<(), String> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(&serde_json::to_vec_pretty(request).map_err(|e| e.to_string())?)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())
}

enum Failure {
    Blocked(serde_json::Value),
    Plan(serde_json::Value),
    Other(String),
}

fn build(args: &Args) -> Result<(Request, serde_json::Value), Failure> {
    let other = Failure::Other;
    let action = args.action.unwrap_or(Action::Stage);
    let manifest_path = match (&args.manifest, action) {
        (Some(m), _) => absolute(m).map_err(other)?,
        (None, Action::Remove) => String::new(),
        (None, _) => return Err(other("--manifest is required".into())),
    };
    let component = |p: &Option<String>| p.as_deref().map(absolute).transpose();
    let component_paths = ComponentPaths {
        staging_esp: component(&args.esp).map_err(other)?.unwrap_or_default(),
        staging_root: component(&args.root).map_err(other)?.unwrap_or_default(),
    };
    if action != Action::Stage {
        if args.disk.is_some() {
            return Err(other("--disk names the staging target of a stage".into()));
        }
        let id = args
            .record
            .as_deref()
            .ok_or(other("--record is required for arm and remove".into()))?;
        let record = helper::pending_records()
            .into_iter()
            .find(|r| r.operation_id == id)
            .ok_or_else(|| other(format!("no unfinished staging record {id}")))?;
        let request = Request {
            schema: helper::REQUEST_SCHEMA,
            operation_id: record.operation_id.clone(),
            action,
            release: record.release.clone(),
            manifest_path,
            component_paths,
            plan_sha256: record.plan_sha256.clone(),
            plan: record.plan,
            profile: serde_json::Map::new(),
            install_plan: None,
            token_file: None,
            development_layers: None,
        };
        let summary = json!({"ok": true, "action": action, "operation_id": id});
        return Ok((request, summary));
    }
    if component_paths.staging_esp.is_empty() || component_paths.staging_root.is_empty() {
        return Err(other(
            "both --component staging-esp=PATH and staging-root=PATH are required".into(),
        ));
    }
    let target = args
        .target_bytes
        .ok_or(other("--target-bytes is required".into()))?;
    let manifest = std::fs::read(&manifest_path).map_err(|e| other(e.to_string()))?;
    let facts = crate::setup::internal_bundle::facts(&manifest).map_err(other)?;
    let profile = match &args.profile {
        Some(p) => read_profile(p).map_err(other)?,
        None => serde_json::Map::new(),
    };
    let probed = match &args.disk {
        Some(target) => super::probe::probe_target(target).map_err(other)?,
        None => super::probe::probe(),
    };
    let blockers = &probed.blockers;
    let Some(basis) = probed.basis.as_ref().filter(|_| blockers.is_empty()) else {
        return Err(Failure::Blocked(json!({"ok": false, "blockers": blockers})));
    };
    let mut plan = super::plan::compute(basis, facts.requirements(target))
        .map(|p| super::plan::with_loader_path(p, &facts.efi_path))
        .map_err(|e| {
            Failure::Plan(match e {
                super::plan::PlanError::InsufficientSpace {
                    needed_bytes,
                    available_bytes,
                } => json!({"ok": false, "plan_error": "insufficient-space",
                    "needed_bytes": needed_bytes, "available_bytes": available_bytes}),
                super::plan::PlanError::Unsupported(m) => {
                    json!({"ok": false, "plan_error": "unsupported", "message": m})
                }
            })
        })?;
    let install_plan = match &args.plan {
        Some(path) => {
            let bytes = std::fs::read(path).map_err(|e| other(format!("{path}: {e}")))?;
            let plan: install_profile_model::plan::Plan = serde_json::from_slice(&bytes)
                .map_err(|e| other(format!("{path}: not an install plan: {e}")))?;
            Some(plan)
        }
        None => None,
    };
    let token_file = match &args.token_file {
        Some(path) => Some(absolute(path).map_err(other)?),
        None => None,
    };
    let development_layers = match &args.development_layers {
        Some(path) => Some(absolute(path).map_err(other)?),
        None => None,
    };
    if let Some(target) = &args.disk {
        // The plan's disk handle is the identity the helper resolves again;
        // a `/dev` name means nothing in another namespace or boot.
        let identity = super::TargetDisk::parse(target).map_err(other)?.to_string();
        plan.basis.disk.id = identity;
    }
    let request = Request {
        schema: helper::REQUEST_SCHEMA,
        operation_id: super::random_guid(),
        action,
        release: ReleaseRef {
            release_id: facts.release_id.clone(),
            manifest_sha256: hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&manifest)),
            architecture: facts.architecture,
        },
        manifest_path,
        component_paths,
        plan_sha256: plan.digest().map_err(|error| other(error.to_string()))?,
        plan,
        profile,
        install_plan,
        token_file,
        development_layers,
    };
    helper::validate_request(&request).map_err(|r| other(r.to_string()))?;
    let summary = json!({
        "ok": true,
        "action": action,
        "operation_id": request.operation_id,
        "plan_sha256": request.plan_sha256,
        "shrink": request.plan.shrink,
        "target_extent": request.plan.target_extent,
        "staging_esp": request.plan.staging_esp,
        "staging_root": request.plan.staging_root,
        "disk": {"guid": request.plan.basis.disk.guid, "id": request.plan.basis.disk.id},
        "install_plan": request.install_plan.as_ref().map(|plan| json!({
            "id": plan.plan,
            "destination": plan.destination,
            "nonce": plan.token.nonce,
        })),
    });
    Ok((request, summary))
}

pub fn maybe_run_staging_request(args: &[String]) -> Result<Option<i32>, String> {
    if args.get(1).map(String::as_str) != Some(STAGING_REQUEST_ARG) {
        return Ok(None);
    }
    let parsed = parse(&args[2..])?;
    let out = PathBuf::from(parsed.out.clone().ok_or("--out is required")?);
    let (code, summary) = match build(&parsed) {
        Ok((request, mut summary)) => {
            write_request(&out, &request)?;
            summary["request"] = json!(out);
            (0, summary)
        }
        Err(Failure::Blocked(v)) => (2, v),
        Err(Failure::Plan(v)) => (3, v),
        Err(Failure::Other(m)) => (1, json!({"ok": false, "error": m})),
    };
    println!("{summary}");
    Ok(Some(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn staging_cli_parses_only_exact_strict_forms() {
        {
            let a = parse(&s(&[
                "--manifest",
                "m.json",
                "--component",
                "staging-esp=e.img",
                "--component",
                "staging-root=r.img",
                "--target-bytes",
                "36507222016",
                "--out",
                "req.json",
            ]))
            .unwrap();
            assert_eq!(a.esp.as_deref(), Some("e.img"));
            assert_eq!(a.target_bytes, Some(36_507_222_016));
            assert_eq!(a.action, None);
            assert_eq!(a.disk, None);
            let target = parse(&s(&[
                "--disk",
                "serial:16Q0A07GTM7J",
                "--target-bytes",
                "1",
                "--out",
                "x",
            ]))
            .unwrap();
            assert_eq!(target.disk.as_deref(), Some("serial:16Q0A07GTM7J"));
            assert!(parse(&s(&["--disk", "a", "--disk", "b"])).is_err());
            let r = parse(&s(&["--action", "remove", "--record", "op", "--out", "x"])).unwrap();
            assert_eq!(r.action, Some(Action::Remove));
            let layers = parse(&s(&[
                "--development-layers",
                "/var/tmp/layers",
                "--out",
                "x",
            ]))
            .unwrap();
            assert_eq!(
                layers.development_layers.as_deref(),
                Some("/var/tmp/layers")
            );
            assert!(
                parse(&s(&[
                    "--development-layers",
                    "a",
                    "--development-layers",
                    "b"
                ]))
                .is_err()
            );
            assert!(parse(&s(&["--component", "boot=x"])).is_err());
            assert!(parse(&s(&["--out", "a", "--out", "b"])).is_err());
            assert!(parse(&s(&["--bogus"])).is_err());
            assert!(parse(&s(&["--target-bytes", "lots"])).is_err());
            assert!(parse(&s(&["--manifest"])).is_err());
        }

        {
            assert_eq!(maybe_run_staging_request(&s(&["imager"])).unwrap(), None);
            assert!(maybe_run_staging_request(&s(&["imager", STAGING_REQUEST_ARG])).is_err());
        }

        {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("req.json");
            let code = maybe_run_staging_request(&s(&[
                "imager",
                STAGING_REQUEST_ARG,
                "--action",
                "remove",
                "--record",
                "missing-operation",
                "--out",
                out.to_str().unwrap(),
            ]))
            .unwrap();
            assert_eq!(code, Some(1));
            assert!(!out.exists());
        }

        {
            let dir = tempfile::tempdir().unwrap();
            let bad = dir.path().join("bad-profile.json");
            std::fs::write(&bad, r#"{"schema":1,"hostname":"bad host"}"#).unwrap();
            assert!(read_profile(bad.to_str().unwrap()).is_err());

            let long = dir.path().join("long-profile.json");
            std::fs::write(
                &long,
                serde_json::json!({"schema": 1, "hostname": "h".repeat(64)}).to_string(),
            )
            .unwrap();
            assert!(read_profile(long.to_str().unwrap()).is_err());

            let good = dir.path().join("good-profile.json");
            std::fs::write(&good, r#"{"schema":1,"hostname":"studio"}"#).unwrap();
            let profile = read_profile(good.to_str().unwrap()).unwrap();
            assert_eq!(profile["hostname"], "studio");
        }
    }
}
