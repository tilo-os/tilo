//! The studio's words: what its receipt says of a screenshot on its way to one
//! of the person's other devices, in the desktop's language. The source
//! catalogue is embedded; other languages come from the language packs, and a
//! message missing from one falls back to `en-US`.

use std::path::Path;

use crate::link::{Failure, Refusal, Sent, WriteCause};

ui::embedded_catalog!(pub(crate) fn, ["en-US", "de-DE"]);

/// What the studio says while a screenshot is on its way to `device`.
pub(crate) fn sending(device: &str) -> String {
    t_args("screenshot-send-sending", &[("device", device.into())])
}

/// What the studio says while a screenshot is copied into this workspace's
/// files.
pub(crate) fn saving() -> String {
    t("screenshot-send-saving")
}

/// What became of the send: offered and saved where it went, or why it was
/// not.
pub(crate) fn sent(sent: &Sent) -> String {
    match sent {
        Sent::Offered { device } => t_args(
            "screenshot-send-offered",
            &[("device", device.as_str().into())],
        ),
        Sent::Refused { why, .. } | Sent::NotSaved { why } => refusal(why),
        Sent::Saved { name } => t_args("screenshot-send-saved", &[("name", name.as_str().into())]),
    }
}

/// Why a capture the person took did not come out as they asked.
pub(crate) fn failed(failure: &Failure) -> String {
    match failure {
        Failure::RecordingNotStarted(why) => t_args(
            "capture-failed-recording-not-started",
            &[("why", why.as_str().into())],
        ),
        Failure::RecordingStopped(why) => t_args(
            "capture-failed-recording-stopped",
            &[("why", why.as_str().into())],
        ),
        Failure::ReplayNotStarted(why) => t_args(
            "capture-failed-replay-not-started",
            &[("why", why.as_str().into())],
        ),
        Failure::ReplayStopped(why) => t_args(
            "capture-failed-replay-stopped",
            &[("why", why.as_str().into())],
        ),
        Failure::SoundLost => t("capture-failed-sound-lost"),
        Failure::NotTaken(why) => {
            t_args("capture-failed-not-taken", &[("why", why.as_str().into())])
        }
        Failure::NotWritten { folder, cause } => t_args(
            "capture-failed-not-written",
            &[("why", write_cause(*cause, folder).into())],
        ),
        Failure::NotCopied(why) => {
            t_args("capture-failed-not-copied", &[("why", why.as_str().into())])
        }
        Failure::NotRead(why) => t_args("capture-failed-not-read", &[("why", why.as_str().into())]),
        Failure::ClipNotSaved(why) => t_args(
            "capture-failed-clip-not-saved",
            &[("why", why.as_str().into())],
        ),
        Failure::NotOpened { path, error } => t_args(
            "capture-failed-not-opened",
            &[("path", named(path).into()), ("why", error.as_str().into())],
        ),
    }
}

/// Why a screenshot's file was not written, in the person's words: what the
/// system would have said as an error number is said as what it means for
/// their folder.
fn write_cause(cause: WriteCause, folder: &Path) -> String {
    let key = format!("capture-failed-not-written-{}", cause.words());
    t_args(&key, &[("folder", folder_words(folder).into())])
}

/// The folder a file goes in, as a person names it: their home is `~`.
pub(crate) fn folder_words(folder: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from)
        && let Ok(rest) = folder.strip_prefix(&home)
    {
        if rest.as_os_str().is_empty() {
            return String::from("~");
        }
        return format!("~/{}", rest.display());
    }
    folder.display().to_string()
}

/// Why a screenshot did not go where it was sent.
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
        Refusal::NoWorkspace => t("screenshot-send-no-workspace"),
        Refusal::NoFileName { path } => t_args(
            "screenshot-send-no-file-name",
            &[("path", named(path).into())],
        ),
        Refusal::CreateFailed { folder, error } => t_args(
            "screenshot-send-create-failed",
            &[
                ("path", named(folder).into()),
                ("error", error.as_str().into()),
            ],
        ),
        Refusal::NotSaved(error) => t_args(
            "screenshot-send-save-failed",
            &[("error", error.as_str().into())],
        ),
        Refusal::NoFreeName { folder } => t_args(
            "screenshot-send-too-many",
            &[("path", named(folder).into())],
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
    use std::path::PathBuf;

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
        assert_eq!(
            sent(&Sent::saved("Screenshot 1 (2).png".into())),
            "Saved to this workspace's files as Screenshot 1 (2).png"
        );
        assert_eq!(
            sent(&Sent::not_saved(Refusal::NoWorkspace)),
            "This session has no workspace open"
        );
        assert_eq!(
            sent(&Sent::not_saved(Refusal::NotSaved(
                "no space left on device".into()
            ))),
            "Cannot save to this workspace's files: no space left on device"
        );
        assert_eq!(
            sent(&Sent::not_saved(Refusal::NoFreeName {
                folder: "/var/workspace/Screenshots".into()
            })),
            "Every name in /var/workspace/Screenshots is taken"
        );
        assert_eq!(saving(), "Saving to this workspace's files…");
    }

    #[test]
    fn a_capture_that_did_not_come_out_says_why() {
        assert_eq!(
            failed(&Failure::RecordingNotStarted(
                "no disk space left".to_owned()
            )),
            "A recording was not started: no disk space left"
        );
        assert_eq!(
            failed(&Failure::RecordingStopped("the encoder gave up".to_owned())),
            "The recording stopped: the encoder gave up"
        );
        assert_eq!(
            failed(&Failure::ReplayNotStarted("no sound device".to_owned())),
            "The last half minute could not be kept: no sound device"
        );
        assert_eq!(
            failed(&Failure::ReplayStopped("the buffer is gone".to_owned())),
            "The last half minute is not kept any more: the buffer is gone"
        );
        assert_eq!(
            failed(&Failure::SoundLost),
            "The recording carries no more sound: its source stopped"
        );
        assert_eq!(
            failed(&Failure::NotTaken("no window".to_owned())),
            "A screenshot was not taken: no window"
        );
        // A folder that refused the file is said as what it means for the
        // person, never as what the system said.
        assert_eq!(
            failed(&Failure::NotWritten {
                folder: PathBuf::from("/pictures"),
                cause: WriteCause::NoRoom,
            }),
            "Couldn't save the screenshot: there's no room left in /pictures"
        );
        assert_eq!(
            failed(&Failure::NotWritten {
                folder: PathBuf::from("/from-the-system"),
                cause: WriteCause::ReadOnly,
            }),
            "Couldn't save the screenshot: /from-the-system is read-only"
        );
        assert_eq!(
            failed(&Failure::NotCopied("no clipboard".to_owned())),
            "A screenshot was saved, but not copied: no clipboard"
        );
        assert_eq!(
            failed(&Failure::NotRead("gone".to_owned())),
            "A screenshot was not copied: gone"
        );
        assert_eq!(
            failed(&Failure::ClipNotSaved("the worker stopped".to_owned())),
            "A clip of what just happened was not saved: the worker stopped"
        );
        assert_eq!(
            failed(&Failure::NotOpened {
                path: "/home/me/Screenshot 1.png".into(),
                error: "no app for it".to_owned()
            }),
            "/home/me/Screenshot 1.png could not be opened: no app for it"
        );
    }
}
