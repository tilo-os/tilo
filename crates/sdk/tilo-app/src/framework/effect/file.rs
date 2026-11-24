//! Saving and choosing files through the host's chooser: a document
//! written where the person chooses, a copy or an export, and the window's
//! record of the file it shows.

use std::sync::mpsc::TryRecvError;

use tilo_resource::Resource;
use ui_core::portal::{
    ChosenHandle, ContentsOutcome, DestinationOutcome, OpenFileOutcome, PickRequest, SaveChannel,
    SaveDestination, SaveOutcome,
};

use super::{CHOOSER_GONE, ChosenFile, EffectAnswer, EffectCx, Effected};

impl<R: Resource, H: 'static> EffectCx<R, H> {
    /// Ask the person to choose a file with the host's chooser (`request`
    /// is an Open request), then run `then` on the UI thread with its name
    /// and contents, or `None` when they cancelled. Works where files have
    /// no paths, such as a browser page. Bound reads with
    /// [`PickRequest::max_contents_bytes`]; an oversized selection fails the
    /// job before calling `then`. The effect must be declared `job`.
    pub fn choose_file(
        &mut self,
        request: PickRequest,
        then: impl FnOnce(&mut H, Option<ChosenFile>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let chosen = request.send_contents();
        self.wait_for(chosen, move |host, outcome, cx| match outcome {
            Some(ContentsOutcome::Picked(files)) => {
                then(host, files.into_iter().next().map(ChosenFile::from), cx)
            }
            Some(ContentsOutcome::Cancelled) => then(host, None, cx),
            Some(ContentsOutcome::Failed(reason)) => Err(told(reason)),
            None => Err(CHOOSER_GONE.into()),
        })
    }

    /// [`Self::choose_file`] for a request that lets the person choose
    /// several files ([`PickRequest::open_multiple`]): `then` receives every
    /// chosen file, none when they cancelled. A request contents limit applies
    /// to the total bytes across the entire selection.
    pub fn choose_files(
        &mut self,
        request: PickRequest,
        then: impl FnOnce(&mut H, Vec<ChosenFile>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let chosen = request.send_contents();
        self.wait_for(chosen, move |host, outcome, cx| match outcome {
            Some(ContentsOutcome::Picked(files)) => {
                then(host, files.into_iter().map(ChosenFile::from).collect(), cx)
            }
            Some(ContentsOutcome::Cancelled) => then(host, Vec::new(), cx),
            Some(ContentsOutcome::Failed(reason)) => Err(told(reason)),
            None => Err(CHOOSER_GONE.into()),
        })
    }

    /// Ask the person to choose a file or folder with the host's chooser and run `then` on the
    /// UI thread with it already open, or `None` when they cancelled: for a file the app reads
    /// in place rather than into memory (a disk image, a video), or a folder it walks. The
    /// handle says what names the same item again ([`ChosenHandle::reference`]), so a window
    /// that showed it reopens it after a restart. The effect must be declared `job`.
    pub fn choose_handle(
        &mut self,
        request: PickRequest,
        then: impl FnOnce(&mut H, Option<ChosenHandle>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let chosen = request.send_open_handle();
        self.wait_for(chosen, move |host, outcome, cx| match outcome {
            Some(OpenFileOutcome::Opened(opened)) => {
                let name = opened.display_name().to_owned();
                let (file, reference) = opened.into_parts();
                match reference {
                    Some(reference) => then(
                        host,
                        Some(ChosenHandle::from_picked(file, reference, name)),
                        cx,
                    ),
                    None => Err(told(
                        "This host cannot open the file again once it is chosen.".into(),
                    )),
                }
            }
            Some(OpenFileOutcome::Cancelled) => then(host, None, cx),
            Some(OpenFileOutcome::Failed(reason)) => Err(told(reason)),
            None => Err(CHOOSER_GONE.into()),
        })
    }

    /// Ask the person where to save `bytes` with the host's chooser
    /// (`request` is a Save request suggesting a file name; a browser page
    /// downloads them under it), then run `then` on the UI thread with
    /// whether they were saved: `false` when the person cancelled. The
    /// effect must be declared `job`.
    pub fn save_file(
        &mut self,
        request: PickRequest,
        bytes: Vec<u8>,
        then: impl FnOnce(&mut H, bool, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        self.wait_on(
            saving_copy(request, bytes),
            move |host, outcome, cx| match outcome {
                Some(SaveOutcome::Saved) => then(host, true, cx),
                Some(SaveOutcome::Cancelled) => then(host, false, cx),
                Some(SaveOutcome::Failed(reason)) => Err(reason),
                None => Err(CHOOSER_GONE.into()),
            },
        )
    }

    /// Ask the person where to save with the host's chooser (`request` is
    /// a Save request suggesting a file name), then run `then` on the UI
    /// thread with the place, or `None` when they cancelled. The place takes
    /// bytes made after the choice, and can be kept to write there again on
    /// every later Save without asking: a path where files have paths, the
    /// browser's own Save dialog file on a browser page. The effect must be
    /// declared `job`.
    pub fn choose_destination(
        &mut self,
        request: PickRequest,
        then: impl FnOnce(&mut H, Option<SaveDestination>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let chosen = request.send_save_destination();
        self.wait_on(
            move || chosen.try_recv(),
            move |host, outcome, cx| match outcome {
                Some(DestinationOutcome::Chosen(destination)) => then(host, Some(destination), cx),
                Some(DestinationOutcome::Cancelled) => then(host, None, cx),
                Some(DestinationOutcome::Failed(reason)) => Err(told(reason)),
                None => Err(CHOOSER_GONE.into()),
            },
        )
    }

    /// Run `then` on the UI thread once `destination` holds `bytes`: `Ok`
    /// when they were saved, the reason when not. The effect must be
    /// declared `job`.
    pub fn write_destination(
        &mut self,
        destination: &SaveDestination,
        bytes: Vec<u8>,
        then: impl FnOnce(&mut H, Result<(), String>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let written = destination.write(bytes);
        self.wait_for(written, move |host, outcome, cx| match outcome {
            Some(SaveOutcome::Saved) => then(host, Ok(()), cx),
            Some(SaveOutcome::Cancelled) => then(host, Err("the save was cancelled".into()), cx),
            Some(SaveOutcome::Failed(reason)) => then(host, Err(reason), cx),
            None => Err(CHOOSER_GONE.into()),
        })
    }

    /// Ask where this window's document is saved (`request` is a Save
    /// request), make its bytes with `bytes` for the name they chose, and
    /// write them there: the window shows that file from then on. `then` runs
    /// with the place, or `None` when the person cancelled. This is the one
    /// path a document is saved to a new place by; a copy or an export uses
    /// [`Self::save_file`] or [`Self::write_destination`], and the window goes
    /// on showing what it showed. The effect must be declared `job`.
    pub fn save_document(
        &mut self,
        request: PickRequest,
        bytes: impl FnOnce(&str) -> Result<Vec<u8>, String> + 'static,
        then: impl FnOnce(&mut H, Option<SaveDestination>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let chosen = request.send_save_destination();
        self.wait_on(
            move || chosen.try_recv(),
            move |host, outcome, cx| match outcome {
                Some(DestinationOutcome::Chosen(destination)) => {
                    let written = destination.write(bytes(destination.name())?);
                    Ok(
                        cx.wait_for(written, move |host, outcome, cx| match outcome {
                            Some(SaveOutcome::Saved) => {
                                cx.saved_document_at(
                                    destination.reference().or_else(|| destination.location()),
                                );
                                then(host, Some(destination), cx)
                            }
                            Some(SaveOutcome::Cancelled) => Err("the save was cancelled".into()),
                            Some(SaveOutcome::Failed(reason)) => Err(told(reason)),
                            None => Err(CHOOSER_GONE.into()),
                        }),
                    )
                }
                Some(DestinationOutcome::Cancelled) => then(host, None, cx),
                Some(DestinationOutcome::Failed(reason)) => Err(told(reason)),
                None => Err(CHOOSER_GONE.into()),
            },
        )
    }

    /// As [`Self::write_destination`], and the window shows that file from
    /// then on: the place the document lives, written again without asking.
    pub fn write_document(
        &mut self,
        destination: &SaveDestination,
        bytes: Vec<u8>,
        then: impl FnOnce(&mut H, Result<(), String>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let at = destination
            .reference()
            .or_else(|| destination.location())
            .map(std::path::Path::to_path_buf);
        let written = destination.write(bytes);
        self.wait_for(written, move |host, outcome, cx| match outcome {
            Some(SaveOutcome::Saved) => {
                cx.saved_document_at(at.as_deref());
                then(host, Ok(()), cx)
            }
            Some(SaveOutcome::Cancelled) => then(host, Err("the save was cancelled".into()), cx),
            Some(SaveOutcome::Failed(reason)) => then(host, Err(reason), cx),
            None => Err(CHOOSER_GONE.into()),
        })
    }

    /// The window shows the file at `path` from now on: its document was
    /// saved there (Save As). An app that writes its document itself says so
    /// once the write landed; a save through [`Self::save_document`] or
    /// [`Self::write_document`] says it already.
    pub fn show_file(&self, path: &std::path::Path) {
        if let Some(shows) = &self.env.shows {
            shows.show(path);
        }
    }

    /// The window shows what a save wrote when that file is one the app opens: its
    /// document. A save to any other file (an export, a copy) leaves the window as it
    /// was.
    fn saved_document_at(&self, path: Option<&std::path::Path>) {
        let Some(path) = path else {
            return;
        };
        let opens = ui_core::dnd::mime_for_path(path).is_some_and(|media_type| {
            self.env
                .opens
                .iter()
                .any(|open| *open == ui_core::application::ANY_MEDIA_TYPE || *open == media_type)
        });
        if opens {
            self.show_file(path);
        }
    }
}

/// What [`EffectCx::save_file`] waits for, outside the app's generic code: the chosen place, then
/// its write. `Cancelled` only when the person chose no place.
#[inline(never)]
fn saving_copy(
    request: PickRequest,
    bytes: Vec<u8>,
) -> Box<dyn FnMut() -> Result<SaveOutcome, TryRecvError>> {
    let chosen = request.send_save_destination();
    let mut bytes = Some(bytes);
    let mut written: Option<SaveChannel> = None;
    Box::new(move || {
        let written = match &written {
            Some(written) => written,
            None => match chosen.try_recv()? {
                DestinationOutcome::Chosen(place) => {
                    written.insert(place.write(bytes.take().unwrap_or_default()))
                }
                DestinationOutcome::Cancelled => return Ok(SaveOutcome::Cancelled),
                DestinationOutcome::Failed(reason) => return Ok(SaveOutcome::Failed(told(reason))),
            },
        };
        written.try_recv().map(|outcome| match outcome {
            SaveOutcome::Cancelled => SaveOutcome::Failed("the save was cancelled".into()),
            outcome => outcome,
        })
    })
}

/// Tell the person on the window's bar why the chooser or its write failed: the host words the
/// reason for them (the workspace is not on screen, the disk is full). The effect fails with it too.
fn told(reason: String) -> String {
    ui_core::window_toolbar::notify(Some(ui_core::window_toolbar::ToolbarNotice::new(
        reason.clone(),
        ui_core::widget::toast::ToastKind::Error,
    )));
    reason
}

#[cfg(test)]
mod tests {
    /// A chooser that fails says why on the window's bar, and the effect fails with the same words.
    #[test]
    fn a_failed_chooser_says_why_on_the_bar() {
        let reason = super::told("the disk is full".to_owned());
        assert_eq!(reason, "the disk is full");
        let notice = ui_core::window_toolbar::notice().expect("the bar says why");
        assert_eq!(notice.text, "the disk is full");
    }
}
