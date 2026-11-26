//! Saving and choosing files through the host's chooser: a document
//! written where the person chooses, a copy or an export, and the window's
//! record of the file it shows.

use std::sync::mpsc::TryRecvError;

use tilo_resource::Resource;
use ui_core::portal::{
    ChosenHandle, DestinationOutcome, MultiOutcome, OpenFileChannel, OpenFileOutcome, OpenedFile,
    PickRequest, SaveChannel, SaveDestination, SaveOutcome,
};

use super::{CHOOSER_GONE, ChosenFile, EffectAnswer, EffectCx, Effected};

impl<R: Resource, H: 'static> EffectCx<R, H> {
    /// Ask the person to choose a file with the host's chooser (`request` is an Open request),
    /// then run `then` on the UI thread with its name and contents, or `None` when they
    /// cancelled. The file comes through the chooser's handle ([`Self::choose_handle`]), so it
    /// is read with the rights the person gave exactly that file. Bound the read with
    /// [`PickRequest::max_contents_bytes`]; an oversized selection fails the job before calling
    /// `then`. The effect must be declared `job`.
    pub fn choose_file(
        &mut self,
        request: PickRequest,
        then: impl FnOnce(&mut H, Option<ChosenFile>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let limit = request.contents_byte_limit();
        self.choose_handle(request, move |host, chosen, cx| {
            let Some(handle) = chosen else {
                return then(host, None, cx);
            };
            then(host, Some(read_chosen(handle, limit)?), cx)
        })
    }

    /// [`Self::choose_file`] for a request that lets the person choose several files
    /// ([`PickRequest::open_multiple`]): `then` receives every chosen file, none when they
    /// cancelled, each read as [`Self::choose_file`] reads one. A request contents limit applies
    /// to the total bytes across the entire selection. The effect must be declared `job`.
    pub fn choose_files(
        &mut self,
        request: PickRequest,
        then: impl FnOnce(&mut H, Vec<ChosenFile>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        let mut remaining = request.contents_byte_limit();
        self.choose_handles(request, move |host, chosen, cx| {
            let mut files = Vec::with_capacity(chosen.len());
            for handle in chosen {
                let file = read_chosen(handle, remaining)?;
                if let Some(remaining) = &mut remaining {
                    *remaining = remaining.checked_sub(file.bytes.len()).ok_or_else(|| {
                        told("Those files are too large for the app to read. Choose fewer or smaller files.".into())
                    })?;
                }
                files.push(file);
            }
            then(host, files, cx)
        })
    }

    /// Ask the person to choose several files or folders with the host's chooser
    /// ([`PickRequest::open_multiple`]) and run `then` on the UI thread with each already open:
    /// what [`Self::choose_handle`] gives for a selection. A browser page chooses one file at a
    /// time and refuses the rest. The effect must be declared `job`.
    pub fn choose_handles(
        &mut self,
        request: PickRequest,
        then: impl FnOnce(&mut H, Vec<ChosenHandle>, &mut Self) -> EffectAnswer + 'static,
    ) -> Effected {
        self.wait_on(
            picked_handles(request),
            move |host, outcome, cx| match outcome {
                Some(Ok(opened)) => {
                    let mut handles = Vec::with_capacity(opened.len());
                    for opened in opened {
                        let name = opened.display_name().to_owned();
                        let (file, reference) = opened.into_parts();
                        let Some(reference) = reference else {
                            return Err(told(
                                "This host cannot open the file again once it is chosen.".into(),
                            ));
                        };
                        handles.push(ChosenHandle::from_picked(file, reference, name));
                    }
                    then(host, handles, cx)
                }
                Some(Err(reason)) => Err(told(reason)),
                None => Err(CHOOSER_GONE.into()),
            },
        )
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
        let opens = tilo_ui_base::dnd::mime_for_path(path).is_some_and(|media_type| {
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

/// The files a choice reopened, or why the app cannot open them. Empty when the person
/// cancelled.
type OpenedHandles = Result<Vec<OpenedFile>, String>;

/// Polls the choice [`EffectCx::choose_handles`] waits for: the reopened files once it is taken
/// up, or [`TryRecvError::Empty`] while some of them are still on their way.
type PickedHandles = dyn FnMut() -> Result<OpenedHandles, TryRecvError>;

/// What [`EffectCx::choose_handles`] waits for, outside the app's generic code: the choice, then
/// each chosen item reopened by the reference it answered. Empty when the person cancelled.
#[inline(never)]
fn picked_handles(request: PickRequest) -> Box<PickedHandles> {
    let picks = request.send_multiple();
    let mut picks = Some(picks);
    let mut opening: Vec<OpenFileChannel> = Vec::new();
    let mut opened: Vec<OpenedFile> = Vec::new();
    Box::new(move || {
        if let Some(receiver) = picks.as_ref() {
            match receiver.try_recv() {
                Ok(MultiOutcome::Selected(paths)) => {
                    picks = None;
                    if paths.is_empty() {
                        return Ok(Ok(Vec::new()));
                    }
                    for path in paths {
                        opening.push(ui_core::portal::reopen(&path, false));
                    }
                }
                Ok(MultiOutcome::Cancelled) => {
                    picks = None;
                    return Ok(Ok(Vec::new()));
                }
                Ok(MultiOutcome::Failed(reason)) => {
                    picks = None;
                    return Ok(Err(reason));
                }
                Err(TryRecvError::Empty) => return Err(TryRecvError::Empty),
                Err(TryRecvError::Disconnected) => {
                    picks = None;
                    return Ok(Err(CHOOSER_GONE.to_owned()));
                }
            }
        }
        while let Some(receiver) = opening.first() {
            match receiver.try_recv() {
                Ok(OpenFileOutcome::Opened(file)) => {
                    opened.push(file);
                    opening.remove(0);
                }
                Ok(OpenFileOutcome::Cancelled) => {
                    return Ok(Err(CHOOSER_GONE.to_owned()));
                }
                Ok(OpenFileOutcome::Failed(reason)) => return Ok(Err(reason)),
                Err(TryRecvError::Empty) => return Err(TryRecvError::Empty),
                Err(TryRecvError::Disconnected) => {
                    opening.remove(0);
                }
            }
        }
        Ok(Ok(std::mem::take(&mut opened)))
    })
}

/// The bytes of a file the person chose, read through the handle that holds exactly it.
fn read_chosen(handle: ChosenHandle, limit: Option<usize>) -> Result<ChosenFile, String> {
    let name = handle.name().to_owned();
    let reference = handle.reference().to_path_buf();
    let mut file = handle.into_file();
    let mut bytes = Vec::new();
    let read = match limit {
        Some(limit) => {
            let mut limited = std::io::Read::take(&mut file, (limit as u64).saturating_add(1));
            std::io::Read::read_to_end(&mut limited, &mut bytes)
        }
        None => std::io::Read::read_to_end(&mut file, &mut bytes),
    };
    read.map_err(|error| told(format!("“{name}” can't be read: {error}")))?;
    if limit.is_some_and(|limit| bytes.len() > limit) {
        return Err(told(
            "Those files are too large for the app to read. Choose fewer or smaller files.".into(),
        ));
    }
    Ok(ChosenFile {
        identity: Some(ui_core::resource::DocumentId::identify_file(&reference)),
        name,
        media_type: tilo_ui_base::dnd::mime_for_path(&reference).map(str::to_owned),
        bytes,
    })
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
