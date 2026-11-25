//! The files a box carries that the session made itself: what a report carries by default, the
//! recording of the last moments the system's capture kept and a picture of the window it is
//! about.
//!
//! The session hands the window's box each file as a chip the person may take off before sending
//! ([`ui::floating_surface::FloatingSurfaceHostMessage::ShareSelection`]). Nothing leaves the
//! device until the person sends: only then is each file kept in the content store of the
//! workspace the message goes to, under the scope the content put API serves, and the message
//! names it there as the file Chat's actions take, as a file chosen in an app is.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use tilo_reference::Attached;
use tilolang_tilo::runtime::blob::FileValue;

/// How many files the session holds for boxes at once: a report's two, for a few reports the
/// person opened and did not send.
const HELD: usize = 8;

/// One file the session handed a box: where its bytes are, and what kind of file it is.
#[derive(Clone, Debug)]
struct Held {
    path: PathBuf,
    media: String,
    size: u64,
}

/// The files the session handed boxes, by the content hash each chip carries.
#[derive(Clone, Default)]
pub(crate) struct Carried {
    held: Rc<RefCell<Vec<(String, Held)>>>,
}

impl Carried {
    /// Hand a box the file at `path`, called `name`, of the kind `media`: answers the chip it
    /// carries, a picture or a video of `width` × `height` (0 when unknown) or any other file.
    ///
    /// # Errors
    /// The file cannot be read.
    pub(crate) fn offer(
        &self,
        path: &Path,
        name: &str,
        media: &str,
        (width, height): (u32, u32),
    ) -> Result<Attached, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let blob = blake3::hash(&bytes).to_hex().to_string();
        let size = bytes.len() as u64;
        let attached = if media.starts_with("image/") {
            Attached::Image {
                name: name.to_owned(),
                blob: blob.clone(),
                w: width,
                h: height,
            }
        } else if media.starts_with("video/") {
            Attached::Video {
                name: name.to_owned(),
                blob: blob.clone(),
                w: width,
                h: height,
                size,
            }
        } else {
            Attached::File {
                name: name.to_owned(),
                blob: blob.clone(),
                size,
            }
        };
        let mut held = self.held.borrow_mut();
        held.retain(|(kept, _)| *kept != blob);
        held.push((
            blob,
            Held {
                path: path.to_owned(),
                media: media.to_owned(),
                size,
            },
        ));
        let over = held.len().saturating_sub(HELD);
        for (_, gone) in held.drain(..over) {
            let _ = std::fs::remove_file(gone.path);
        }
        Ok(attached)
    }

    /// The file `file` a box sent, kept by `keep` in the workspace it goes to, as an app's action
    /// takes a file: its name, kind and size, and the address `keep` answered for its bytes.
    /// `None` for what is no file.
    ///
    /// # Errors
    /// The session handed no box this file, or it could not be kept.
    pub(crate) fn kept(
        &self,
        file: &Attached,
        keep: &dyn Fn(&Path) -> Result<String, String>,
    ) -> Option<Result<FileValue, String>> {
        let blob = file.blob()?;
        let name = file.label();
        let held = self
            .held
            .borrow()
            .iter()
            .find(|(kept, _)| kept == blob)
            .map(|(_, held)| held.clone());
        let Some(held) = held else {
            return Some(Err(format!(
                "{name} is not in the workspace yet: add it to Files, then send it from there"
            )));
        };
        Some(keep(&held.path).map(|hash| {
            FileValue::new(
                name,
                held.media,
                i64::try_from(held.size).unwrap_or(i64::MAX),
                hash,
            )
        }))
    }
}

/// Where the session keeps the files it hands boxes until they are sent: its own folder in the
/// person's runtime directory, which goes when they log out.
pub(crate) fn folder() -> PathBuf {
    tilo_paths::xdg::runtime_dir_or_default().join("tilo-session/carried")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file the session handed a box is kept where the message goes only once it is sent, and
    /// goes as the file it is; a file the session never handed a box is refused, saying why.
    #[test]
    fn a_file_handed_to_a_box_is_kept_where_it_is_sent_and_no_other_is() {
        let folder = tempfile::tempdir().expect("a folder");
        let path = folder.path().join("0-recording.mp4");
        std::fs::write(&path, b"the last half minute").expect("a recording");
        let carried = Carried::default();
        let chip = carried
            .offer(&path, "Recording.mp4", "video/mp4", (0, 0))
            .expect("the recording is carried");
        let Attached::Video { name, size, .. } = &chip else {
            panic!("a recording is a video: {chip:?}");
        };
        assert_eq!((name.as_str(), *size), ("Recording.mp4", 20));

        let kept = std::cell::RefCell::new(Vec::new());
        let keep = |at: &Path| {
            kept.borrow_mut().push(at.to_owned());
            Ok("ef".repeat(32))
        };
        let file = carried
            .kept(&chip, &keep)
            .expect("a file")
            .expect("kept where it goes");
        assert_eq!(
            *kept.borrow(),
            [path],
            "its bytes are kept, from where the session wrote them"
        );
        assert_eq!(file.name, "Recording.mp4");
        assert_eq!(file.media, "video/mp4");
        assert_eq!(file.size, 20);
        assert_eq!(
            file.hash,
            "ef".repeat(32),
            "under the address the store answered"
        );

        let words = Attached::Text {
            text: "issue 42".into(),
            from: None,
        };
        assert!(carried.kept(&words, &keep).is_none(), "words are no file");
        let stranger = Attached::File {
            name: "notes.pdf".into(),
            blob: "00".repeat(32),
            size: 1,
        };
        let refused = carried.kept(&stranger, &keep).expect("a file");
        assert!(
            refused.is_err_and(|why| why.contains("add it to Files")),
            "a file the session never held is refused, saying why"
        );
    }
}
