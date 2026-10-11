//! The current device-private file, separate from the collaborative library.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalFile {
    pub path: PathBuf,
    pub track: crate::model::Track,
    /// Decoder metadata retained when the finished pipeline is torn down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_duration_ms: Option<u64>,
}

impl LocalFile {
    pub(crate) fn open(input: &str, previous: Option<&Self>) -> Result<Self, String> {
        let path = if input.to_ascii_lowercase().starts_with("file:") {
            media::source::file_uri_to_path(input).map_err(|error| error.to_string())?
        } else {
            // A Windows drive letter is a path, not a URL scheme. All other
            // explicit URL schemes belong to a different launch capability.
            let drive = input.as_bytes().get(1) == Some(&b':')
                && input
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphabetic);
            if !drive && url::Url::parse(input).is_ok() {
                return Err("Music opens local audio files or music://track links".into());
            }
            PathBuf::from(input)
        };
        let path = path
            .canonicalize()
            .map_err(|error| format!("Cannot open audio file: {error}"))?;
        if !crate::files::is_nonempty_file(&path) {
            return Err("Choose a non-empty audio file".into());
        }
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !matches!(
            extension.as_str(),
            "wav"
                | "wave"
                | "mp3"
                | "flac"
                | "ogg"
                | "oga"
                | "opus"
                | "m4a"
                | "aac"
                | "aif"
                | "aiff"
                | "wma"
        ) {
            return Err("This file type is not supported by Music".into());
        }
        // Validate readability without copying or hashing the media on the UI
        // thread. Codec errors remain the asynchronous decoder's authority.
        crate::files::metadata(&path)
            .map_err(|error| format!("Cannot read audio file: {error}"))?;
        media::source::path_to_file_uri(&path).map_err(|error| error.to_string())?;
        let id = previous.filter(|file| file.path == path).map_or_else(
            || format!("local-{}", uuid::Uuid::new_v4()),
            |file| file.track.id.clone(),
        );
        Ok(Self {
            track: crate::model::Track {
                id,
                title: file_title(&path),
                album_id: String::new(),
                artist_id: String::new(),
                track_no: 1,
                duration_s: 0,
                yt_id: String::new(),
                plays: 0,
            },
            path,
            observed_duration_ms: None,
        })
    }
}

fn file_title(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
