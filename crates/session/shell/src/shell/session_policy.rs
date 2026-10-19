//! Shared session startup and presentation acknowledgement policy.
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
static WORKSPACE_SALT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
/// Result of waiting for the precise lock frame which allows a pending sleep
/// request to proceed.  Kept pure so the failure policy is testable without a
/// compositor, logind, seat, or GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepLockFrameStatus {
    /// The frame is still in the healthy render queue.
    Awaiting,
    /// The required frame has retired; it is safe to acknowledge the delay
    /// inhibitor.
    Retired,
    /// The render worker exited before retiring the required frame.  This is
    /// intentionally not an acknowledgement: fail closed and let logind's
    /// own bounded delay-inhibitor policy decide when to proceed.
    PresenterLost,
}

pub fn sleep_lock_frame_status(
    serial: u64,
    presented: u64,
    presenter_finished: bool,
) -> SleepLockFrameStatus {
    if presented >= serial {
        SleepLockFrameStatus::Retired
    } else if presenter_finished {
        SleepLockFrameStatus::PresenterLost
    } else {
        SleepLockFrameStatus::Awaiting
    }
}

/// Only unlock a failed suspend when the compositor actually entered the
/// pre-sleep state and that state created the lock. A `CanSuspend` denial can
/// arrive before preparation, and must not clear a user's unrelated lock.
pub fn should_unlock_after_suspend_failure(was_prepared: bool, was_locked: bool) -> bool {
    was_prepared && !was_locked
}

fn read_workspace_salt(path: &Path) -> std::io::Result<u16> {
    let bytes = std::fs::read(path)?;
    let bytes: [u8; 2] = bytes.try_into().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "workspace peer salt must contain exactly two bytes",
        )
    })?;
    let salt = u16::from_be_bytes(bytes);
    if salt == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "workspace peer salt must be non-zero",
        ));
    }
    // A salt stored before the namespace was reserved may be that
    // namespace; this device mints beside it, never in it.
    Ok(tilo_desktop_model::ids::device_salt(salt))
}

/// Load the namespace salt used for durable workspace object ids, creating a
/// non-secret random one without creating or advertising a transport identity.
/// Existing installations retain the salt derived from their existing device
/// key on first migration.
/// # Errors
/// Returns filesystem, identity-key or invalid stored salt errors.
pub fn load_or_create_workspace_salt(
    state_path: &Path,
    peer_key_path: &Path,
) -> std::io::Result<u16> {
    let parent = state_path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let path = parent.join("workspace-peer-salt");
    if path.try_exists()? {
        return read_workspace_salt(&path);
    }

    let salt = if peer_key_path.try_exists()? {
        let key = crate::network::behaviour::load_or_create_keypair(peer_key_path)?;
        crate::network::behaviour::peer_salt_from_keypair(&key)
    } else {
        let bytes = crate::random::bytes::<2>();
        // Never `0` (no id space) or the reserved namespace.
        tilo_desktop_model::ids::device_salt(u16::from_be_bytes(bytes))
    };

    for _ in 0..16 {
        let sequence = WORKSPACE_SALT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".workspace-peer-salt-{}-{sequence}",
            std::process::id()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        if let Err(error) = file
            .write_all(&salt.to_be_bytes())
            .and_then(|()| file.sync_all())
        {
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        let claim = std::fs::hard_link(&temporary, &path);
        let _ = std::fs::remove_file(&temporary);
        match claim {
            Ok(()) => {
                std::fs::File::open(parent)?.sync_all()?;
                return Ok(salt);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return read_workspace_salt(&path);
            }
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a workspace-salt claim file",
    ))
}

/// A deferred first-login transport starts precisely once: after the trusted
/// identity ceremony has released the desktop, never while it is still
/// collecting secrets.
pub fn take_deferred_peer_sync_activation(deferred: &mut bool, onboarding_active: bool) -> bool {
    if *deferred && !onboarding_active {
        *deferred = false;
        true
    } else {
        false
    }
}

#[expect(
    clippy::fn_params_excessive_bools,
    reason = "four independent startup facts combine into one decision"
)]
pub fn should_activate_first_use(
    identity_missing: bool,
    progress_pending: bool,
    identity_recovery: bool,
    test_control_session: bool,
) -> bool {
    !test_control_session && (identity_missing || progress_pending || identity_recovery)
}

pub fn portal_screencast_should_publish(has_streams: bool, session_locked: bool) -> bool {
    has_streams && !session_locked
}

pub fn update_output_presentation_repaint_pending(
    pending: &mut bool,
    previous_surface_count: &mut usize,
    current_surface_count: usize,
    frame_changed: bool,
) -> bool {
    *pending |= frame_changed || *previous_surface_count != current_surface_count;
    *previous_surface_count = current_surface_count;
    *pending
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn onboarding_and_identity_policy_keep_sessions_and_salts_consistent() {
        // Phase `deferred_peer_sync_starts_in_the_session_that_finishes_onboarding`.
        {
            let mut deferred = true;
            assert!(!take_deferred_peer_sync_activation(&mut deferred, true));
            assert!(deferred, "onboarding retains the pending start");
            assert!(take_deferred_peer_sync_activation(&mut deferred, false));
            assert!(!deferred, "the current-session start is one-shot");
            assert!(!take_deferred_peer_sync_activation(&mut deferred, false));
        }

        // Phase `unfinished_post_identity_setup_reactivates_trusted_first_use_chrome`.
        {
            assert!(should_activate_first_use(false, true, false, false));
            assert!(
                !should_activate_first_use(false, false, false, false),
                "an established user without a journal must not see first-use again"
            );
            assert!(
                should_activate_first_use(false, false, true, false),
                "a missing secret-service key must reopen the recovery flow"
            );
            assert!(
                !should_activate_first_use(true, true, true, true),
                "the explicit development control session remains isolated"
            );
        }

        // Phase `fresh_workspace_salt_does_not_create_a_transport_identity`.
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let state_path = dir.path().join("config/state.json");
            let peer_key_path = dir.path().join("system/device-key");

            let first = load_or_create_workspace_salt(&state_path, &peer_key_path)
                .expect("create workspace salt");
            let second = load_or_create_workspace_salt(&state_path, &peer_key_path)
                .expect("reload workspace salt");

            assert_ne!(first, 0);
            assert_eq!(first, second);
            assert!(
                !peer_key_path.exists(),
                "allocating an object-id namespace must not mint a network identity"
            );
        }

        // Phase `existing_transport_identity_keeps_its_historical_workspace_salt`.
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let state_path = dir.path().join("config/state.json");
            let peer_key_path = dir.path().join("system/device-key");
            let key = crate::network::behaviour::load_or_create_keypair(&peer_key_path)
                .expect("create existing transport identity");
            let expected = crate::network::behaviour::peer_salt_from_keypair(&key).max(1);

            let actual = load_or_create_workspace_salt(&state_path, &peer_key_path)
                .expect("migrate historical salt");

            assert_eq!(actual, expected);
            assert_eq!(
                read_workspace_salt(
                    &state_path
                        .parent()
                        .expect("fixture state path has a parent")
                        .join("workspace-peer-salt"),
                )
                .expect("read persisted salt"),
                expected
            );
        }
    }

    #[test]
    fn presentation_and_lock_policy_waits_for_real_frames_and_fails_closed() {
        // Phase `output_presentation_removal_stays_pending_until_a_full_frame_submits`.
        {
            let mut pending = false;
            let mut previous_surface_count = 1;

            assert!(update_output_presentation_repaint_pending(
                &mut pending,
                &mut previous_surface_count,
                0,
                false,
            ));
            assert_eq!(previous_surface_count, 0);
            assert!(
                pending,
                "an off-cadence removal must survive until the next submitted frame"
            );

            pending = false;
            assert!(!update_output_presentation_repaint_pending(
                &mut pending,
                &mut previous_surface_count,
                0,
                false,
            ));
            assert!(update_output_presentation_repaint_pending(
                &mut pending,
                &mut previous_surface_count,
                0,
                true,
            ));
        }

        // Phase `portal_screencast_never_publishes_a_locked_frame`.
        {
            assert!(portal_screencast_should_publish(true, false));
            assert!(!portal_screencast_should_publish(true, true));
            assert!(!portal_screencast_should_publish(false, false));
        }

        // Phase `sleep_lock_ack_waits_for_its_exact_frame_and_fails_closed_on_presenter_loss`.
        {
            // A prior desktop frame has retired, but lock frame #42 has not: it
            // cannot release the sleep-delay inhibitor.  If the presenter dies
            // first, the only safe result is likewise no acknowledgement.
            assert_eq!(
                sleep_lock_frame_status(42, 41, false),
                SleepLockFrameStatus::Awaiting
            );
            assert_eq!(
                sleep_lock_frame_status(42, 42, false),
                SleepLockFrameStatus::Retired
            );
            assert_eq!(
                sleep_lock_frame_status(42, 41, true),
                SleepLockFrameStatus::PresenterLost
            );
        }

        // Phase `failed_suspend_only_unlocks_a_lock_it_prepared`.
        {
            assert!(should_unlock_after_suspend_failure(true, false));
            assert!(!should_unlock_after_suspend_failure(true, true));
            assert!(!should_unlock_after_suspend_failure(false, false));
        }
    }
}
