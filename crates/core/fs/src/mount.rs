//! Where and how a workspace is mounted: what a mount is given, what it serves, and the loop
//! that runs it until the session ends.

use super::{
    AclHandle, Arc, BackendArgs, Config, Heuristics, Hydrator, IdentityKey, KeptTree, MountOption,
    Mutex, Namespace, PathBuf, QuickFS, Result, ServedSocket, SessionACL, StopSignals,
    mount_point_owner, parse_owner, session_supervisor,
};
/// The native backend is Linux's: other platforms serve the mount through FUSE.
#[cfg(target_os = "linux")]
use super::{native_config, run_native};

/// FUSE worker threads: `TILO_FS_FUSE_THREADS` when set, else one per CPU between 2 and 16.
fn fuse_worker_threads() -> usize {
    std::env::var("TILO_FS_FUSE_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|threads| *threads > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(4, std::num::NonZeroUsize::get)
                .clamp(2, 16)
        })
}

/// Where and how [`run_mount`] mounts a workspace: the workspace it is,
/// the mount point and cache root, optional thumbnail, share-control and storage-accounting
/// endpoints, and the backend.
pub(super) struct MountConfig {
    pub(super) workspace_id: uuid::Uuid,
    pub(super) mount_point: PathBuf,
    /// The tree's documents kept on disk, if anywhere: every change is journaled there before
    /// the request that made it is answered.
    pub(super) kept: Option<Arc<KeptTree>>,
    /// Makes what the mount has already done durable before it stops, so nothing it answered is
    /// lost by the stop itself.
    pub(super) shutdown: Option<Arc<dyn Fn() + Send + Sync>>,
    pub(super) predictive_cache_root: Option<PathBuf>,
    pub(super) thumb_dir: Option<PathBuf>,
    pub(super) storage_sock: Option<PathBuf>,
    pub(super) read_only: bool,
    pub(super) backend: Option<(BackendArgs, [u8; 32])>,
}

pub(super) struct MountServices {
    pub(super) namespace: Arc<Mutex<Namespace>>,
    pub(super) hydrator: Arc<Hydrator>,
    pub(super) acl: Option<(AclHandle, IdentityKey)>,
    pub(super) availability: Option<Arc<Mutex<tilo_fs::AvailabilityRegistry>>>,
    pub(super) eviction_queue: Option<Arc<tokio::sync::Mutex<tilo_fs::eviction::EvictionQueue>>>,
    pub(super) actor: Option<fs_protocol::FileActor>,
}

#[allow(
    clippy::too_many_lines,
    reason = "Mount setup wires independent sync, ACL, storage, history, sharing, recovery, and readiness services before supervising FUSE shutdown."
)]
pub(super) async fn run_mount(
    config: MountConfig,
    services: MountServices,
) -> Result<MountOutcome> {
    let MountConfig {
        workspace_id,
        mount_point,
        kept,
        shutdown,
        predictive_cache_root,
        thumb_dir,
        storage_sock,
        read_only,
        backend,
    } = config;
    let MountServices {
        namespace,
        hydrator,
        acl,
        availability,
        eviction_queue,
        actor,
    } = services;
    let content = Arc::clone(&hydrator.store);
    // Before anything is served: from here on a stop signal ends the mount through the final save
    // instead of killing the process.
    let mut stop = StopSignals::install()?;
    // A daemon that ended without unmounting (a kill, a crash, a mount this process may not
    // unmount) leaves a mount answering nothing; the path cannot be mounted again, and making it
    // fails, until the dead mount is gone.
    tilo_fs::fuse_session::clear_stale_mount(&mount_point)?;
    if !mount_point.exists() {
        match std::fs::create_dir_all(&mount_point) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // The mount died as the path was being made; clear it and make the path again.
                tilo_fs::fuse_session::clear_stale_mount(&mount_point)?;
                std::fs::create_dir_all(&mount_point)?;
            }
            Err(error) => return Err(error.into()),
        }
    }

    let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
    // The person's prefetch switch and cache budget, applied before the mount is served.
    let (cache_policies, kept_policy) = tilo_fs::cache_policy::CachePolicies::load(
        predictive_cache_root
            .as_ref()
            .map(|root| root.join(tilo_fs::cache_policy::CACHE_POLICY_FILE)),
        Arc::clone(&heuristics),
        Arc::clone(hydrator.store.files()),
    );
    if let Err(error) = kept_policy {
        eprintln!("ignoring the kept cache policy: {error:#}");
    }
    let cache_policies = Arc::new(cache_policies);
    let predictive_policy_task =
        tilo_fs::smart_monitor::start(Arc::clone(&heuristics), predictive_cache_root.clone()).await;
    // Settings' socket receives these same durable services below; it never falls back to
    // Hydrator-only proof or direct blob deletion.
    let storage_availability = availability.clone();
    let storage_eviction_queue = eviction_queue.clone();
    #[cfg(target_os = "linux")]
    let signalled_availability = availability.clone();
    // The watcher that turns a folder's own changes into signals reads the tree too.
    #[cfg(target_os = "linux")]
    let signalled_namespace = Arc::clone(&namespace);
    let fs = QuickFS::new(namespace, hydrator, heuristics);
    let fs = match availability {
        Some(registry) => fs.with_availability(registry),
        None => fs,
    };
    let fs = match eviction_queue {
        Some(queue) => fs.with_eviction_queue(queue),
        None => fs,
    };
    let fs = match actor {
        Some(actor) => fs.with_actor(actor),
        None => fs,
    };
    // Who writes, from the process each request comes from: an agent session, an app, another
    // program, or the person (the same resolver a computer's `tilo login` folder uses).
    let fs = fs.with_writers(Arc::new(tilo_fs::writer::WriterResolver::default()));
    // Bind the capability list, if given, before the mount goes live, so every op is checked from the start.
    let fs = match acl {
        Some((acl, identity)) => fs.with_acl(acl, identity),
        None => fs,
    };
    // Durable pins become an active residency promise only after the live ACL
    // is attached, so inherited folder hydration cannot cross the mount view.
    let fs = fs.with_pinned_hydration();
    // Host-side thumbnails: generate previews as metadata, served via the `user.tilo.thumb` xattr.
    let fs = match thumb_dir {
        Some(dir) => match tilo_fs::ThumbProvider::open(&dir) {
            Ok(provider) => fs.with_thumbs(provider),
            Err(e) => {
                eprintln!(
                    "thumbnail cache unavailable at {} ({e}); previews disabled",
                    dir.display()
                );
                fs
            }
        },
        None => fs,
    };
    // What this device learned about files opened together survives the restart, per workspace.
    let fs = match predictive_cache_root.as_ref() {
        Some(root) => fs.with_kept_prefetch_history(root, workspace_id),
        None => fs,
    };
    // Resuming the evictions a restart interrupted waits on this device's store through the
    // hydrator's synchronous bridge, which a thread driving this runtime must not do: it runs on a
    // thread of its own while the mount waits for it.
    let fs = {
        let (resumed, done) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("tilo-fs-evictions".into())
            .spawn(move || {
                fs.resume_pending_evictions();
                let _ = resumed.send(fs);
            })
            .map_err(|error| {
                anyhow::anyhow!("a thread to resume the mount's evictions: {error}")
            })?;
        done.await
            .map_err(|_| anyhow::anyhow!("resuming the mount's pending evictions stopped"))?
    };
    // Bytes accepted by fsync, or whose commit failed, are staged per workspace so they survive a
    // crash or unmount and are replayed here.
    let unsaved_dir = predictive_cache_root
        .as_ref()
        .filter(|_| !read_only)
        .map(|root| root.join("unsaved-writes").join(workspace_id.to_string()));
    let fs = match unsaved_dir {
        Some(dir) => match tilo_fs::unsaved::UnsavedStore::open(&dir) {
            Ok(store) => {
                let fs = fs.with_unsaved_store(store);
                let placed = fs.recover_unsaved_writes();
                if placed > 0 {
                    println!("Recovered {placed} staged unsaved file(s)");
                }
                fs
            }
            Err(error) => {
                eprintln!(
                    "unsaved-write staging unavailable at {} ({error}); fsync commits directly",
                    dir.display()
                );
                fs
            }
        },
        None => fs,
    };

    // Written bytes of open files are staged in sparse files beside the cache rather than in
    // memory, so a large write never has to fit in RAM.
    let fs = match predictive_cache_root.as_ref().filter(|_| !read_only) {
        Some(root) => {
            fs.with_write_stage_dir(root.join("write-stage").join(workspace_id.to_string()))
        }
        None => fs,
    };

    let storage_authority = storage_sock.as_ref().map(|_| {
        tilo_fs::storage::StorageAuthority::new(
            Arc::clone(&fs.namespace),
            Arc::clone(&fs.hydrator),
            fs.write_buffers_handle(),
            storage_availability,
            storage_eviction_queue,
            // The name the person's devices tell this workspace by: its mount's directory, or its
            // id where the mount has none.
            mount_point
                .file_name()
                .and_then(|name| name.to_str())
                .map_or_else(|| workspace_id.to_string(), str::to_owned),
        )
        .with_cache_policy(Arc::clone(&cache_policies))
    });

    let _storage = storage_sock
        .zip(storage_authority)
        .and_then(|(sock, authority)| {
            let _ = std::fs::remove_file(&sock);
            // Through the shared helper: a socket is reached whatever the path's length.
            match tilo_files::native::socket::bind(&sock)
                .and_then(|listener| {
                    listener.set_nonblocking(true)?;
                    Ok(listener)
                })
                .and_then(tokio::net::UnixListener::from_std)
            {
                Ok(listener) => {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o660));
                    println!("Storage accounting socket at {}", sock.display());
                    tokio::spawn(tilo_fs::storage::serve(listener, authority));
                    Some(ServedSocket(sock))
                }
                Err(error) => {
                    eprintln!("could not bind storage socket {}: {error}", sock.display());
                    None
                }
            }
        });

    // Both backends: every change the mount makes is journaled on disk under the namespace's
    // lock, before the request that made it is answered, so a process killed at any instant
    // starts again holding every change it answered for.
    let journal: Option<tilo_fs::fs::ChangeJournal> = kept.clone().map(|kept| {
        Arc::new(move |namespace: &mut Namespace| kept.journal(namespace))
            as tilo_fs::fs::ChangeJournal
    });
    let fs = match journal.clone() {
        Some(journal) => fs.with_change_journal(journal),
        None => fs,
    };
    // Content and journal reach the disk on one barrier: content first, then the journals that
    // name it, so a crash never leaves a journaled tree entry whose bytes are missing. A copy of
    // thousands of small files pays one sync per window instead of one per file; `fsync`,
    // `fsyncdir` and unmount still sync at once. `TILO_FS_CONTENT_WINDOW=0` keeps the per-write
    // sync, for isolating a problem.
    if std::env::var("TILO_FS_CONTENT_WINDOW").as_deref() != Ok("0") {
        fs.sync_on_window(
            Arc::clone(&content),
            kept.clone()
                .map(|kept| Arc::new(move || kept.sync()) as tilo_fs::fs::DurabilityHook),
        );
    }
    #[cfg(target_os = "linux")]
    if let Some((args, device)) = backend.as_ref()
        && let Some(cfg) = native_config(
            args,
            *device,
            workspace_id,
            &mount_point,
            predictive_cache_root.as_deref(),
            read_only,
        )?
    {
        let outcome = run_native(fs, cfg, &mut stop).await;
        if let Some(task) = predictive_policy_task {
            task.abort();
        }
        return outcome;
    }
    #[cfg(not(target_os = "linux"))]
    let _ = &backend;

    // Kernel passthrough for fully local read-only opens is opt-in while the backing files are a
    // per-open materialisation of the chunk store.
    let fs = match predictive_cache_root
        .as_ref()
        .filter(|_| std::env::var_os("TILO_FS_PASSTHROUGH").is_some())
    {
        Some(root) => fs.with_passthrough(root.join("passthrough")),
        None => fs,
    };
    // The local person owns the files on either backend; without one named,
    // FUSE presents each caller as their owner.
    // Every caller is shown one owner - the mount point's, or the one named - so the kernel can
    // keep the attributes it was answered: presented per caller, they may never be cached, and
    // listing a folder asks for each file's attributes again on every listing.
    let owner = match backend
        .as_ref()
        .and_then(|(args, _)| args.native_owner.as_deref())
    {
        Some(spec) => Some(parse_owner(spec)?),
        None => mount_point_owner(&mount_point),
    };
    let fs = fs.with_owner(owner);
    // Watchers on the mount hear of the changes the kernel did not make (a peer's edit) instead of
    // polling. A read-only mount cannot take the signal, so its watchers keep polling.
    #[cfg(target_os = "linux")]
    let change_signal = if read_only {
        None
    } else {
        match tilo_fs::change_signal::ChangeSignal::new() {
            Ok(signal) => Some(Arc::new(signal)),
            Err(error) => {
                eprintln!("folders on the mount will not signal their changes: {error}");
                None
            }
        }
    };
    #[cfg(target_os = "linux")]
    let fs = match &change_signal {
        Some(signal) => fs.with_change_signal(Arc::clone(signal)),
        None => fs,
    };

    let mut config = Config::default();
    config.mount_options = vec![MountOption::FSName("quickfs".to_string())];
    // Several workers, each with its own /dev/fuse descriptor, so one slow request does not stall
    // every other process using the mount.
    config.n_threads = Some(fuse_worker_threads());
    config.clone_fd = true;
    if read_only {
        config.mount_options.push(MountOption::RO);
    }
    // The installed service uses auto-unmount and the system-wide Tilo mount policy. The mandatory
    // process harness runs all peers under one test UID and opts into the kernel's owner-only mode,
    // which needs no host `/etc/fuse.conf` change.
    if std::env::var_os("TILO_FS_FUSE_OWNER_ONLY").is_some() {
        config.acl = SessionACL::Owner;
    } else {
        config.acl = SessionACL::All;
        config.mount_options.push(MountOption::AutoUnmount);
    }

    let mp = mount_point.clone();
    let (unmount_tx, session_events) = std::sync::mpsc::channel();
    let session_ended = unmount_tx.clone();
    // Keep the BackgroundSession under a supervisor instead of consuming it in `join()`: a rebind
    // must unmount the old FUSE session and wait for its worker before this process returns to
    // systemd.
    let mut mount_task = tokio::task::spawn_blocking(move || {
        // A daemon that ended without unmounting (a kill, a crash) leaves its mount answering
        // nothing; the path cannot be mounted again until it is gone.
        tilo_fs::fuse_session::clear_stale_mount(&mp)?;
        let session = fuser::spawn_mount(fs, &mp, &config)?;
        // Held until the session ends: dropping it stops the signals.
        #[cfg(target_os = "linux")]
        let _signals = change_signal.and_then(|signal| {
            signal
                .start(
                    signalled_namespace,
                    signalled_availability,
                    &mp,
                    session.notifier(),
                    &tokio::runtime::Handle::current(),
                )
                .map_err(|error| {
                    eprintln!(
                        "folders on {} will not signal their changes; watchers poll: {error}",
                        mp.display()
                    );
                })
                .ok()
        });
        let session = session_supervisor::MountedSession {
            session,
            mount_point: mp,
        };
        session_supervisor::supervise(session, &session_events, session_ended)
    });
    println!("Mounting QuickFS at {}", mount_point.display());

    // Exit cleanly on a stop signal or mount exit, so a crash-looping service does not hang.
    let outcome = tokio::select! {
        res = &mut mount_task => {
            match res {
            Ok(Ok(())) => println!("Filesystem unmounted."),
            Ok(Err(e)) => eprintln!("FUSE mount exited with error: {e}"),
            Err(e) => eprintln!("mount task failed: {e}"),
            }
            MountOutcome::Unmounted
        },
        signal = stop.recv() => {
            println!("{signal}, exiting...");
            // What the mount has already answered reaches the disk before it stops answering.
            if let Some(shutdown) = &shutdown {
                shutdown();
            }
            let _ = unmount_tx.send(session_supervisor::SessionEvent::Unmount);
            let _ = (&mut mount_task).await;
            MountOutcome::Unmounted
        },
    };
    if let Some(task) = predictive_policy_task {
        task.abort();
    }
    Ok(outcome)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MountOutcome {
    Unmounted,
}
