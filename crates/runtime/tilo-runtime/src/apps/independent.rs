//! Host-retained executing sources; installed packages keep their signed facts.
use super::{Apps, process_start_time};
use std::os::unix::fs::MetadataExt as _;
use std::{
    collections::HashMap,
    fs::File,
    io::Read as _,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};
use tilo_action_broker::services::{
    ApplicationCreationEvidence, AuthenticatedApplicationLaunch, ExecutionCustody,
};
use tilo_app_package::BlobHash;

const EXECUTABLE_LIMIT: u64 = 512 * 1024 * 1024;
const LAUNCH_LIMIT: usize = 64;

#[derive(Clone)]
pub(crate) struct OriginalSource {
    package_hash: String,
    creation: Option<ApplicationCreationEvidence>,
    development: bool,
    executable: BlobHash,
}

#[derive(Clone)]
struct Task {
    pid: u32,
    start: u64,
    executable: Arc<OnceLock<ExecutableStamp>>,
}

#[derive(Clone, PartialEq, Eq)]
struct ExecutableStamp {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl ExecutableStamp {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}
impl Task {
    fn current(&self) -> bool {
        process_start_time(self.pid) == Some(self.start)
    }

    fn executable_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(format!("/proc/{}/exe", self.pid))
    }

    fn source_current(&self) -> bool {
        self.current()
            && self.executable.get().is_some_and(|verified| {
                std::fs::metadata(self.executable_path())
                    .is_ok_and(|metadata| ExecutableStamp::of(&metadata) == *verified)
            })
            && self.current()
    }

    fn verify_source(&self, expected: BlobHash) -> bool {
        let Ok(mut file) = File::open(self.executable_path()) else {
            return false;
        };
        let Ok(before) = file.metadata() else {
            return false;
        };
        if !before.is_file() || before.len() > EXECUTABLE_LIMIT {
            return false;
        }
        let Ok((actual, bytes)) = BlobHash::of_reader(&mut (&mut file).take(EXECUTABLE_LIMIT + 1))
        else {
            return false;
        };
        let Ok(after) = file.metadata() else {
            return false;
        };
        let stamp = ExecutableStamp::of(&before);
        if bytes > EXECUTABLE_LIMIT || actual != expected || stamp != ExecutableStamp::of(&after) {
            return false;
        }
        let held = self.executable.get_or_init(|| stamp.clone());
        held == &stamp && self.source_current()
    }
}

#[derive(Clone)]
struct Cohort {
    launcher: Task,
    source: OriginalSource,
    tasks: Vec<Task>,
}

type LaunchKey = (u32, String, String);
#[derive(Default)]
pub(super) struct IndependentLaunches(Mutex<HashMap<LaunchKey, Cohort>>);

pub(super) fn executable_hash(path: &Path) -> Result<BlobHash, String> {
    let file = File::open(path).map_err(|error| format!("native executable: {error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > EXECUTABLE_LIMIT {
        return Err("native executable is not a bounded regular file".into());
    }
    let (hash, bytes) = BlobHash::of_reader(&mut file.take(EXECUTABLE_LIMIT + 1))
        .map_err(|error| error.to_string())?;
    if bytes > EXECUTABLE_LIMIT {
        return Err("native executable exceeded its source verification limit".into());
    }
    Ok(hash)
}

impl Apps {
    /// Retain code identity for supported uninstalled SDK clients. Only an
    /// explicitly started dev host grants the existing development-kind policy.
    /// Installed packages always take the original signed-source path.
    pub(crate) fn launch_source(
        &self,
        app_id: &str,
        executable: &Path,
    ) -> Result<OriginalSource, String> {
        let installed = self.catalog.as_ref().map_or_else(
            || Ok(Vec::new()),
            |catalog| catalog.list().map_err(|error| error.to_string()),
        )?;
        if installed.iter().any(|package| package.id == app_id) {
            return self.independent_source(app_id, executable);
        }
        if !executable.is_absolute() {
            return Err("native source verification requires an absolute executable path".into());
        }
        let executable = executable_hash(executable)?;
        Ok(OriginalSource {
            package_hash: format!("uninstalled:{executable}"),
            creation: None,
            development: self.development,
            executable,
        })
    }

    pub(crate) fn independent_source(
        &self,
        app_id: &str,
        executable: &Path,
    ) -> Result<OriginalSource, String> {
        if !executable.is_absolute() {
            return Err("native source verification requires an absolute executable path".into());
        }
        let app = self.launchable(app_id).map_err(|reason| {
            format!("cannot attach native app {app_id}: {reason}; install its signed GUI package, then run the declared entrypoint")
        })?;
        let original = self.original_launch(&app)?;
        let installed = self
            .catalog
            .as_ref()
            .ok_or("native source has no app store")?
            .list()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|package| {
                package.id == app_id
                    && package.package_id_hex == original.package_hash
                    && package.manifest_path == app.manifest_path
                    && package.payload_root == app.payload_root
            })
            .ok_or("native original installation changed")?;
        let report = package_store::integrity::verify(&installed, &installed.payload_root)
            .map_err(|error| error.to_string())?;
        if !report.is_intact() {
            return Err("native original signed payload changed".into());
        }
        let record = package_store::integrity::read_record(
            &package_store::integrity::record_dir(&installed),
            app_id,
            &original.package_hash,
        )
        .map_err(|error| error.to_string())?;
        let manifest = tilo_app_package::manifest::Manifest::from_toml_str(
            std::str::from_utf8(&record.manifest_bytes).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let entry = manifest
            .entrypoint
            .as_ref()
            .and_then(tilo_app_package::Entrypoint::gui_bin_path)
            .ok_or("native app has no declared GUI entrypoint")?;
        let expected = record
            .collection
            .entries
            .get(&entry.to_string_lossy().into_owned())
            .ok_or("native entrypoint is absent from the original signed collection")?;
        if executable_hash(executable)? != *expected {
            return Err(
                "independent executable does not match its original signed entrypoint".into(),
            );
        }
        Ok(OriginalSource {
            package_hash: original.package_hash,
            creation: original.creation,
            development: false,
            executable: *expected,
        })
    }

    pub(crate) fn register_independent(
        &self,
        launcher: u32,
        executing: u32,
        scope: (&str, &str),
        source: OriginalSource,
    ) -> Result<(), String> {
        self.independent
            .register(launcher, executing, scope, source)
    }
}

impl IndependentLaunches {
    fn register(
        &self,
        launcher: u32,
        executing: u32,
        (app, workspace): (&str, &str),
        source: OriginalSource,
    ) -> Result<(), String> {
        let launcher = Task {
            pid: launcher,
            start: process_start_time(launcher).ok_or("native registering launcher ended")?,
            executable: Arc::new(OnceLock::new()),
        };
        let task = Task {
            pid: executing,
            start: process_start_time(executing).ok_or("native executing task ended")?,
            executable: Arc::new(OnceLock::new()),
        };
        let mut cohorts = self
            .0
            .lock()
            .map_err(|_| "native source registry poisoned")?;
        cohorts.retain(|_, cohort| {
            cohort.tasks.retain(Task::current);
            cohort.launcher.current() && !cohort.tasks.is_empty()
        });
        let key = (launcher.pid, app.to_owned(), workspace.to_owned());
        if let Some(cohort) = cohorts.get_mut(&key) {
            if cohort.launcher.start != launcher.start
                || cohort.source.package_hash != source.package_hash
                || cohort.source.executable != source.executable
                || cohort.source.creation != source.creation
                || cohort.source.development != source.development
            {
                return Err(
                    "original independent launch source changed while its tasks are live".into(),
                );
            }
            if !cohort
                .tasks
                .iter()
                .any(|held| held.pid == task.pid && held.start == task.start)
            {
                if cohort.tasks.len() >= LAUNCH_LIMIT {
                    return Err("native independent launch cohort is full".into());
                }
                cohort.tasks.push(task);
            }
        } else {
            if cohorts.len() >= LAUNCH_LIMIT {
                return Err("native independent launch registry is full".into());
            }
            cohorts.insert(
                key,
                Cohort {
                    launcher,
                    source,
                    tasks: vec![task],
                },
            );
        }
        drop(cohorts);
        Ok(())
    }

    fn cohort(&self, pid: u32, app: &str, workspace: &str) -> Option<Cohort> {
        let cohort = self
            .0
            .lock()
            .ok()?
            .get(&(pid, app.to_owned(), workspace.to_owned()))?
            .clone();
        (cohort.launcher.current() && cohort.tasks.iter().any(Task::current)).then_some(cohort)
    }

    pub(super) fn attested(&self, pid: u32, app: &str, workspace: &str) -> bool {
        self.cohort(pid, app, workspace).is_some()
    }

    pub(super) fn creation(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
    ) -> Option<ApplicationCreationEvidence> {
        self.cohort(pid, app, workspace)?.source.creation
    }

    pub(super) fn authenticated(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
    ) -> Option<AuthenticatedApplicationLaunch> {
        let cohort = self.cohort(pid, app, workspace)?;
        Some(AuthenticatedApplicationLaunch {
            creation: cohort.source.creation,
            development_source: cohort.source.development.then_some(
                tilo_action_broker::services::DevelopmentSource {
                    executable_hash: *cohort.source.executable.as_bytes(),
                },
            ),
            continuation_instance: None,
            execution_custody: Some(ExecutionCustody(Arc::new(move |executing| {
                cohort.launcher.current()
                    && cohort
                        .tasks
                        .iter()
                        .any(|task| task.pid == executing && task.source_current())
            }))),
        })
    }

    pub(super) fn executing(
        &self,
        launcher: u32,
        peer: &launcher::services::ExecutingPeer,
        scope: (&str, &str),
    ) -> bool {
        let Some(cohort) = self.cohort(launcher, scope.0, scope.1) else {
            return false;
        };
        cohort.tasks.iter().any(|task| {
            task.current()
                && peer.pid() == task.pid
                && peer.belongs_to(task.pid, task.start)
                && task.verify_source(cohort.source.executable)
                && task.current()
                && peer.current()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, Write as _};
    use std::process::{Command, Stdio};

    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn unsigned_attachment_reports_the_required_package_and_entrypoint() {
        let root = tempfile::tempdir().unwrap();
        let apps = Apps::new(
            None,
            root.path(),
            "0b7c7a52-6f7e-4a55-9a51-1c2f4b3f7e10",
            root.path().join("support"),
            Vec::new(),
        );
        let error = apps
            .independent_source("dev.tilo.uninstalled", Path::new("/no-executable-to-read"))
            .map(|_| ())
            .unwrap_err();
        assert_eq!(
            error,
            "cannot attach native app dev.tilo.uninstalled: dev.tilo.uninstalled is not installed; install its signed GUI package, then run the declared entrypoint"
        );
    }

    #[test]
    fn executing_source_is_retired_when_the_same_live_pid_execs_other_code() {
        let mut child = Child(
            Command::new("/bin/sh")
                .args(["-c", "printf 'ready\\n'; read marker; exec /bin/sleep 30"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut ready = String::new();
        std::io::BufReader::new(child.0.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        let task = Task {
            pid: child.0.id(),
            start: process_start_time(child.0.id()).unwrap(),
            executable: Arc::new(OnceLock::new()),
        };
        let original = executable_hash(&task.executable_path()).unwrap();
        assert!(task.verify_source(original));
        assert!(task.source_current());
        let registry = IndependentLaunches::default();
        let scope = (
            "dev.tilo.exec-boundary",
            "0b7c7a52-6f7e-4a55-9a51-1c2f4b3f7e10",
        );
        registry
            .register(
                child.0.id(),
                child.0.id(),
                scope,
                OriginalSource {
                    package_hash: format!("uninstalled:{original}"),
                    creation: None,
                    development: true,
                    executable: original,
                },
            )
            .unwrap();
        let cohort = registry.cohort(child.0.id(), scope.0, scope.1).unwrap();
        assert!(cohort.tasks[0].verify_source(original));
        let retained = registry
            .authenticated(child.0.id(), scope.0, scope.1)
            .unwrap();
        let custody = retained.execution_custody.unwrap();
        assert!((custody.0)(child.0.id()));
        assert!(
            registry
                .authenticated(child.0.id(), scope.0, "another-room")
                .is_none()
        );
        assert!(
            registry
                .authenticated(child.0.id(), "another-app", scope.1)
                .is_none()
        );
        child.0.stdin.take().unwrap().write_all(b"exec\n").unwrap();
        tilo_test_env::wait_until("source replacement in the retained live task", || {
            !task.source_current()
        });
        assert!(
            task.current(),
            "the original process is still live after exec"
        );
        assert!(
            !task.verify_source(original),
            "replacement code must not recover original custody"
        );
        assert!(
            !(custody.0)(child.0.id()),
            "a retained native capability must fence the live same-PID exec"
        );
    }
}
