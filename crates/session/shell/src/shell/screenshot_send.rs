//! "Send…" for a saved screenshot: to one of your devices, or into the
//! active workspace's files.
//!
//! A device send is an ordinary Nearby Send offer over the device fabric —
//! the same secure staging and resumable transfer Files uses, and the
//! receiving device still asks its person where to save it. Saving to the
//! workspace copies the file into its shared files, where collaborators see
//! it like anything else placed there.

use std::path::{Path, PathBuf};

use ui::widget::{ScreenshotSendDevice, ScreenshotSendTarget};

use crate::shell::i18n::{t, t_args};

/// Folder inside the workspace files that sent screenshots land in.
pub const WORKSPACE_SCREENSHOTS_DIR: &str = "Screenshots";

/// What a successful send did, for the confirmation the person sees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendReceipt {
    /// Offered to the device; delivery completes when its person accepts.
    Offered { device: String },
    /// Copied into the workspace files at this path.
    Saved(PathBuf),
}

impl SendReceipt {
    pub fn message(&self) -> String {
        match self {
            Self::Offered { device } => t_args(
                "screenshot-send-offered",
                &[("device", device.as_str().into())],
            ),
            Self::Saved(path) => t_args(
                "screenshot-send-saved",
                &[(
                    "name",
                    path.file_name()
                        .map_or_else(
                            || path.display().to_string(),
                            |name| name.to_string_lossy().into_owned(),
                        )
                        .into(),
                )],
            ),
        }
    }
}

/// The transports a screenshot can leave by. Production goes over the
/// device fabric and the workspace mount; tests record what was asked.
pub trait ScreenshotSendBridge {
    /// Your other devices that are online now.
    fn devices(&self) -> Vec<ScreenshotSendDevice>;
    /// Offer `path` to device `id` through Nearby Send.
    ///
    /// # Errors
    /// Why the fabric refused (offline, policy, unreadable source).
    fn offer_to_device(&mut self, id: &str, path: &Path) -> Result<(), String>;
    /// Copy `path` into the workspace files; returns where it landed.
    ///
    /// # Errors
    /// Why the copy failed.
    fn save_to_workspace(&mut self, path: &Path) -> Result<PathBuf, String>;
}

/// Deliver the screenshot at `path` to `target`. A device must still be
/// online and yours: the id is re-resolved here, never trusted from the
/// picker that offered it.
///
/// # Errors
/// A message for the person when the device is gone or the transport failed.
pub fn route(
    path: &Path,
    target: &ScreenshotSendTarget,
    bridge: &mut dyn ScreenshotSendBridge,
) -> Result<SendReceipt, String> {
    if !path.is_file() {
        return Err(t_args(
            "screenshot-send-gone",
            &[("path", path.display().to_string().into())],
        ));
    }
    match target {
        ScreenshotSendTarget::Workspace => bridge.save_to_workspace(path).map(SendReceipt::Saved),
        ScreenshotSendTarget::Device(id) => {
            let device = bridge
                .devices()
                .into_iter()
                .find(|device| &device.id == id)
                .ok_or_else(|| t("screenshot-send-device-offline"))?;
            bridge.offer_to_device(id, path)?;
            Ok(SendReceipt::Offered {
                device: device.label,
            })
        }
    }
}

/// Copy `source` into `root/Screenshots`, never replacing an existing file.
///
/// # Errors
/// The I/O error, phrased for the person.
pub fn copy_into_workspace(source: &Path, root: &Path) -> Result<PathBuf, String> {
    let folder = root.join(WORKSPACE_SCREENSHOTS_DIR);
    std::fs::create_dir_all(&folder).map_err(|error| {
        t_args(
            "screenshot-send-create-failed",
            &[
                ("path", folder.display().to_string().into()),
                ("error", error.to_string().into()),
            ],
        )
    })?;
    let name = source.file_name().ok_or_else(|| {
        t_args(
            "screenshot-send-no-file-name",
            &[("path", source.display().to_string().into())],
        )
    })?;
    let stem = Path::new(name)
        .file_stem()
        .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned());
    let ext = Path::new(name)
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned());
    for attempt in 0..1000_u32 {
        let candidate = if attempt == 0 {
            folder.join(name)
        } else {
            let numbered = format!("{stem} ({attempt})");
            folder.join(match &ext {
                Some(ext) => format!("{numbered}.{ext}"),
                None => numbered,
            })
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut target) => {
                let mut input = std::fs::File::open(source).map_err(|error| {
                    t_args(
                        "screenshot-send-read-failed",
                        &[
                            ("path", source.display().to_string().into()),
                            ("error", error.to_string().into()),
                        ],
                    )
                })?;
                std::io::copy(&mut input, &mut target).map_err(|error| {
                    let _ = std::fs::remove_file(&candidate);
                    t_args(
                        "screenshot-send-save-failed",
                        &[("error", error.to_string().into())],
                    )
                })?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(t_args(
                    "screenshot-send-save-failed",
                    &[("error", error.to_string().into())],
                ));
            }
        }
    }
    Err(t("screenshot-send-too-many"))
}

/// The production bridge: the session's device fabric and workspace mount.
pub struct FabricSendBridge<'a> {
    pub network: Option<&'a crate::shell::network::ShellNetwork>,
    pub workspace_root: PathBuf,
}

impl ScreenshotSendBridge for FabricSendBridge<'_> {
    fn devices(&self) -> Vec<ScreenshotSendDevice> {
        self.network.map_or_else(Vec::new, online_devices)
    }

    fn offer_to_device(&mut self, id: &str, path: &Path) -> Result<(), String> {
        let network = self.network.ok_or_else(|| t("screenshot-send-sync-off"))?;
        let device = crate::shell::personal_continuity::DeviceId::new(id)
            .map_err(|_| t("screenshot-send-device-offline"))?;
        let folder = path.parent().ok_or_else(|| {
            t_args(
                "screenshot-send-no-folder",
                &[("path", path.display().to_string().into())],
            )
        })?;
        let source = crate::shell::nearby_transfer::SecureNearbySource::open_beneath(folder, path)?;
        crate::shell::device_fabric_runtime::request_nearby_drop_secure(
            network,
            &device,
            source,
            "capture:screenshot",
        )
        .map(|_| ())
    }

    fn save_to_workspace(&mut self, path: &Path) -> Result<PathBuf, String> {
        copy_into_workspace(path, &self.workspace_root)
    }
}

/// "Save to this workspace's files" alone, for the capture worker: the copy
/// writes to the workspace mount, which may be remote, so it never runs on
/// the compositor loop. It reaches no device.
pub struct WorkspaceFilesBridge {
    pub workspace_root: PathBuf,
}

impl ScreenshotSendBridge for WorkspaceFilesBridge {
    fn devices(&self) -> Vec<ScreenshotSendDevice> {
        Vec::new()
    }

    fn offer_to_device(&mut self, _id: &str, _path: &Path) -> Result<(), String> {
        Err(t("screenshot-send-device-offline"))
    }

    fn save_to_workspace(&mut self, path: &Path) -> Result<PathBuf, String> {
        copy_into_workspace(path, &self.workspace_root)
    }
}

/// Your other devices that are online, as "Send…" lists them.
pub fn online_devices(network: &crate::shell::network::ShellNetwork) -> Vec<ScreenshotSendDevice> {
    let local = network
        .local_device_fabric_identity()
        .map(|(device, _)| device);
    let mut devices: Vec<ScreenshotSendDevice> = network
        .device_fabric()
        .snapshots()
        .values()
        .filter(|snapshot| snapshot.presence.online && local.as_ref() != Some(&snapshot.owner))
        .map(|snapshot| ScreenshotSendDevice {
            id: snapshot.owner.as_str().to_string(),
            label: snapshot.presence.label.clone(),
        })
        .collect();
    devices.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));
    devices
}

/// Where "Save to this workspace's files" writes: the projected files of
/// the active workspace.
pub fn workspace_files_root() -> PathBuf {
    PathBuf::from(crate::portal::doc_store::WORKSPACE_ROOT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records every transport call instead of touching the fabric.
    struct RecordingBridge {
        devices: Vec<ScreenshotSendDevice>,
        workspace: PathBuf,
        calls: Vec<String>,
        refuse: Option<String>,
    }

    impl ScreenshotSendBridge for RecordingBridge {
        fn devices(&self) -> Vec<ScreenshotSendDevice> {
            self.devices.clone()
        }
        fn offer_to_device(&mut self, id: &str, path: &Path) -> Result<(), String> {
            self.calls.push(format!(
                "offer {id} {}",
                path.file_name().unwrap().to_string_lossy()
            ));
            self.refuse.clone().map_or(Ok(()), Err)
        }
        fn save_to_workspace(&mut self, path: &Path) -> Result<PathBuf, String> {
            self.calls.push(format!(
                "save {}",
                path.file_name().unwrap().to_string_lossy()
            ));
            copy_into_workspace(path, &self.workspace)
        }
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, RecordingBridge) {
        let dir = tempfile::tempdir().unwrap();
        let shot = dir.path().join("Screenshot 1.png");
        std::fs::write(&shot, b"png").unwrap();
        let bridge = RecordingBridge {
            devices: vec![ScreenshotSendDevice {
                id: "device-phone".into(),
                label: "Ada's phone".into(),
            }],
            workspace: dir.path().join("workspace"),
            calls: Vec::new(),
            refuse: None,
        };
        (dir, shot, bridge)
    }

    #[test]
    fn a_device_target_is_offered_over_the_fabric() {
        let (_dir, shot, mut bridge) = fixture();
        let receipt = route(
            &shot,
            &ScreenshotSendTarget::Device("device-phone".into()),
            &mut bridge,
        )
        .unwrap();
        assert_eq!(bridge.calls, vec!["offer device-phone Screenshot 1.png"]);
        assert_eq!(
            receipt,
            SendReceipt::Offered {
                device: "Ada's phone".into()
            }
        );
        assert!(receipt.message().contains("accept it there"));
    }

    #[test]
    fn a_device_that_went_offline_is_refused_without_sending() {
        let (_dir, shot, mut bridge) = fixture();
        let error = route(
            &shot,
            &ScreenshotSendTarget::Device("device-gone".into()),
            &mut bridge,
        )
        .unwrap_err();
        assert!(error.contains("no longer online"), "{error}");
        assert!(bridge.calls.is_empty());
    }

    #[test]
    fn a_fabric_refusal_reaches_the_person() {
        let (_dir, shot, mut bridge) = fixture();
        bridge.refuse = Some("file transfer is off on this device".into());
        let error = route(
            &shot,
            &ScreenshotSendTarget::Device("device-phone".into()),
            &mut bridge,
        )
        .unwrap_err();
        assert_eq!(error, "file transfer is off on this device");
    }

    #[test]
    fn the_workspace_target_copies_into_screenshots_without_overwriting() {
        let (_dir, shot, mut bridge) = fixture();
        let first = route(&shot, &ScreenshotSendTarget::Workspace, &mut bridge).unwrap();
        let second = route(&shot, &ScreenshotSendTarget::Workspace, &mut bridge).unwrap();
        let SendReceipt::Saved(first) = first else {
            panic!("saved");
        };
        let SendReceipt::Saved(second) = second else {
            panic!("saved");
        };
        let folder = bridge.workspace.join(WORKSPACE_SCREENSHOTS_DIR);
        assert_eq!(first, folder.join("Screenshot 1.png"));
        assert_eq!(second, folder.join("Screenshot 1 (1).png"));
        assert_eq!(std::fs::read(&second).unwrap(), b"png");
        assert!(shot.exists(), "the original stays where it was saved");
        assert_eq!(
            bridge.calls,
            vec!["save Screenshot 1.png", "save Screenshot 1.png"]
        );
    }

    #[test]
    fn a_missing_screenshot_is_refused() {
        let (dir, _shot, mut bridge) = fixture();
        let error = route(
            &dir.path().join("gone.png"),
            &ScreenshotSendTarget::Workspace,
            &mut bridge,
        )
        .unwrap_err();
        assert!(error.contains("gone"));
        assert!(bridge.calls.is_empty());
    }
}
