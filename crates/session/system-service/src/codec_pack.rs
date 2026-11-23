//! The `codec-pack` resource: what [`tilo_system_resources::codec_pack`] says it is.
//!
//! The codecs are the media system extension: `os_actions::codec_pack` reads its status and runs
//! the helper that installs or removes it. A change on its status file makes the service read
//! again, so the reading follows an install as it runs.

use tilo_actions::ResourceSpec;
use tilo_resource::Effect as _;
use tilo_service::offer;
use tilo_system_resources::codec_pack::{
    CodecPack, CodecPackAction, CodecState, HardwareVideo, CODEC_PACK,
};

use crate::i18n::name;

/// The `codec-pack` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.codec-pack", label = "Media codecs")]
pub struct CodecPackReading;

/// The `codec-pack` resource reading `codec_pack`.
#[must_use]
pub fn resource(_codecs: &CodecPack) -> ResourceSpec {
    offer::<CodecPackAction>(tilo_resource::typed_resource::<CodecPackReading>(
        CODEC_PACK,
        name("so.tilo.system.codec-pack", "Media codecs"),
    ))
}

/// The codecs as `os_actions` reads them now.
#[must_use]
pub fn reading() -> CodecPack {
    use os_actions::codec_pack::{CodecPackStatus, MesaState, PackState};
    let status = CodecPackStatus::read();
    CodecPack {
        available: os_actions::codec_pack::available(),
        state: match status.state {
            PackState::NotInstalled => CodecState::NotInstalled,
            PackState::WaitingForNetwork => CodecState::WaitingForNetwork,
            PackState::Installing => CodecState::Installing,
            PackState::Installed => CodecState::Installed,
            PackState::Removing => CodecState::Removing,
            PackState::Failed => CodecState::Failed,
        },
        hardware: match status.mesa {
            MesaState::Enabled => HardwareVideo::Enabled,
            MesaState::Waiting => HardwareVideo::Waiting,
            MesaState::Off => HardwareVideo::Off,
            MesaState::Unsupported => HardwareVideo::Unsupported,
        },
        summary: status.summary(),
        problem: status.refresh_problem(),
        offered: status
            .offered_actions()
            .into_iter()
            .map(|action| action.verb().to_owned())
            .collect(),
        ..CodecPack::default()
    }
}

/// Make `change` on the codecs.
///
/// # Errors
/// The helper refused or failed.
pub fn change(change: &crate::device::CodecChange) -> Result<(), String> {
    use crate::device::CodecChange;
    use os_actions::codec_pack::CodecAction;
    let action = match change {
        CodecChange::Install => CodecAction::Install,
        CodecChange::Remove => CodecAction::Remove,
        CodecChange::Refresh => CodecAction::Refresh,
    };
    os_actions::codec_pack::run(action)
}
