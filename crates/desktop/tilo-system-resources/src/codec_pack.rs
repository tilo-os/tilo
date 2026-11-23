//! The `codec-pack` resource: the media codecs extension (H.264 and HEVC playback and recording),
//! whether it is installed and what it is doing, with the actions that install, remove and check
//! it again.
//!
//! The extension is a signed system extension, installed by a privileged helper through polkit;
//! this contract is the vocabulary a shell, Settings and an agent speak to it.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const CODEC_PACK: &str = "codec-pack";

/// What the extension is doing.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum CodecState {
    /// Not installed, and not asked for.
    #[default]
    NotInstalled,
    /// Asked for while offline: it installs by itself once this device is online.
    WaitingForNetwork,
    Installing,
    Installed,
    Removing,
    Failed,
}

/// How the hardware part (the matching Mesa VA-API driver) stands.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum HardwareVideo {
    /// On, and hardware H.264/HEVC works with it.
    #[default]
    Enabled,
    /// On; the hardware part follows once a matching driver is published.
    Waiting,
    /// Not applicable to this machine's graphics.
    Off,
    Unsupported,
}

/// What this computer's media codecs read as now.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct CodecPack {
    /// Whether this system offers the codecs at all.
    #[serde(default)]
    pub available: bool,
    pub state: CodecState,
    pub hardware: HardwareVideo,
    /// What it is doing, in words.
    pub summary: String,
    /// Why the last attempt failed, when it did.
    #[serde(default)]
    pub problem: Option<String>,
    /// The actions that make sense now, by id.
    pub offered: Vec<String>,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
}

/// The `codec-pack` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.codec-pack")]
pub enum CodecPackAction {
    /// Install the codecs, or try again after a failure.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "download",
        keywords = "install codecs, mediencodecs installieren"
    )]
    Install,
    /// Remove the codecs.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "remove codecs, mediencodecs entfernen"
    )]
    Remove,
    /// Check whether the hardware part is available again.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "rotate-cw",
        keywords = "check codecs, mediencodecs prüfen"
    )]
    Refresh,
}
