//! The studio's words: what its receipt says of a screenshot on its way to one
//! of the person's other devices, in the desktop's language. The source
//! catalogue is embedded; other languages come from the language packs, and a
//! message missing from one falls back to `en-US`.

use std::path::Path;

use crate::link::{Refusal, Sent};

ui::embedded_catalog!(pub(crate) fn, ["en-US", "de-DE"]);

/// What the studio says while a screenshot is on its way to `device`.
pub(crate) fn sending(device: &str) -> String {
    t_args("screenshot-send-sending", &[("device", device.into())])
}

/// What became of the send: offered, or why it was not.
pub(crate) fn sent(sent: &Sent) -> String {
    match &sent.refused {
        None => t_args(
            "screenshot-send-offered",
            &[("device", sent.device.as_str().into())],
        ),
        Some(why) => refusal(why),
    }
}

/// Why a screenshot did not go to its device.
fn refusal(refusal: &Refusal) -> String {
    match refusal {
        Refusal::SyncOff => t("screenshot-send-sync-off"),
        Refusal::Offline => t("screenshot-send-offline"),
        Refusal::Gone { path } => gone(path),
        Refusal::NotKept => t("screenshot-send-not-kept"),
        Refusal::NoFolder { path } => no_folder(path),
        Refusal::TooLarge { bytes, limit } => t_args(
            "screenshot-send-too-large",
            &[
                ("limit", ui::format::size(*limit).into()),
                ("size", ui::format::size(*bytes).into()),
            ],
        ),
        Refusal::Refused(reason) => t_args(
            "screenshot-send-refused",
            &[("reason", reason.as_str().into())],
        ),
    }
}

/// The screenshot `path` names is no longer there.
pub(crate) fn gone(path: &Path) -> String {
    t_args("screenshot-send-gone", &[("path", named(path).into())])
}

/// `path` has no folder to be read from.
pub(crate) fn no_folder(path: &Path) -> String {
    t_args("screenshot-send-no-folder", &[("path", named(path).into())])
}

fn named(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_send_says_what_became_of_it() {
        assert_eq!(
            sent(&Sent::offered("Phone".into())),
            "Offered to Phone; accept it there to save"
        );
        assert_eq!(
            sent(&Sent::refused("Phone".into(), Refusal::Offline)),
            "That device is no longer online"
        );
        assert_eq!(
            sent(&Sent::refused(
                "Phone".into(),
                Refusal::Gone {
                    path: "/home/me/Screenshots/one.png".into()
                }
            )),
            "The screenshot /home/me/Screenshots/one.png is gone"
        );
        assert_eq!(
            sent(&Sent::refused(
                "Phone".into(),
                Refusal::TooLarge {
                    bytes: 70 * 1024 * 1024 * 1024,
                    limit: 64 * 1024 * 1024 * 1024
                }
            )),
            "A Nearby send carries up to 68.7 GB; this screenshot is 75.2 GB"
        );
        assert_eq!(
            sent(&Sent::refused(
                "Phone".into(),
                Refusal::Refused("no spool room".into())
            )),
            "The screenshot was not offered: no spool room"
        );
    }
}
