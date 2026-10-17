//! First-use Copy on the normal native app admission and private helper gate.
use super::*;
use crate::onboarding_migration::OriginalCopyRequest;
use crate::shell::i18n::t;
use launcher::retained_view::installed_copy::CopyOwnerEvent;
use std::io;

pub(crate) fn start(
    request: OriginalCopyRequest,
    tree: super::super::state::WorkspaceTreeHandle,
    opening: NativeAppOpening,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> io::Result<()> {
    // Capture this session's launcher globals before entering the worker.
    spawn_reporting(
        std::thread::Builder::new().name("tilo-original-copy-bootstrap".into()),
        super::super::launch_readiness::inherit(move || run(request, tree, opening, wake)),
    )?;
    Ok(())
}
fn prepare(
    tree: &super::super::state::WorkspaceTreeHandle,
    opening: &NativeAppOpening,
) -> io::Result<Arc<retained_home::PreparedRetainedHome>> {
    let identity_dir = runtime_identity::identity_dir_from_env()?;
    let active = runtime_identity::resolve_active_identity(&identity_dir)
        .ok_or_else(|| io::Error::other("Active native identity is unavailable"))?;
    let signer = runtime_identity::load_from_path(active).map_err(io::Error::other)?;
    let room: tilo_desktop_model::RoomId =
        collab_types::person_room(&signer.group_public_key()).into();
    drop(signer);
    let app = installed_apps()
        .map_err(|error| io::Error::other(error.user_message()))?
        .into_iter()
        .find(|app| app.package_id == "so.tilo.imager")
        .ok_or_else(|| io::Error::other("Signed installed Imager is unavailable"))?;
    let owner = tree
        .read()
        .installed_native_owner()
        .ok_or_else(|| io::Error::other("Native identity owner is unavailable"))?;
    let certificate = owner
        .with_current(room, |view| Ok(view.device().clone()))
        .map_err(io::Error::other)?;
    let (_, current) = tilo_identity_host::broker::trusted_current_device(&room.to_string())
        .map_err(io::Error::other)?;
    if certificate != current {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    match native_app_open_access(&owner, room, &certificate, &app.package_id)
        .map_err(io::Error::other)?
    {
        NativeAppOpenAccess::Viewer => return Err(io::ErrorKind::PermissionDenied.into()),
        NativeAppOpenAccess::Author => (),
        NativeAppOpenAccess::Delegate => {
            let broker = opening.0.upgrade().map_err(io::Error::other)?;
            let job = broker.change_workspace_membership(
                &room.to_string(),
                &tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::GrantAppAuthorship {
                    app: app.package_id.clone(),
                }, &certificate,
            ).map_err(io::Error::other)?;
            drop(broker);
            let completed = job
                .completion
                .recv_timeout(Duration::from_secs(30))
                .map_err(io::Error::other)?;
            if !completed.code.is_empty() {
                return Err(io::Error::other(completed.message));
            }
        }
    }
    // Reload the owner's committed projection after normal durable delegation.
    native_launch_workspace(tree, room).map_err(io::Error::other)?;
    let native = crate::shell::collab_broker::NativePersonalSource::admit_for_socket(
        &launcher::services::collab_socket(),
        &hex::encode(certificate.user_public_key),
    )
    .map_err(io::Error::other)?;
    if native
        .retained_launch_certificate(room)
        .map_err(io::Error::other)?
        != certificate
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let identity = crate::identity_registry::selected_identity_path(&room.to_string())
        .map_err(io::Error::other)?;
    let command = build_launch_command_for(
        &app,
        &selected_execution_program().map_err(|error| io::Error::other(error.user_message()))?,
        None,
        room,
        &LaunchExtras {
            capabilities_file: None,
            identity_path: identity.as_deref(),
            render_node: None,
            document: None,
            uri: None,
        },
    );
    Ok(Arc::new(retained_home::PreparedRetainedHome::prepare(
        &app, command, owner, native, room,
    )?))
}
fn run(
    request: OriginalCopyRequest,
    tree: super::super::state::WorkspaceTreeHandle,
    opening: NativeAppOpening,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let emit = |event| {
        let result = request.events.send(event);
        wake();
        result.is_ok()
    };
    let operation = request.operation.clone();
    let mut custody = None;
    let mut cleanup_finished = None;
    let mut cleanup_finished_at = None;
    let mut transfer_complete = false;
    let mut terminal = false;
    let mut retry_requested = false;
    loop {
        if custody.is_none() && !terminal {
            let result = prepare(&tree, &opening).and_then(|prepared| {
                prepared.bootstrap_original_copy(
                    &operation,
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                )
            });
            match result {
                Ok(admitted) => custody = Some(admitted),
                Err(error) => {
                    terminal = true;
                    if !emit(CopyOwnerEvent::Failed {
                        operation: operation.clone(),
                        message: error.to_string(),
                    }) {
                        return;
                    }
                }
            }
        }
        match request.commands.try_recv() {
            Ok(launcher::retained_view::installed_copy::CopyOwnerCommand::Retry {})
                if terminal && !transfer_complete =>
            {
                retry_requested = true;
            }
            Ok(command) => {
                if let Some(custody) = custody.as_mut() {
                    if let Err(error) = custody.send_owner_command(&command) {
                        terminal = true;
                        let event = if transfer_complete {
                            CopyOwnerEvent::CleanupPending {
                                operation: operation.clone(),
                                message: t("onboarding-copy-cleanup-unavailable"),
                                resumable: false,
                            }
                        } else {
                            CopyOwnerEvent::Failed {
                                operation: operation.clone(),
                                message: error.to_string(),
                            }
                        };
                        if !emit(event) {
                            return;
                        }
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            Err(std::sync::mpsc::TryRecvError::Empty) => (),
        }
        if terminal && retry_requested {
            // A new certified helper may reopen recovery only after both actual
            // prior children are reaped. Unknown retirement keeps this owner.
            let retired = match custody.as_mut() {
                Some(custody) => matches!(custody.try_wait(), Ok(Some(_))),
                None => true,
            };
            if retired {
                drop(custody.take());
                terminal = false;
                retry_requested = false;
                continue;
            }
        }
        if terminal {
            if cleanup_finished.is_some() {
                let retirement = custody
                    .as_mut()
                    .ok_or_else(|| io::Error::other("Copy retirement custody unavailable"))
                    .and_then(|owned| owned.try_observe_cleanup());
                let stopped = match retirement {
                    Ok(stopped) => stopped,
                    Err(error) => {
                        // Keep the same actual custody on refusal; an observed
                        // join/exit error cannot become cleanup or RW admission.
                        cleanup_finished = None;
                        cleanup_finished_at = None;
                        if !emit(CopyOwnerEvent::CleanupPending {
                            operation: operation.clone(),
                            message: error.to_string(),
                            resumable: false,
                        }) {
                            return;
                        }
                        continue;
                    }
                };
                if stopped.is_some_and(|status| status.success()) {
                    if !emit(cleanup_finished.take().unwrap()) {
                        return;
                    }
                    cleanup_finished_at = None;
                } else if stopped.is_some() {
                    cleanup_finished = None;
                    cleanup_finished_at = None;
                    if !emit(CopyOwnerEvent::CleanupPending {
                        operation: operation.clone(),
                        message: t("onboarding-copy-cleanup-unconfirmed"),
                        resumable: false,
                    }) {
                        return;
                    }
                } else if cleanup_finished_at.is_some_and(|started: std::time::Instant| {
                    started.elapsed() >= Duration::from_secs(10)
                }) {
                    cleanup_finished_at = None;
                    if !emit(CopyOwnerEvent::CleanupPending {
                        operation: operation.clone(),
                        message: t("onboarding-copy-cleanup-unconfirmed"),
                        resumable: false,
                    }) {
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        let Some(admitted) = custody.as_mut() else {
            continue;
        };
        match admitted.receive_owner_event() {
            Ok(Some(event)) => {
                if event.operation() != operation {
                    terminal = true;
                    let event = if transfer_complete {
                        CopyOwnerEvent::CleanupPending {
                            operation: operation.clone(),
                            message: t("onboarding-copy-cleanup-mismatch"),
                            resumable: false,
                        }
                    } else {
                        CopyOwnerEvent::Failed {
                            operation: operation.clone(),
                            message: t("onboarding-copy-receipt-mismatch"),
                        }
                    };
                    if !emit(event) {
                        return;
                    }
                    continue;
                }
                if let CopyOwnerEvent::Complete {
                    operation,
                    home_cipher_sha256,
                    applications_cipher_sha256,
                } = &event
                {
                    transfer_complete = true;
                    if admitted.send_owner_command(
                        &launcher::retained_view::installed_copy::CopyOwnerCommand::AcknowledgeComplete {
                            operation: operation.clone(), home_cipher_sha256: home_cipher_sha256.clone(), applications_cipher_sha256: applications_cipher_sha256.clone(),
                        },
                    ).is_err() {
                        terminal = true;
                        if !emit(event.clone()) || !emit(CopyOwnerEvent::CleanupPending { operation: operation.clone(), message: t("onboarding-copy-cleanup-unavailable"), resumable: false }) { return; }
                        continue;
                    }
                }
                if let CopyOwnerEvent::CleanupFinished {
                    operation,
                    metadata_quarantined,
                } = &event
                {
                    if admitted.send_owner_command(&launcher::retained_view::installed_copy::CopyOwnerCommand::AcknowledgeCleanup { operation: operation.clone(), metadata_quarantined: *metadata_quarantined }).is_err() {
                        terminal = true;
                        if !emit(CopyOwnerEvent::CleanupPending { operation: operation.clone(), message: t("onboarding-copy-cleanup-confirmation-unavailable"), resumable: false }) { return; }
                        continue;
                    }
                }
                terminal = matches!(
                    &event,
                    CopyOwnerEvent::CleanupFinished { .. }
                        | CopyOwnerEvent::Cancelled { .. }
                        | CopyOwnerEvent::Failed { .. }
                );
                if matches!(&event, CopyOwnerEvent::CleanupFinished { .. }) {
                    cleanup_finished = Some(event);
                    cleanup_finished_at = Some(std::time::Instant::now());
                } else if !emit(event) {
                    return;
                }
            }
            Ok(None) => (),
            Err(error) => {
                terminal = true;
                let event = if transfer_complete {
                    CopyOwnerEvent::CleanupPending {
                        operation: operation.clone(),
                        message: t("onboarding-copy-cleanup-unavailable"),
                        resumable: false,
                    }
                } else {
                    CopyOwnerEvent::Failed {
                        operation: operation.clone(),
                        message: error.to_string(),
                    }
                };
                if !emit(event) {
                    return;
                }
            }
        }
    }
}
