//! Device-local cloud transfers, isolated by their stable operation IDs.
use crate::file_ops::{FileOp, Outcome, TransferProgress};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use ui::tilo_resource::Settled;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Lifecycle {
    #[default]
    Queued,
    Running,
    Finished,
}

#[derive(Default)]
struct Job {
    cancel: AtomicBool,
    progress: Mutex<TransferProgress>,
    lifecycle: Mutex<Lifecycle>,
}

thread_local! {
    // Issuers never enter a record. Each checked component lifetime has its own.
    static APPROVALS: std::cell::RefCell<HashMap<uuid::Uuid, tilo_cloud_files::ImportAuthority>> = std::cell::RefCell::new(HashMap::new());
}

pub(crate) fn approve(device: &str, op: &FileOp, entry: &str) -> Result<(), String> {
    if ui::tilo_actions::executor::is_replaying() {
        return Err("replay cannot approve a private cloud import".into());
    }
    if !ui::bound_dispatch::host_person_invocation_permitted() {
        return Err("only the host person can approve a personal cloud import".into());
    }
    let scope = ui::bound_dispatch::executing_host_scope()
        .ok_or("cloud import requires a checked host issuer")?;
    let FileOp::CloudImport {
        job_id,
        account,
        entries,
        folder,
        destination,
    } = op
    else {
        return Err("approval requires a cloud import".into());
    };
    APPROVALS.with_borrow_mut(|issuers| {
        issuers.entry(scope).or_default().approve(
            &format!("{entry}:{job_id}"),
            account,
            entries,
            folder,
            destination,
            device,
        )
    })
}

/// Called on the checked host thread, before a worker or source request exists.
/// Do not re-check the current collaborator: an already approved owner job may
/// complete while another collaborator is interacting with the document.
pub(crate) fn consume(
    device: &str,
    op: &FileOp,
    entry: &str,
) -> Result<tilo_cloud_files::ImportConsent, String> {
    let scope = ui::bound_dispatch::executing_host_scope()
        .ok_or("cloud import requires a checked host issuer")?;
    let FileOp::CloudImport {
        job_id,
        account,
        entries,
        folder,
        destination,
    } = op
    else {
        return Err("approval requires a cloud import".into());
    };
    APPROVALS.with_borrow_mut(|issuers| {
        issuers
            .get_mut(&scope)
            .ok_or("cloud import has no local issuer")?
            .consume(
                &format!("{entry}:{job_id}"),
                account,
                entries,
                folder,
                destination,
                device,
            )
    })
}

/// A mounted import grant never enters the replicated outbox.
pub(crate) struct MountedConsent {
    paths: Vec<String>,
    folder: String,
    destination: tilo_cloud_files::DestinationIdentity,
}

struct MountedApproval {
    device: String,
    op: FileOp,
    destination: tilo_cloud_files::DestinationIdentity,
}

thread_local! {
    static MOUNTED_APPROVALS: std::cell::RefCell<HashMap<(uuid::Uuid, String), Option<MountedApproval>>> = std::cell::RefCell::new(HashMap::new());
}

pub(crate) fn approve_mounted(device: &str, op: &FileOp, entry: &str) -> Result<(), String> {
    if ui::tilo_actions::executor::is_replaying() {
        return Err("replay cannot approve a private mounted import".into());
    }
    if !ui::bound_dispatch::host_person_invocation_permitted()
        || ui::bound_dispatch::executing_host_device().as_deref() != Some(device)
    {
        return Err("only the receiving host's person can approve a mounted import".into());
    }
    let scope = ui::bound_dispatch::executing_host_scope()
        .ok_or("mounted import requires a checked host issuer")?;
    let FileOp::ImportMounted { folder, .. } = op else {
        return Err("approval requires a mounted import".into());
    };
    let destination = crate::cloud_transfer::pin_destination(folder, std::iter::empty())?;
    MOUNTED_APPROVALS.with_borrow_mut(|approvals| {
        use std::collections::hash_map::Entry;
        match approvals.entry((scope, entry.into())) {
            Entry::Occupied(_) => Err("mounted import already admitted".into()),
            Entry::Vacant(slot) => {
                slot.insert(Some(MountedApproval {
                    device: device.into(),
                    op: op.clone(),
                    destination,
                }));
                Ok(())
            }
        }
    })
}

pub(crate) fn consume_mounted(
    device: &str,
    op: &FileOp,
    entry: &str,
) -> Result<MountedConsent, String> {
    let scope = ui::bound_dispatch::executing_host_scope()
        .ok_or("mounted import requires a checked host issuer")?;
    MOUNTED_APPROVALS.with_borrow_mut(|approvals| {
        let MountedApproval {
            device: approved_device,
            op: approved_op,
            destination,
        } = approvals
            .get_mut(&(scope, entry.into()))
            .and_then(Option::take)
            .ok_or("mounted import has no unused local approval")?;
        if approved_device != device
            || approved_op != *op
            || ui::bound_dispatch::executing_host_device().as_deref() != Some(device)
        {
            return Err("mounted import differs from its local approval".into());
        }
        let FileOp::ImportMounted { paths, folder } = approved_op else {
            return Err("approval requires a mounted import".into());
        };
        Ok(MountedConsent {
            paths,
            folder,
            destination,
        })
    })
}

impl MountedConsent {
    pub(crate) fn execute(self) -> Outcome {
        let mut changes = Vec::new();
        let result = (|| {
            if crate::cloud_transfer::pin_destination(&self.folder, std::iter::empty())?
                != self.destination
            {
                return Err("mounted import destination changed since approval".into());
            }
            for path in self.paths {
                let copied = crate::fileops::import_mounted_into(
                    std::path::Path::new(&path),
                    std::path::Path::new(&self.folder),
                )?;
                changes.push(crate::file_ops::Change::Copied {
                    from: path,
                    to: copied.display().to_string(),
                });
            }
            Ok(())
        })();
        match result {
            Ok(()) => Settled::Done(changes),
            Err(reason) => Settled::Failed {
                reason,
                partial: changes,
            },
        }
    }
}

fn jobs() -> &'static Mutex<HashMap<String, Arc<Job>>> {
    static JOBS: OnceLock<Mutex<HashMap<String, Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(Mutex::default)
}

fn job(id: &str) -> Arc<Job> {
    jobs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(id.to_owned())
        .or_default()
        .clone()
}

/// Register before dispatch so cancellation also applies while queued.
pub(crate) fn register(id: &str) {
    let _ = job(id);
}

pub(crate) fn snapshot(id: &str) -> TransferProgress {
    job(id)
        .progress
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

pub(crate) fn cancel(id: &str) {
    job(id).cancel.store(true, Ordering::Release);
}

pub(crate) fn run(op: &FileOp, consent: tilo_cloud_files::ImportConsent) -> Outcome {
    let FileOp::CloudImport { job_id, .. } = op else {
        return Settled::Failed {
            reason: "cloud runner requires a cloud import".into(),
            partial: vec![],
        };
    };
    run_job(job_id, |cancel, progress| {
        crate::cloud_transfer::execute(consent, cancel, progress)
    })
}

fn run_job(
    id: &str,
    execute: impl FnOnce(&AtomicBool, &dyn Fn(TransferProgress)) -> Outcome,
) -> Outcome {
    let job = job(id);
    {
        let mut lifecycle = job
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *lifecycle != Lifecycle::Queued {
            return Settled::Failed {
                reason: "this cloud import has already started; retry with a new job ID".into(),
                partial: vec![],
            };
        }
        *lifecycle = Lifecycle::Running;
    }
    let report = |status| {
        *job.progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = status;
    };
    let outcome = if job.cancel.load(Ordering::Acquire) {
        Settled::Failed {
            reason: "cloud import cancelled".into(),
            partial: vec![],
        }
    } else {
        execute(&job.cancel, &report)
    };
    let mut status = snapshot(id);
    status.phase = if job.cancel.load(Ordering::Acquire) {
        "cancelled"
    } else if outcome.is_done() {
        "done"
    } else {
        "failed"
    }
    .into();
    report(status);
    *job.lifecycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Lifecycle::Finished;
    outcome
}

/// Explicit authorized fixture; no production path falls back to this issuer.
#[cfg(test)]
pub(crate) fn fixture_consent(op: &FileOp) -> tilo_cloud_files::ImportConsent {
    let FileOp::CloudImport {
        job_id,
        account,
        entries,
        folder,
        destination,
    } = op
    else {
        panic!("cloud fixture requires an import");
    };
    let mut issuer = tilo_cloud_files::ImportAuthority::default();
    issuer
        .approve(
            job_id,
            account,
            entries,
            folder,
            destination,
            "fixture-device",
        )
        .unwrap();
    issuer
        .consume(
            job_id,
            account,
            entries,
            folder,
            destination,
            "fixture-device",
        )
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_jobs_isolate_cancellation_and_progress() {
        let first = uuid::Uuid::new_v4().to_string();
        let second = uuid::Uuid::new_v4().to_string();
        let ready = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                run_job(&first, |cancelled, report| {
                    report(TransferProgress {
                        bytes: 11,
                        ..Default::default()
                    });
                    ready.wait();
                    ready.wait();
                    assert!(cancelled.load(Ordering::Acquire));
                    Settled::Failed {
                        reason: "cancelled".into(),
                        partial: vec![],
                    }
                })
            });
            ready.wait();
            let result = run_job(&second, |cancelled, report| {
                report(TransferProgress {
                    bytes: 29,
                    ..Default::default()
                });
                cancel(&first);
                assert!(!cancelled.load(Ordering::Acquire));
                assert_eq!(snapshot(&first).bytes, 11);
                assert_eq!(snapshot(&second).bytes, 29);
                Settled::Done(vec![])
            });
            assert!(result.is_done());
            ready.wait();
            assert!(!worker.join().unwrap().is_done());
        });
        assert_eq!(snapshot(&first).phase, "cancelled");
        assert_eq!(snapshot(&second).phase, "done");
        assert_eq!(snapshot(&first).bytes, 11);
        assert_eq!(snapshot(&second).bytes, 29);
    }

    #[test]
    fn cancellation_before_start_is_not_reset() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().display().to_string();
        let job_id = uuid::Uuid::new_v4().to_string();
        let op = FileOp::CloudImport {
            job_id: job_id.clone(),
            account: "account".into(),
            entries: vec![],
            destination: crate::cloud_transfer::pin_destination(&folder, std::iter::empty())
                .unwrap(),
            folder,
        };
        register(&job_id);
        cancel(&job_id);
        let outcome = run(&op, fixture_consent(&op));
        assert_eq!(outcome.reason(), Some("cloud import cancelled"));
        assert!(outcome.result().is_empty());
        assert_eq!(snapshot(&job_id).phase, "cancelled");
        assert!(!run(&op, fixture_consent(&op)).is_done());
        assert_eq!(snapshot(&job_id).phase, "cancelled");
    }
}
