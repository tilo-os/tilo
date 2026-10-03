//! Legacy apps on a compute worker that advertises `kvm`, and the request
//! an environment worker sends to launch one.
//!
//! A guest in a relay environment runs `code .`; its broker asks the worker
//! hosting the session, which calls [`request_legacy_launch`] on its
//! connection to the relay. The relay picks a worker advertising `kvm` and
//! opens a legacy stream to it ([`WorkerStreamOpen::Legacy`]), served here
//! by [`LegacyWorker::serve`]: the worker's [`LegacyVmHost`] runs the app in
//! the member's legacy VM, its windows go up as encoded
//! [`legacy_window`](tilo_relay_protocol::legacy_window) frames, and the
//! member's input comes back down. Wall time each launch runs is reported as
//! [`LegacyFrame::Usage`] so the relay meters it per member.
//!
//! [`WorkerStreamOpen::Legacy`]: tilo_relay_protocol::environment::WorkerStreamOpen::Legacy

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tilo_relay_protocol::environment::{
    ENVIRONMENT_PROTOCOL_VERSION, LaunchId, LegacyFrame, LegacyGuestOs, LegacyLaunch,
    LegacyLaunchOutcome, LegacyUsage, MemberIdentity, QuotaKind, Refusal, SessionId, StreamOpen,
    WindowInput, bounded_detail,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::environment_host::{
    EnvironmentStreamError, LaunchError, MemberArea, MemberContext, MemberQuota, read_message,
    write_message,
};

/// How often a running launch's wall time is reported.
pub const USAGE_REPORT_INTERVAL: Duration = Duration::from_secs(60);

/// A running legacy app, as the worker drives it.
pub struct LegacyApp {
    /// Its frames: [`LegacyFrame::Started`], window frames, and finally
    /// [`LegacyFrame::Exited`]. The channel closing also ends the launch.
    pub frames: mpsc::Receiver<LegacyFrame>,
    /// Input for its windows.
    pub input: mpsc::Sender<WindowInput>,
}

/// Runs legacy apps in members' legacy VMs on this worker.
pub trait LegacyVmHost: Send + Sync + 'static {
    /// Launch `request` for `member`. Called on the blocking pool.
    ///
    /// # Errors
    ///
    /// Returns an error when the VM or the app cannot be started.
    fn launch(
        &self,
        member: &MemberContext,
        launch: LaunchId,
        request: &LegacyLaunch,
    ) -> Result<LegacyApp, LaunchError>;
}

/// Each member's running launches and the guest OS each runs in.
type RunningLaunches = HashMap<[u8; 32], Vec<(LaunchId, LegacyGuestOs)>>;

/// The legacy side of a worker advertising `kvm`.
#[derive(Clone)]
pub struct LegacyWorker {
    inner: Arc<LegacyInner>,
}

struct LegacyInner {
    host: Arc<dyn LegacyVmHost>,
    root: std::path::PathBuf,
    quota: MemberQuota,
    running: Mutex<RunningLaunches>,
}

impl LegacyWorker {
    /// A worker running apps through `host`, with members' areas under
    /// `root`.
    #[must_use]
    pub fn new(host: Arc<dyn LegacyVmHost>, root: std::path::PathBuf, quota: MemberQuota) -> Self {
        Self {
            inner: Arc::new(LegacyInner {
                host,
                root,
                quota,
                running: Mutex::new(HashMap::new()),
            }),
        }
    }

    fn running(&self) -> std::sync::MutexGuard<'_, RunningLaunches> {
        self.inner
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Launches running for `user`.
    #[must_use]
    pub fn member_launches(&self, user: &[u8; 32]) -> usize {
        self.running().get(user).map_or(0, Vec::len)
    }

    /// Admit a launch against the member's VM quota: one VM per guest OS,
    /// so a launch into an OS whose VM already runs is always admitted.
    fn admit(&self, member: &MemberIdentity, launch: LaunchId, guest: LegacyGuestOs) -> bool {
        let mut running = self.running();
        let launches = running.entry(member.user).or_default();
        let mut vms: Vec<LegacyGuestOs> = launches.iter().map(|(_, os)| *os).collect();
        vms.sort_unstable();
        vms.dedup();
        let admitted = vms.contains(&guest)
            || u32::try_from(vms.len()).unwrap_or(u32::MAX) < self.inner.quota.max_legacy_vms;
        if admitted {
            launches.push((launch, guest));
        }
        if launches.is_empty() {
            running.remove(&member.user);
        }
        drop(running);
        admitted
    }

    fn release(&self, member: &MemberIdentity, launch: &LaunchId) {
        let mut running = self.running();
        if let Some(launches) = running.get_mut(&member.user) {
            launches.retain(|(id, _)| id != launch);
            if launches.is_empty() {
                running.remove(&member.user);
            }
        }
    }

    /// Serve one legacy stream the relay opened.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream fails or a frame is malformed.
    pub async fn serve<S, R>(
        &self,
        member: MemberIdentity,
        launch: LaunchId,
        request: LegacyLaunch,
        send: &mut S,
        receive: &mut R,
    ) -> Result<(), EnvironmentStreamError>
    where
        S: AsyncWrite + Unpin + Send,
        R: AsyncRead + Unpin + Send,
    {
        let refuse = |reason, detail: &str| LegacyFrame::Refused {
            reason,
            detail: bounded_detail(detail),
        };
        if !request.valid() {
            write_message(
                send,
                &refuse(Refusal::InvalidRequest, "invalid legacy launch"),
            )
            .await?;
            return Ok(());
        }
        if !self.admit(&member, launch, request.guest) {
            write_message(
                send,
                &refuse(
                    Refusal::Quota(QuotaKind::LegacyVms),
                    "you already run as many legacy VMs as this relay allows",
                ),
            )
            .await?;
            return Ok(());
        }
        let context = MemberContext {
            identity: member,
            area: MemberArea::of(&self.inner.root, &member.user),
            quota: self.inner.quota,
        };
        let host = Arc::clone(&self.inner.host);
        let launched = tokio::task::spawn_blocking(move || host.launch(&context, launch, &request))
            .await
            .map_err(|_| LaunchError::Failed("legacy launch panicked".into()))
            .and_then(|launched| launched);
        let app = match launched {
            Ok(app) => app,
            Err(error) => {
                self.release(&member, &launch);
                let reason = match error {
                    LaunchError::Quota(kind) => Refusal::Quota(kind),
                    LaunchError::Failed(_) => Refusal::LaunchFailed,
                };
                write_message(send, &refuse(reason, &error.to_string())).await?;
                return Ok(());
            }
        };
        let outcome = carry_app(app, send, receive).await;
        self.release(&member, &launch);
        let _ = send.shutdown().await;
        outcome
    }
}

async fn carry_app<S, R>(
    app: LegacyApp,
    send: &mut S,
    receive: &mut R,
) -> Result<(), EnvironmentStreamError>
where
    S: AsyncWrite + Unpin + Send,
    R: AsyncRead + Unpin + Send,
{
    let LegacyApp { mut frames, input } = app;
    let started = Instant::now();
    let mut reported = started;
    let first_report = started
        .checked_add(USAGE_REPORT_INTERVAL)
        .unwrap_or(started);
    let mut report = tokio::time::interval_at(first_report, USAGE_REPORT_INTERVAL);
    let usage = |now: Instant, reported: &mut Instant| {
        let elapsed = now.saturating_duration_since(*reported);
        *reported = now;
        LegacyFrame::Usage(LegacyUsage {
            vm_running_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            peak_memory_bytes: 0,
        })
    };
    let mut receiving = true;
    loop {
        tokio::select! {
            frame = frames.recv() => {
                let Some(frame) = frame else {
                    write_message(send, &usage(Instant::now(), &mut reported)).await?;
                    write_message(send, &LegacyFrame::Exited { code: None }).await?;
                    return Ok(());
                };
                let last = matches!(frame, LegacyFrame::Exited { .. } | LegacyFrame::Refused { .. });
                if last {
                    write_message(send, &usage(Instant::now(), &mut reported)).await?;
                }
                write_message(send, &frame).await?;
                if last {
                    return Ok(());
                }
            }
            _ = report.tick() => {
                write_message(send, &usage(Instant::now(), &mut reported)).await?;
            }
            incoming = read_message::<_, WindowInput>(receive), if receiving => {
                match incoming? {
                    // The relay stopped forwarding input; windows still flow.
                    None => receiving = false,
                    Some(event) => {
                        if event.valid() {
                            let _ = input.try_send(event);
                        }
                    }
                }
            }
        }
    }
}

/// Ask the relay to launch a legacy app for the member whose `session` this
/// worker hosts.
///
/// # Errors
///
/// Returns an error when the stream to the relay fails.
pub async fn request_legacy_launch(
    connection: &iroh::endpoint::Connection,
    workspace_id: u64,
    session: SessionId,
    request: LegacyLaunch,
) -> Result<LegacyLaunchOutcome, EnvironmentStreamError> {
    let (mut send, mut receive) = connection
        .open_bi()
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    write_message(
        &mut send,
        &StreamOpen::LegacyLaunch {
            version: ENVIRONMENT_PROTOCOL_VERSION,
            workspace_id,
            session,
            request,
        },
    )
    .await?;
    let _ = send.finish();
    read_message(&mut receive)
        .await?
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl LegacyVmHost for Echo {
        fn launch(
            &self,
            _member: &MemberContext,
            _launch: LaunchId,
            request: &LegacyLaunch,
        ) -> Result<LegacyApp, LaunchError> {
            if request.app == "missing" {
                return Err(LaunchError::Failed("no such app".into()));
            }
            let (frames_tx, frames) = mpsc::channel(8);
            let (input, mut input_rx) = mpsc::channel::<WindowInput>(8);
            tokio::spawn(async move {
                let _ = frames_tx.send(LegacyFrame::Started).await;
                while let Some(event) = input_rx.recv().await {
                    let surface = u8::try_from(event.surface).unwrap_or(u8::MAX);
                    let _ = frames_tx
                        .send(LegacyFrame::Window(vec![0xF1, surface]))
                        .await;
                    if event.surface == 0 {
                        let _ = frames_tx.send(LegacyFrame::Exited { code: Some(0) }).await;
                        break;
                    }
                }
            });
            Ok(LegacyApp { frames, input })
        }
    }

    fn member(user: u8) -> MemberIdentity {
        MemberIdentity {
            user: [user; 32],
            device: [user; 32],
        }
    }

    fn launch(guest: LegacyGuestOs, app: &str) -> LegacyLaunch {
        LegacyLaunch {
            guest,
            app: app.into(),
            args: Vec::new(),
            cwd: "/".into(),
        }
    }

    #[test]
    fn one_vm_per_guest_os_counts_against_the_member_quota() {
        let quota = MemberQuota {
            max_legacy_vms: 1,
            ..MemberQuota::default()
        };
        let worker = LegacyWorker::new(
            Arc::new(Echo),
            std::path::PathBuf::from("/nonexistent"),
            quota,
        );
        let alice = member(1);
        assert!(worker.admit(&alice, [1; 16], LegacyGuestOs::Linux));
        // A second app in the same VM is not a second VM.
        assert!(worker.admit(&alice, [2; 16], LegacyGuestOs::Linux));
        assert!(!worker.admit(&alice, [3; 16], LegacyGuestOs::Windows));
        // Another member's quota is their own.
        assert!(worker.admit(&member(2), [4; 16], LegacyGuestOs::Windows));
        worker.release(&alice, &[1; 16]);
        worker.release(&alice, &[2; 16]);
        assert_eq!(worker.member_launches(&alice.user), 0);
        assert!(worker.admit(&alice, [3; 16], LegacyGuestOs::Windows));
    }

    #[tokio::test]
    async fn a_launch_streams_windows_takes_input_and_meters_its_wall_time() {
        let worker = LegacyWorker::new(
            Arc::new(Echo),
            std::path::PathBuf::from("/nonexistent"),
            MemberQuota::default(),
        );
        let (mut relay_end, mut worker_end) = tokio::io::duplex(64 * 1024);
        let (mut worker_read, mut worker_write) = tokio::io::split(&mut worker_end);
        let serving = worker.serve(
            member(1),
            [9; 16],
            launch(LegacyGuestOs::Linux, "code"),
            &mut worker_write,
            &mut worker_read,
        );
        let driving = async {
            assert_eq!(
                read_message::<_, LegacyFrame>(&mut relay_end)
                    .await
                    .unwrap(),
                Some(LegacyFrame::Started)
            );
            for surface in [5, 0] {
                write_message(
                    &mut relay_end,
                    &WindowInput {
                        launch: [9; 16],
                        surface,
                        events: vec![
                            tilo_relay_protocol::legacy_window::InputEvent::PointerAxis {
                                dx: 0.0,
                                dy: 1.0,
                            },
                        ],
                    },
                )
                .await
                .unwrap();
                assert_eq!(
                    read_message::<_, LegacyFrame>(&mut relay_end)
                        .await
                        .unwrap(),
                    Some(LegacyFrame::Window(vec![
                        0xF1,
                        u8::try_from(surface).unwrap()
                    ]))
                );
            }
            assert!(matches!(
                read_message::<_, LegacyFrame>(&mut relay_end)
                    .await
                    .unwrap(),
                Some(LegacyFrame::Usage(_))
            ));
            assert_eq!(
                read_message::<_, LegacyFrame>(&mut relay_end)
                    .await
                    .unwrap(),
                Some(LegacyFrame::Exited { code: Some(0) })
            );
        };
        let (served, ()) = tokio::join!(serving, driving);
        served.unwrap();
        assert_eq!(worker.member_launches(&[1; 32]), 0);
    }

    #[tokio::test]
    async fn a_failed_launch_is_refused_and_releases_its_quota() {
        let worker = LegacyWorker::new(
            Arc::new(Echo),
            std::path::PathBuf::from("/nonexistent"),
            MemberQuota::default(),
        );
        let (mut relay_end, mut worker_end) = tokio::io::duplex(64 * 1024);
        let (mut worker_read, mut worker_write) = tokio::io::split(&mut worker_end);
        worker
            .serve(
                member(1),
                [9; 16],
                launch(LegacyGuestOs::Linux, "missing"),
                &mut worker_write,
                &mut worker_read,
            )
            .await
            .unwrap();
        assert!(matches!(
            read_message::<_, LegacyFrame>(&mut relay_end)
                .await
                .unwrap(),
            Some(LegacyFrame::Refused {
                reason: Refusal::LaunchFailed,
                ..
            })
        ));
        assert_eq!(worker.member_launches(&[1; 32]), 0);
    }
}
