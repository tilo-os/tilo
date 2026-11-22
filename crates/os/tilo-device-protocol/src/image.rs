//! The device's OS image and power: what it boots (part of `status`), moving it onto another image
//! (`upgrade`), and the `reboot` and `poweroff` steps.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where an OCI layout sent for an `upgrade` is laid out on the device.
pub const LAYOUT_DIR: &str = "/var/lib/tilo-vm/image";

/// The tag the layout's image is named by in its `index.json`.
pub const LAYOUT_TAG: &str = "tilo-vm";

/// One bootc deployment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deployment {
    pub image: String,
    pub transport: String,
    pub digest: String,
    /// The composefs identity it boots (`composefs.verity`, without the `sha512:` prefix), which
    /// is how its boot loader entry names it.
    #[serde(default)]
    pub verity: Option<String>,
}

impl Deployment {
    /// `ref (sha256:0123456789ab)`.
    pub fn describe(&self) -> String {
        let reference = if self.transport == "registry" {
            self.image.clone()
        } else {
            format!("{}:{}", self.transport, self.image)
        };
        format!("{reference} ({})", short_digest(&self.digest))
    }
}

/// `sha256:` and the first twelve hex digits.
pub fn short_digest(digest: &str) -> &str {
    digest.get(..19).unwrap_or(digest)
}

/// What bootc says the device boots, has staged and keeps to roll back to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootcStatus {
    /// The image and transport it follows.
    #[serde(default)]
    pub spec: Option<(String, String)>,
    #[serde(default)]
    pub booted: Option<Deployment>,
    #[serde(default)]
    pub staged: Option<Deployment>,
    #[serde(default)]
    pub rollback: Option<Deployment>,
}

/// The device's boot, as `status` reports it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BootFacts {
    /// `/proc/sys/kernel/random/boot_id`: a new value is a new boot.
    pub boot_id: String,
    /// The dev image's `base.json`, when it is one.
    #[serde(default)]
    pub base: Option<Value>,
    /// Free bytes on `/var`.
    #[serde(default)]
    pub free_bytes: Option<u64>,
    /// The board the image was built for, on a board dev image.
    #[serde(default)]
    pub board: Option<String>,
    /// `None` on an image without bootc, which `upgrade` cannot move.
    #[serde(default)]
    pub bootc: Option<BootcStatus>,
    /// The image the kernel command line says this boot runs.
    #[serde(default)]
    pub booted_image: Option<String>,
}

impl BootFacts {
    /// The CPU architecture the image was built for.
    pub fn arch(&self) -> Option<String> {
        let target = self.base.as_ref()?.get("target")?.as_str()?;
        target
            .split('-')
            .next()
            .filter(|arch| !arch.is_empty())
            .map(str::to_owned)
    }
}

/// What an `upgrade` stages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageSource {
    /// An OCI layout in the device's content store, as a tree manifest (blake3 hex) whose files
    /// are blobs there too; it is laid out at [`LAYOUT_DIR`] and staged from there.
    Layout { manifest: String },
    /// A registry image the device pulls itself, through its update channel when it has one;
    /// `None` follows the image it already follows.
    Registry {
        #[serde(default)]
        image: Option<String>,
    },
    /// The deployment bootc keeps to roll back to.
    Rollback,
}

/// Stage another OS image under the VM lock and give it its boot loader try counter. Nothing
/// reboots: `reboot` does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upgrade {
    pub source: ImageSource,
    /// The manifest digest the staged deployment must have.
    #[serde(default)]
    pub expected: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum UpgradeResult {
    /// bootc refused: the device already boots this image.
    AlreadyBooted,
    /// Nothing new was staged; the device boots the newest image there is.
    Current {
        #[serde(default)]
        booted: Option<Deployment>,
    },
    /// `next` boots at the next restart.
    Staged {
        #[serde(default)]
        previous: Option<Deployment>,
        next: Deployment,
        /// What the person should know about the boot that follows.
        #[serde(default)]
        warnings: Vec<String>,
    },
}

/// Restart the device now, leaving the ESP writable for the deployment bootc applies at shutdown.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reboot {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebootResult {
    /// The boot being left: the next boot answers with another.
    pub boot_id: String,
}

/// Power the device off cleanly, once no swap, gc or upgrade holds the VM lock.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Poweroff {}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoweroffResult {}
