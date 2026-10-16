//! Startup-only trusted check definitions; public jobs select an approved ID.
use super::{NativeOwnerCommitService, WorkspaceTreeHandle};
use runtime_collab::approved_checks::{Binding, Diagnostic, MAX_PREVIEW_BYTES, Outcome, Receipt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tilo_action_broker::collab_broker_capnp::{ApprovedCheckContext, ApprovedCheckServices};
use ush::approved_checks::{ApprovedCheck, ApprovedChecks, CheckOutcome, ProjectBinding};

const MAX_CONFIG_BYTES: u64 = 8 * 1024 * 1024;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
    projects: Vec<Project>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Project {
    app_id: String,
    resource_identity: ResourceIdentity,
    artifacts: PathBuf,
    checks: Vec<ApprovedCheck>,
}
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ResourceIdentity {
    kind: String,
    key: String,
}
struct Registered {
    app_id: String,
    resource_identity: ResourceIdentity,
    binding: ProjectBinding,
    artifacts: PathBuf,
    checks: ApprovedChecks,
}
impl Registered {
    fn matches(
        &self,
        app: &str,
        workspace: &str,
        record: &str,
        identity: &tilo_actions::ResourceSemanticIdentity,
    ) -> bool {
        self.app_id == app
            && self.binding.workspace.to_string() == workspace
            && self.binding.record == record
            && self.resource_identity.kind == identity.kind
            && self.resource_identity.key == identity.key
    }
}
#[derive(Default)]
struct Definitions(Vec<Registered>);
static DEFINITIONS: OnceLock<Arc<Definitions>> = OnceLock::new();

fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn load(path: &Path) -> Result<Definitions, String> {
    let mut file = tilo_files::native::private::read(path).map_err(|error| error.to_string())?;
    if file.metadata().map_err(|error| error.to_string())?.len() > MAX_CONFIG_BYTES {
        return Err("approved-check startup configuration exceeds bound".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    drop(file);
    if u64::try_from(bytes.len()).map_err(|error| error.to_string())? > MAX_CONFIG_BYTES {
        return Err("approved-check startup configuration grew beyond bound".into());
    }
    let config: Config = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if config.version != 1 || config.projects.len() > 32 {
        return Err("unsupported or oversized approved-check startup configuration".into());
    }
    let mut definitions = Vec::<Registered>::new();
    for project in config.projects {
        if project.app_id.is_empty()
            || project.app_id.len() > 128
            || !project
                .app_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err("approved project app identity invalid".into());
        }
        let binding = project
            .checks
            .first()
            .ok_or("approved project has no checks")?
            .binding
            .clone();
        let semantic = tilo_actions::ResourceSemanticIdentity::new(
            project.resource_identity.kind.clone(),
            project.resource_identity.key.clone(),
        )
        .ok_or("approved project semantic identity invalid")?;
        if semantic.key != binding.resource {
            return Err("approved fixed check resource must equal its semantic key".into());
        }
        if project.app_id.starts_with('-') || binding.record.len() > 1024 {
            return Err("approved project identity exceeds portable receipt vocabulary".into());
        }
        for check in &project.checks {
            runtime_collab::approved_checks::Request {
                version: 1,
                invocation: check.binding.workspace.to_string(),
                check_id: check.id.clone(),
            }
            .encode()?;
        }
        if definitions.iter().any(|other| {
            other.app_id == project.app_id
                && other.resource_identity == project.resource_identity
                && other.binding.workspace == binding.workspace
                && other.binding.record == binding.record
        }) {
            return Err("duplicate approved project binding".into());
        }
        if !project.artifacts.is_absolute()
            || project
                .artifacts
                .canonicalize()
                .map_err(|error| error.to_string())?
                != project.artifacts
            || project.checks.iter().any(|check| {
                project.artifacts.starts_with(&check.checkout)
                    || project.artifacts.starts_with(&check.build)
                    || check.checkout.starts_with(&project.artifacts)
                    || check.build.starts_with(&project.artifacts)
            })
        {
            return Err(
                "private check artifacts must be exact and outside guest source/build".into(),
            );
        }
        // Existing objects must already be private; startup never silently
        // tightens a public directory containing grader material.
        let metadata =
            std::fs::symlink_metadata(&project.artifacts).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.uid() != effective_uid() || metadata.mode() & 0o077 != 0 {
            return Err("approved private artifacts directory is not owned/private".into());
        }
        let checks = ApprovedChecks::new(binding.clone(), project.checks)?;
        definitions.push(Registered {
            app_id: project.app_id,
            resource_identity: project.resource_identity,
            binding,
            artifacts: project.artifacts,
            checks,
        });
    }
    Ok(Definitions(definitions))
}

fn argument_path(
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<Option<PathBuf>, String> {
    let mut arguments = arguments.into_iter();
    let mut selected = None;
    while let Some(argument) = arguments.next() {
        let path = if argument == "--approved-checks" {
            Some(PathBuf::from(arguments.next().ok_or(
                "--approved-checks requires a private configuration path",
            )?))
        } else {
            argument
                .to_str()
                .and_then(|argument| argument.strip_prefix("--approved-checks="))
                .map(PathBuf::from)
        };
        if let Some(path) = path {
            if selected.is_some() || !path.is_absolute() {
                return Err("approved-check startup path must be unique and absolute".into());
            }
            selected = Some(path);
        }
    }
    Ok(selected)
}

/// Load immutable private host configuration before any app/task admission.
///
/// # Errors
/// Refuses duplicate flags, non-private/link files, malformed or foreign registrations.
pub fn install_from_args(
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let definitions =
        argument_path(arguments)?.map_or_else(|| Ok(Definitions::default()), |path| load(&path))?;
    DEFINITIONS
        .set(Arc::new(definitions))
        .map_err(|_| "approved-check definitions already installed".into())
}

pub(super) fn service(
    tree: WorkspaceTreeHandle,
    network: Option<Arc<super::super::network::ShellNetwork>>,
) -> Arc<dyn ApprovedCheckServices> {
    Arc::new(NativeChecks {
        definitions: DEFINITIONS.get().cloned().unwrap_or_default(),
        owner: NativeOwnerCommitService::new(tree, network),
    })
}
struct NativeChecks {
    definitions: Arc<Definitions>,
    owner: NativeOwnerCommitService,
}
impl NativeChecks {
    fn host_current(&self, binding: &ProjectBinding) -> Result<(), String> {
        let room = tilo_desktop_model::RoomId::parse(&binding.workspace.to_string())
            .ok_or("invalid approved project room")?;
        let current = tilo_identity_host::broker::trusted_current_device_public(
            &binding.workspace.to_string(),
        )
        .map_err(|error| error.to_string())?;
        if current.device_public_key != binding.host {
            return Err("approved project host device changed".into());
        }
        self.owner
            .opened_owner("committed project host unavailable")?
            .with_current(room, |view| {
                let host = view.host().unwrap_or_else(|| view.governor());
                if host.device_public_key != binding.host {
                    return Err("approved project is governed by a different host".into());
                }
                Ok(())
            })
    }
}
fn diagnostic(text: Option<&String>, sha256: Option<String>) -> Diagnostic {
    let text = text.map_or("", String::as_str);
    let mut end = text.len().min(MAX_PREVIEW_BYTES);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    Diagnostic {
        sha256,
        preview: text.get(..end).map_or_else(String::new, str::to_owned),
        truncated: end != text.len(),
    }
}
fn reported_observation(
    event: ush::approved_checks::ReportedCheckEvent,
) -> runtime_collab::approved_checks::ReportObservation {
    use runtime_collab::approved_checks::{LaneRefusal, ReportObservation};
    use ush::approved_checks::{CheckReportRefusal, ReportedCheckEvent};
    match event {
        ReportedCheckEvent::Reported(report) => ReportObservation::Reported {
            phase: report.phase,
            completed: report.completed,
            total: report.total,
        },
        ReportedCheckEvent::Disabled(reason) => ReportObservation::Disabled {
            reason: match reason {
                CheckReportRefusal::InvalidReport => LaneRefusal::InvalidReport,
                CheckReportRefusal::OversizedReport => LaneRefusal::OversizedReport,
                CheckReportRefusal::UnknownPhase => LaneRefusal::UnknownPhase,
                CheckReportRefusal::CorrelationChanged => LaneRefusal::CorrelationChanged,
                CheckReportRefusal::SequenceChanged => LaneRefusal::SequenceChanged,
                CheckReportRefusal::OutputUnavailable => LaneRefusal::OutputUnavailable,
                CheckReportRefusal::Backpressure => LaneRefusal::Backpressure,
            },
        },
    }
}

impl ApprovedCheckServices for NativeChecks {
    fn run(&self, context: &ApprovedCheckContext) -> Result<Receipt, String> {
        context.recheck()?;
        let identity = context.resource_identity()?;
        let registered = self
            .definitions
            .0
            .iter()
            .find(|registered| {
                registered.matches(
                    context.app_id(),
                    context.workspace(),
                    context.record(),
                    identity,
                )
            })
            .ok_or("no approved checks for this exact project resource")?;
        let observed = registered.checks.run_with_reports(
            &registered.binding,
            context.check_id(),
            || {
                context.recheck()?;
                self.host_current(&registered.binding)
            },
            |event| {
                let _ = context.report(reported_observation(event));
            },
        )?;
        let artifact_value = serde_json::json!({
            "resource_identity": registered.resource_identity,
            "accepted_opaque_resource": context.resource(),
            "observed": observed,
        });
        let bytes = serde_json::to_vec(&artifact_value).map_err(|error| error.to_string())?;
        let artifact_sha256 = hex::encode(Sha256::digest(&bytes));
        let path = registered.artifacts.join(format!("{artifact_sha256}.json"));
        // Full private streams/config/manager receipts never enter an action catalog.
        let mut artifact =
            tilo_files::native::private::create_new(&path).map_err(|error| error.to_string())?;
        artifact
            .write_all(&bytes)
            .and_then(|()| artifact.sync_all())
            .map_err(|error| error.to_string())?;
        drop(artifact);
        tilo_files::native::private::sync_directory(&registered.artifacts)
            .map_err(|error| error.to_string())?;
        let receipt = Receipt {
            version: 1,
            invocation: context.invocation(),
            check_id: context.check_id().into(),
            binding: Binding {
                app_id: registered.app_id.clone(),
                workspace: registered.binding.workspace.to_string(),
                host: registered.binding.host,
                record: registered.binding.record.clone(),
                resource: context.resource().into(),
            },
            artifact_sha256,
            config_sha256: observed.config_sha256,
            runtime_binary_sha256: observed.runtime_binary_sha256,
            supervisor_source_sha256: observed.supervisor_source_sha256,
            outcome: match observed.outcome {
                CheckOutcome::Exited => Outcome::Exited,
                CheckOutcome::Cancelled => Outcome::Cancelled,
                CheckOutcome::TimedOut => Outcome::TimedOut,
                CheckOutcome::OutputLimit => Outcome::OutputLimit,
                CheckOutcome::Failed => Outcome::Failed,
            },
            exit_code: observed.exit_code,
            signal: observed.check_signal,
            elapsed_ms: u64::try_from(observed.elapsed_ms).map_err(|error| error.to_string())?,
            cleanup_observed: observed.cleanup_observed,
            stdout: diagnostic(observed.stdout.as_ref(), observed.stdout_sha256),
            stderr: diagnostic(observed.stderr.as_ref(), observed.stderr_sha256),
        };
        receipt.encode()?;
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    #[test]
    fn approved_check_startup_refuses_public_links_duplicate_flags_and_schema_ambiguity() {
        let directory = tempfile::tempdir().expect("fixture operation succeeds");
        let path = directory.path().join("checks.json");
        std::fs::write(&path, br#"{"version":1,"projects":[]}"#)
            .expect("fixture operation succeeds");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("fixture operation succeeds");
        assert!(
            load(&path).is_err(),
            "startup never accepts publicly readable grading definitions"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("fixture operation succeeds");
        assert!(load(&path).expect("valid private fixture").0.is_empty());
        let linked = directory.path().join("linked.json");
        symlink(&path, &linked).expect("fixture operation succeeds");
        assert!(load(&linked).is_err());
        for bytes in [
            br#"{"version":1,"version":2,"projects":[]}"#.as_slice(),
            br#"{"version":1,"projects":[],"command":"id"}"#.as_slice(),
        ] {
            std::fs::write(&path, bytes).expect("fixture operation succeeds");
            assert!(load(&path).is_err());
        }
        let flag = std::ffi::OsString::from("--approved-checks");
        assert!(argument_path([flag.clone()]).is_err());
        assert!(argument_path([flag.clone(), "relative.json".into()]).is_err());
        assert!(
            argument_path([
                flag.clone(),
                path.as_os_str().into(),
                flag,
                path.as_os_str().into()
            ])
            .is_err()
        );
        assert_eq!(
            argument_path(["--mode".into(), "native".into()]).expect("unrelated startup flags"),
            None
        );
    }

    fn assert_stable_registration(registered: &Registered) {
        let identity = tilo_actions::ResourceSemanticIdentity::new("git_client.repo", "repository")
            .expect("valid fixed semantic identity");
        let workspace = registered.binding.workspace.to_string();
        assert!(registered.matches("so.tilo.git-client", &workspace, "project", &identity));
        for wrong in [
            tilo_actions::ResourceSemanticIdentity::new("git_client.repo", "different-repository"),
            tilo_actions::ResourceSemanticIdentity::new("other.repo", "repository"),
        ] {
            assert!(!registered.matches(
                "so.tilo.git-client",
                &workspace,
                "project",
                &wrong.expect("valid adversarial identity")
            ));
        }
        assert!(!registered.matches(
            "so.tilo.git-client",
            &workspace,
            "foreign-record",
            &identity
        ));
    }

    #[test]
    fn approved_check_startup_refuses_foreign_scope_and_guest_visible_private_receipts() {
        let directory = tempfile::tempdir().expect("fixture operation succeeds");
        let root = directory
            .path()
            .canonicalize()
            .expect("fixture operation succeeds");
        for name in [
            "source/target",
            "build",
            "scratch",
            "artifacts",
            "image/usr/bin",
        ] {
            std::fs::create_dir_all(root.join(name)).expect("fixture operation succeeds");
        }
        for name in ["scratch", "artifacts"] {
            std::fs::set_permissions(root.join(name), std::fs::Permissions::from_mode(0o700))
                .expect("fixture operation succeeds");
        }
        let runtime = root.join("runtime");
        std::fs::write(
            &runtime,
            b"inert configuration-validation fixture; never executed",
        )
        .expect("fixture operation succeeds");
        let check = serde_json::json!({
            "id":"regression", "binding":{"workspace":uuid::Uuid::new_v4(),"host":vec![7u8;32],"record":"project", "resource":"repository"},
            "checkout":root.join("source"), "build":root.join("build"), "scratch":root.join("scratch"),
            "runtime_binary":runtime, "images":[{"identity":format!("sha256:{}", "1".repeat(64)),"root":root.join("image")}],
            "command":["/usr/bin/true"],"environment":[],
            "limits":{"cpu_percent":100,"memory_bytes":16_777_216,"processes":2,"wall_seconds":1,"output_bytes":1024}
        });
        let config = serde_json::json!({"version":1,"projects":[{"app_id":"so.tilo.git-client","resource_identity":{"kind":"git_client.repo","key":"repository"},"artifacts":root.join("artifacts"),"checks":[&check]}]});
        let path = root.join("checks.json");
        let mut file =
            tilo_files::native::private::create_new(&path).expect("fixture operation succeeds");
        file.write_all(&serde_json::to_vec(&config).expect("serialize fixture"))
            .expect("fixture operation succeeds");
        drop(file);
        assert_eq!(
            load(&path).expect("valid private fixture").0.len(),
            1,
            "real startup validates inert trusted definitions without executing them"
        );
        let definitions = load(&path).expect("real startup definitions");
        let registered = &definitions.0[0];
        assert_stable_registration(registered);
        let mut conflicting_host = config.clone();
        let mut duplicate = conflicting_host["projects"][0].clone();
        duplicate["checks"][0]["binding"]["host"] = serde_json::json!(vec![8u8; 32]);
        conflicting_host["projects"]
            .as_array_mut()
            .expect("project array present")
            .push(duplicate);
        std::fs::write(
            &path,
            serde_json::to_vec(&conflicting_host).expect("serialize fixture"),
        )
        .expect("fixture operation succeeds");
        assert!(
            load(&path).is_err(),
            "one stable project address cannot ambiguously register different hosts"
        );
        let mut foreign = config.clone();
        let mut other = check;
        other["id"] = serde_json::json!("other");
        other["binding"]["record"] = serde_json::json!("foreign-record");
        foreign["projects"][0]["checks"]
            .as_array_mut()
            .expect("checks array present")
            .push(other);
        std::fs::write(
            &path,
            serde_json::to_vec(&foreign).expect("serialize fixture"),
        )
        .expect("fixture operation succeeds");
        assert!(
            load(&path).is_err(),
            "one host registry cannot silently execute a foreign record"
        );
        let mut exposed = config;
        exposed["projects"][0]["artifacts"] = serde_json::json!(root.join("source"));
        std::fs::write(
            &path,
            serde_json::to_vec(&exposed).expect("serialize fixture"),
        )
        .expect("fixture operation succeeds");
        assert!(
            load(&path).is_err(),
            "private receipt cannot be in the guest-readable source tree"
        );
    }

    #[test]
    fn approved_check_diagnostics_bound_utf8_without_inventing_a_stream_hash() {
        let text = "λ".repeat(MAX_PREVIEW_BYTES);
        let diagnostic = diagnostic(Some(&text), Some("a".repeat(64)));
        assert_eq!(diagnostic.preview.len(), MAX_PREVIEW_BYTES);
        assert!(diagnostic.truncated);
        assert_eq!(diagnostic.sha256, Some("a".repeat(64)));
        let absent = super::diagnostic(None, None);
        assert!(absent.preview.is_empty());
        assert!(!absent.truncated);
        assert_eq!(absent.sha256, None);
    }
}
