//! Off-paint host lifecycle; all process creation/reaping belongs to the existing `ManagedLauncher`.
use super::{config::Installation, custody::Custody};
use launcher::managed::ManagedLaunchEvent;
use std::{
    io::Write,
    path::Path,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use tilo_action_broker::{
    managed_host::{InstalledSessionConnections, ManagedHost, ManagedNativeAuthority},
    services::ManagedTaskLaunchContext,
};

pub(super) struct Request {
    pub context: ManagedTaskLaunchContext,
    pub reply: tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>,
}
pub(super) fn start(
    installation: Arc<Installation>,
    custody: Arc<Custody>,
    membership: Arc<super::super::NativeWorkspaceAuthority>,
    origin: ManagedTaskLaunchContext,
) -> Result<mpsc::SyncSender<Request>, String> {
    let broker = origin.broker()?;
    let (sender, receiver) = mpsc::sync_channel::<Request>(16);
    std::thread::Builder::new().name("native-managed-tasks".into()).spawn(move || {
        let native = ManagedNativeAuthority { membership, tasks: custody.owner.clone(), certificates: custody.clone(), host_custody: custody.clone() };
        let mut host = match ManagedHost::new(installation.host.clone(), native, Arc::new(InstalledSessionConnections), broker) {
            Ok(host) => host,
            Err(error) => {
                tracing::warn!(%error, "installed native managed runtime initialization refused");
                if let Ok(request) = receiver.recv_timeout(Duration::from_secs(1)) {
                    let _ = request.reply.send(Err("installed native task runtime is unavailable".into()));
                }
                return;
            }
        };
        while origin.host_live() {
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(request) => {
                    let result = launch(&mut host, &custody, &installation.receipts, &request.context);
                    let _ = request.reply.send(result);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if host.poll().is_err() {
                // A failed admission has already stopped its exact supervisor and retained claim.
                tracing::warn!("native managed task admission refused; launch journal retained");
            }
        }
        if host.shutdown().is_err() { tracing::warn!("native managed shutdown uncertain; private custody observations retained"); }
    }).map_err(|error| error.to_string())?;
    Ok(sender)
}
fn launch(
    host: &mut ManagedHost,
    custody: &Arc<Custody>,
    receipts: &Path,
    context: &ManagedTaskLaunchContext,
) -> Result<serde_json::Value, String> {
    custody.person(context)?;
    claim(receipts, context).map_err(|error| {
        tracing::warn!(%error, "native invocation reservation refused");
        "native launch reservation unavailable; original attempt retained".to_owned()
    })?;
    let checked = context.clone();
    let owner = custody.clone();
    let launch = host
        .launch_guarded_task(
            context.room(),
            context.task(),
            Arc::new(move || owner.person(&checked)),
        )
        .map_err(|error| {
            tracing::warn!(%error, "installed signed native task launch refused");
            "installed signed native task launch refused; original attempt retained".to_owned()
        })?;
    let admitted = observe_admission(host, &launch);
    if let Err(error) = admitted {
        let stopped = host.stop_launch(&launch);
        tracing::warn!(%error, ?stopped, "native task admission refused");
        return Err(format!(
            "native launch not admitted; original attempt retained; exact stop accepted: {}",
            stopped.is_ok()
        ));
    }
    let receipt = serde_json::json!({"version":1,"invocation":context.invocation(),
        "room":context.room(),"task":context.task(),"launch":launch,"process_admitted":true,
        "execution_status_source":"canonical task inspect and native owner receipts"});
    if let Err(error) = write_once(receipts, context, "admitted", &receipt) {
        let stopped = host.stop_launch(&launch);
        tracing::warn!(%error, ?stopped, "native admitted receipt uncertain");
        return Err(format!(
            "native launch receipt uncertain; original attempt retained; exact stop accepted: {}",
            stopped.is_ok()
        ));
    }
    Ok(receipt)
}
fn observe_admission(host: &mut ManagedHost, launch: &str) -> Result<(), String> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(15))
        .ok_or("native admission deadline overflow")?;
    loop {
        for event in host.poll()? {
            match event {
                ManagedLaunchEvent::Started(supervisor)
                    if supervisor.binding().launch == launch =>
                {
                    return Ok(());
                }
                ManagedLaunchEvent::Failed {
                    binding,
                    terminal: true,
                    ..
                }
                | ManagedLaunchEvent::Finished { binding, .. }
                    if binding.launch == launch =>
                {
                    return Err("exact supervisor ended before process admission".into());
                }
                _ => {}
            }
        }
        if Instant::now() >= deadline {
            return Err("exact native process admission deadline expired".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn claim(root: &Path, context: &ManagedTaskLaunchContext) -> Result<(), String> {
    let mut entries = 0usize;
    for entry in std::fs::read_dir(root)
        .map_err(|error| error.to_string())?
        .take(2049)
    {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err("native invocation receipts contain an unsafe entry".into());
        }
        entries = entries
            .checked_add(1)
            .ok_or("native launch receipt count exhausted")?;
        if entries >= 2048 {
            return Err("native launch invocation receipts require reconciliation".into());
        }
    }
    write_once(
        root,
        context,
        "claim",
        &serde_json::json!({"version":1,"invocation":context.invocation(),
        "room":context.room(),"task":context.task(),"caller_device":hex::encode(context.certificate().device_public_key)}),
    )
}
fn write_once(
    root: &Path,
    context: &ManagedTaskLaunchContext,
    kind: &str,
    value: &serde_json::Value,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() > 16384 {
        return Err("native launch receipt exceeds bound".into());
    }
    let path = root.join(format!("{}.{kind}.json", context.invocation()));
    let mut file = private_fs::create_new(&path).map_err(|error| error.to_string())?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| error.to_string())?;
    drop(file);
    private_fs::sync_directory(root).map_err(|error| error.to_string())
}
