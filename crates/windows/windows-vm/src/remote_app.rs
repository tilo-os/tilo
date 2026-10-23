use std::ffi::OsString;
use std::path::Path;

use thiserror::Error;

use crate::{
    ApplicationPresentation, BrokeredLaunch, CommandError, CommandRequest, CommandRunner,
    GuestLaunch,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteAppProcess {
    /// The launcher-specific exit code. A successful RDP connection can still
    /// finish with a non-zero code when the guest closes unexpectedly.
    pub exit_code: Option<i32>,
}

pub trait RemoteAppLauncher: Send + Sync {
    fn launch(
        &self,
        guest: &GuestLaunch,
        brokered: &BrokeredLaunch,
    ) -> Result<RemoteAppProcess, RemoteAppError>;
}

#[derive(Clone, Debug)]
pub struct FreeRdpLauncher<R> {
    runner: R,
    binary: OsString,
}

impl<R> FreeRdpLauncher<R> {
    #[must_use]
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            // FreeRDP's upstream RAIL client is xfreerdp.  It is only used
            // when Tilo has explicitly provisioned a RAIL-capable local
            // surface; there is no upstream `wlfreerdp` binary to assume.
            binary: OsString::from("xfreerdp"),
        }
    }

    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<OsString>) -> Self {
        self.binary = binary.into();
        self
    }

    pub fn command(
        &self,
        guest: &GuestLaunch,
        brokered: &BrokeredLaunch,
    ) -> Result<CommandRequest, RemoteAppError> {
        if guest.presentation != ApplicationPresentation::NativeRail {
            return Err(RemoteAppError::PresentationMismatch);
        }
        validate_alias(&guest.remote_app_alias)?;
        validate_username(&guest.endpoint.username)?;
        validate_fingerprint(&guest.endpoint.certificate_sha256)?;
        brokered.validate()?;

        // FreeRDP's `/from-stdin` avoids exposing the one-time password in argv
        // or `/proc`. The guest service invalidates it after this connection.
        let mut stdin = Vec::new();
        stdin.extend_from_slice(guest.password.expose());
        stdin.push(b'\n');
        let mut request = CommandRequest::new(self.binary.clone())
            .arg(format!("/v:{}", guest.endpoint.address))
            .arg(format!("/u:{}", guest.endpoint.username))
            .arg("/from-stdin")
            .arg(format!(
                "/cert:fingerprint:sha256:{}",
                guest.endpoint.certificate_sha256
            ))
            // FreeRDP and Microsoft's .rdp format distinguish an allow-list
            // alias from an executable path with the leading `||`. Omitting
            // it makes client-Windows RAIL especially prone to failing with
            // no application window even though authentication succeeded.
            .arg(format!("/app:program:||{}", guest.remote_app_alias))
            // A brief compositor/display interruption should reconnect the
            // existing RDP session, but an invalid guest or credential must
            // not create an unbounded retry loop.
            .arg("+auto-reconnect")
            .arg("/auto-reconnect-max-retries:3")
            // This is always a host-only, single-hop connection. Advertising
            // LAN characteristics avoids FreeRDP's WAN-oriented visual and
            // latency trade-offs.
            .arg("/network:lan")
            .arg("/gfx:AVC444")
            .arg("/compression-level:2")
            // Desktop sizing and appearance flags (`+dynamic-resolution`,
            // window-drag, wallpaper, themes, aero) do not describe RAIL
            // windows and have caused resize faults in recent FreeRDP/X11
            // combinations. RAIL carries each remote window's own geometry.
            .arg("-grab-keyboard")
            .arg(format!("/wm-class:tilo-windows-{}", guest.remote_app_alias))
            // Playback is output-only; the Pulse endpoint may itself be a
            // short-lived Tilo proxy. Capture remains a separate redirector.
            .arg("/sound:sys:pulse,latency:50,quality:high")
            // RAIL is an application surface, not a hidden full-desktop
            // session. Audio output is safe by default; input is not.
            .arg("/audio-mode:0")
            // Make every broad FreeRDP redirector opt-in below. In particular
            // never inherit FreeRDP distribution defaults for host drives.
            .arg("-drives")
            .arg("-home-drive")
            .stdin_secret(&stdin);

        if brokered.clipboard {
            // Text/image clipboard follows the canonical clipboard grant.
            // File clipboard stays off.  A portal-selected document is made
            // available only through its brokered RDP drive below; Windows
            // can copy or drag it from that drive into the published app.
            // Letting FreeRDP inspect a compositor/X11 file offer here would
            // reintroduce an ambient client path outside that projection.
            request = request.arg("/clipboard:direction-to:all,files-to:off");
        } else {
            request = request.arg("-clipboard");
        }
        if brokered.microphone {
            request = request.arg("/microphone:sys:pulse");
        }
        for share in &brokered.shares {
            // `host_path` is a broker-created, per-lease directory, never a
            // workspace path.  Read-only enforcement belongs to that mount,
            // not to an advisory FreeRDP switch (RDPDR has no trustworthy
            // read-only command-line mode).
            if share.read_only {
                // Consumed by `/usr/libexec/tilo-windows-rail`, never passed
                // to FreeRDP. The helper overlays this broker-owned directory
                // read-only in its rootless mount namespace.
                request = request.arg(format!(
                    "--tilo-read-only-drive={}",
                    share.host_path.display()
                ));
            }
            request = request.arg(format!(
                "/drive:{},{}",
                share.drive_name,
                share.host_path.display()
            ));
        }
        Ok(request)
    }
}

impl<R: CommandRunner> RemoteAppLauncher for FreeRdpLauncher<R> {
    fn launch(
        &self,
        guest: &GuestLaunch,
        brokered: &BrokeredLaunch,
    ) -> Result<RemoteAppProcess, RemoteAppError> {
        let request = self.command(guest, brokered)?;
        let output = self.runner.run(&request)?;
        if !output.success {
            return Err(RemoteAppError::FreeRdp {
                code: output.code,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(RemoteAppProcess {
            exit_code: output.code,
        })
    }
}

/// Upstream virt-viewer fallback for hosts without a local RAIL
/// surface. The guest bridge is responsible for focusing the requested
/// application before this command is issued. This is a whole guest display,
/// not a seamless window; the Tilo UI must label that distinction. No
/// application path or guest shell text crosses this boundary; the viewer
/// attaches only to Tilo's fixed singleton domain.
#[derive(Clone, Debug)]
pub struct AppFocusedSpiceLauncher<R> {
    runner: R,
    binary: OsString,
}

impl<R> AppFocusedSpiceLauncher<R> {
    #[must_use]
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            binary: OsString::from("virt-viewer"),
        }
    }

    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<OsString>) -> Self {
        self.binary = binary.into();
        self
    }

    pub fn command(
        &self,
        guest: &GuestLaunch,
        brokered: &BrokeredLaunch,
    ) -> Result<CommandRequest, RemoteAppError> {
        if guest.presentation != ApplicationPresentation::AppFocusedSpice {
            return Err(RemoteAppError::PresentationMismatch);
        }
        validate_alias(&guest.remote_app_alias)?;
        brokered.validate()?;
        // SPICE is deliberately attached through libvirt rather than a guest
        // TCP listener.  It has no implicit clipboard, drive, microphone, or
        // network redirection.  A caller that needs one of those capabilities
        // must use the capability-specific RAIL adapter instead.
        if brokered.clipboard
            || brokered.microphone
            || brokered.network
            || !brokered.shares.is_empty()
        {
            return Err(RemoteAppError::SpiceCapabilityRequiresRail);
        }
        Ok(CommandRequest::new(self.binary.clone())
            .args([
                "--connect",
                "qemu:///system",
                "--attach",
                "--wait",
                "--reconnect",
                "--auto-resize=always",
                "--zoom=100",
                "--cursor=local",
            ])
            .arg(format!("--title=Tilo — {}", guest.remote_app_alias))
            .arg(crate::APPLIANCE_DOMAIN_NAME))
    }
}

impl<R: CommandRunner> RemoteAppLauncher for AppFocusedSpiceLauncher<R> {
    fn launch(
        &self,
        guest: &GuestLaunch,
        brokered: &BrokeredLaunch,
    ) -> Result<RemoteAppProcess, RemoteAppError> {
        let request = self.command(guest, brokered)?;
        let output = self.runner.run(&request)?;
        if !output.success {
            return Err(RemoteAppError::Spice {
                code: output.code,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(RemoteAppProcess {
            exit_code: output.code,
        })
    }
}

fn validate_alias(value: &str) -> Result<(), RemoteAppError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(RemoteAppError::InvalidAlias);
    }
    Ok(())
}

fn validate_username(value: &str) -> Result<(), RemoteAppError> {
    if value.is_empty()
        || value.len() > 128
        || value.chars().any(char::is_control)
        || value.starts_with('/')
    {
        return Err(RemoteAppError::InvalidUsername);
    }
    Ok(())
}

fn validate_fingerprint(value: &str) -> Result<(), RemoteAppError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RemoteAppError::InvalidFingerprint);
    }
    Ok(())
}

pub(crate) fn validate_share_path(path: &Path) -> Result<(), RemoteAppError> {
    let value = path.to_str().ok_or(RemoteAppError::InvalidSharePath)?;
    if !path.is_absolute() || value.contains(',') || value.contains('\n') || value.contains('\r') {
        return Err(RemoteAppError::InvalidSharePath);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum RemoteAppError {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error("FreeRDP exited with code {code:?}: {stderr}")]
    FreeRdp { code: Option<i32>, stderr: String },
    #[error("SPICE viewer exited with code {code:?}: {stderr}")]
    Spice { code: Option<i32>, stderr: String },
    #[error("SPICE fallback cannot carry Tilo capability redirects; use a RAIL surface")]
    SpiceCapabilityRequiresRail,
    #[error("guest selected a different presentation backend than this launcher")]
    PresentationMismatch,
    #[error("guest returned an invalid RemoteApp alias")]
    InvalidAlias,
    #[error("guest returned an invalid RDP username")]
    InvalidUsername,
    #[error("guest returned an invalid RDP certificate fingerprint")]
    InvalidFingerprint,
    #[error("capability broker returned an invalid RDP drive name")]
    InvalidDriveName,
    #[error("capability broker returned an invalid RDP share path")]
    InvalidSharePath,
    #[error("capability broker returned a duplicate RDP drive name")]
    DuplicateDriveName,
    #[error("capability broker returned an empty lease identifier")]
    InvalidLease,
    #[error("capability broker returned an invalid or duplicate portal document")]
    InvalidPortalDocument,
    #[error("capability broker returned an incomplete network lease")]
    InvalidNetworkLease,
    #[error("capability broker returned a share outside its private lease mount")]
    ShareOutsideLease,
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Mutex;

    use crate::{CommandOutput, RdpEndpoint, Secret, ShareBinding};

    use super::*;

    #[derive(Default)]
    struct Runner {
        request: Mutex<Option<CommandRequest>>,
    }

    impl CommandRunner for Runner {
        fn run(&self, request: &CommandRequest) -> Result<CommandOutput, CommandError> {
            *self.request.lock().unwrap() = Some(request.clone());
            Ok(CommandOutput::successful(""))
        }
    }

    fn guest() -> GuestLaunch {
        GuestLaunch {
            endpoint: RdpEndpoint {
                address: (IpAddr::V4(Ipv4Addr::LOCALHOST), 3389).into(),
                username: "tilo-remoteapp".to_string(),
                certificate_sha256: "a".repeat(64),
            },
            password: Secret::new(b"one-time-password".to_vec()),
            remote_app_alias: "tilo-word".to_string(),
            launch_nonce: "nonce".to_string(),
            presentation: ApplicationPresentation::NativeRail,
        }
    }

    #[test]
    fn remote_app_redirects_only_brokered_capabilities() {
        {
            let launcher = FreeRdpLauncher::new(Runner::default());
            let request = launcher
                .command(&guest(), &BrokeredLaunch::deny_all("lease"))
                .unwrap();
            let args = request
                .args
                .iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>();
            assert!(args.iter().any(|arg| arg == "-clipboard"));
            assert!(!args.iter().any(|arg| arg.starts_with("/drive:")));
            assert!(!args.iter().any(|arg| arg.starts_with("/microphone:")));
            assert!(!args.iter().any(|arg| arg.contains("one-time-password")));
            assert!(String::from_utf8_lossy(&request.stdin).contains("one-time-password"));
        }
        {
            let launcher = FreeRdpLauncher::new(Runner::default());
            let brokered = BrokeredLaunch {
                lease_id: "lease".to_string(),
                network_binding: None,
                clipboard: true,
                microphone: true,
                shares: vec![ShareBinding {
                    drive_name: "TILO1".to_string(),
                    portal_document_id: "document-42".to_string(),
                    host_path: crate::portal_lease_root().join("lease/document-42"),
                    read_only: true,
                }],
                network: false,
            };
            let request = launcher.command(&guest(), &brokered).unwrap();
            let args = request
                .args
                .iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>();
            assert!(
                args.iter()
                    .any(|arg| arg == "/clipboard:direction-to:all,files-to:off")
            );
            assert!(args.iter().any(|arg| arg.starts_with("/drive:TILO1,")));
            assert!(args.iter().any(|arg| arg.starts_with("/microphone:")));
            assert!(args.iter().any(|arg| arg.starts_with("/sound:sys:pulse")));
            assert!(args.iter().any(|arg| arg == "/network:lan"));
        }
        {
            let launcher = FreeRdpLauncher::new(Runner::default());
            let request = launcher
                .command(&guest(), &BrokeredLaunch::deny_all("lease"))
                .unwrap();
            assert_eq!(request.program, OsString::from("xfreerdp"));
            assert!(
                request
                    .args
                    .iter()
                    .any(|arg| arg == "/app:program:||tilo-word")
            );
            assert!(request.args.iter().any(|arg| arg == "+auto-reconnect"));
            assert!(
                request
                    .args
                    .iter()
                    .any(|arg| arg == "/auto-reconnect-max-retries:3")
            );
            assert!(!request.args.iter().any(|arg| arg == "/dynamic-resolution"));
        }
    }

    #[test]
    fn spice_fallback_is_singleton_and_never_redirects_silently() {
        {
            let launcher = AppFocusedSpiceLauncher::new(Runner::default());
            let mut guest = guest();
            guest.presentation = ApplicationPresentation::AppFocusedSpice;
            let request = launcher
                .command(&guest, &BrokeredLaunch::deny_all("lease"))
                .unwrap();
            assert_eq!(request.program, OsString::from("virt-viewer"));
            let args = request
                .args
                .iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>();
            assert!(args.iter().any(|arg| arg == "--attach"));
            assert!(args.iter().any(|arg| arg == "--auto-resize=always"));
            assert!(args.iter().any(|arg| arg == "--cursor=local"));
            assert!(args.iter().any(|arg| arg == crate::APPLIANCE_DOMAIN_NAME));
            assert!(!args.iter().any(|arg| arg.contains("/app")));
        }
        {
            let launcher = AppFocusedSpiceLauncher::new(Runner::default());
            let mut guest = guest();
            guest.presentation = ApplicationPresentation::AppFocusedSpice;
            let brokered = BrokeredLaunch {
                lease_id: "lease".to_string(),
                network_binding: None,
                network: false,
                clipboard: false,
                microphone: false,
                shares: vec![ShareBinding {
                    drive_name: "TILO1".to_string(),
                    portal_document_id: "document-42".to_string(),
                    host_path: crate::portal_lease_root().join("lease/document-42"),
                    read_only: true,
                }],
            };
            assert!(matches!(
                launcher.command(&guest, &brokered),
                Err(RemoteAppError::SpiceCapabilityRequiresRail)
            ));
        }
    }
}
