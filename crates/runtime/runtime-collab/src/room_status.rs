//! Where a room stands for the apps on this device.

/// Where a room stands for the apps on this device: whether an app can open
/// documents in it now, and if not, why.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RoomStatus {
    /// Apps open documents in it.
    #[default]
    Ready,
    /// Not yet: why, in the person's words, and what last held it up when
    /// something did. A join made meanwhile is held until it is ready.
    Waiting {
        reason: String,
        detail: Option<String>,
    },
    /// It will not be ready on this device, and why.
    Refused(String),
}
