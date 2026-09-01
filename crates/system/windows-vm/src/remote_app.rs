use std::ffi::OsString;
use std::path::Path;

use thiserror::Error;

use crate::{BrokeredLaunch, CommandError, CommandRequest, CommandRunner, GuestLaunch};

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
            // qOS intentionally ships no XWayland. The native Wayland client
            // gives each RAIL application an ordinary foreign Wayland surface.
            binary: OsString::from("wlfreerdp"),
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
            .arg(format!("/app:program:{}", guest.remote_app_alias))
            .arg("/dynamic-resolution")
            .arg("/network:auto")
            .arg("/gfx:AVC444")
            .arg("/rfx")
            .arg("/audio-mode:0")
            .stdin_secret(&stdin);

        if brokered.clipboard {
            request = request.arg("+clipboard");
        } else {
            request = request.arg("-clipboard");
        }
        if brokered.microphone {
            request = request.arg("/microphone:sys:pulse");
        }
        for share in &brokered.shares {
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
                username: "qos-remoteapp".to_string(),
                certificate_sha256: "a".repeat(64),
            },
            password: Secret::new(b"one-time-password".to_vec()),
            remote_app_alias: "qos-word".to_string(),
            launch_nonce: "nonce".to_string(),
        }
    }

    #[test]
    fn defaults_do_not_redirect_host_data() {
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

    #[test]
    fn only_brokered_capabilities_become_redirectors() {
        let launcher = FreeRdpLauncher::new(Runner::default());
        let brokered = BrokeredLaunch {
            lease_id: "lease".to_string(),
            clipboard: true,
            microphone: true,
            shares: vec![ShareBinding {
                drive_name: "QOS1".to_string(),
                portal_document_id: "document-42".to_string(),
                host_path: "/run/qos/windows-vm/leases/lease/document-42".into(),
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
        assert!(args.iter().any(|arg| arg == "+clipboard"));
        assert!(args.iter().any(|arg| arg.starts_with("/drive:QOS1,")));
        assert!(args.iter().any(|arg| arg.starts_with("/microphone:")));
    }
}
