//! Tube's effects: everything it does that changes no document.
//!
//! [`TubeEffect`] covers navigating, playing, sharing, the person's own
//! watch history and playback preferences, and Grayjay device sync. Fetched
//! catalogue rows and paired devices are the surface's live rows
//! (`crate::rows`); they offer effects. The shared library's changes are the
//! resource's own ([`crate::resource::Library`]).

use ui::tilo_resource::{self, Effect};

use crate::action::SettingFlag;
use crate::model::{Category, Route, Settings, SkipMode};

const MAX_RESOURCE_LABEL_CHARS: usize = 160;
pub(crate) const QUALITY_OPTIONS: [&str; 4] = ["1080p", "720p", "480p", "auto"];

/// What Tube does that changes no document, invoked from its own controls,
/// the palette, an agent or a script alike. A live row offers the effects
/// bound to its entity (open *this* video).
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "tube.effect")]
pub enum TubeEffect {
    /// Show a screen.
    #[effect(view)]
    ShowScreen {
        #[param("home | trending | subscriptions | queue | history | settings")]
        screen: String,
    },
    /// Show or hide navigation.
    #[effect(view)]
    ToggleSidebar,
    /// Open video.
    #[effect(kind = "read")]
    OpenVideo {
        #[param("The video's id")]
        video: String,
    },
    /// Open creator.
    #[effect(kind = "read")]
    OpenCreator {
        #[param("The creator's channel id")]
        creator: String,
    },
    /// Open playlist.
    #[effect(kind = "read")]
    OpenPlaylist {
        #[param("The playlist's id")]
        playlist: String,
    },
    /// Play or pause.
    #[effect(kind = "read")]
    TogglePlayback,
    /// Copy share link.
    #[effect(kind = "read")]
    Share {
        #[param("The video's id")]
        video: String,
    },
    /// Download video.
    #[effect(device_only)]
    Download {
        #[param("The video's id")]
        video: String,
    },
    /// Mark watched.
    ///
    /// Adds the video to your own watch history, which follows you to your
    /// other devices and never reaches collaborators.
    #[effect(kind = "read")]
    MarkWatched {
        #[param("The video's id")]
        video: String,
    },
    /// Remove from history.
    #[effect(kind = "read")]
    ForgetWatched {
        #[param("The video's id")]
        video: String,
    },
    /// Clear watch history.
    #[effect(destructive, kind = "read")]
    ClearHistory,
    /// Pair using invite.
    ///
    /// Pairs a Grayjay device, which then reads and writes your
    /// subscriptions, history and settings.
    #[effect(destructive, device_only)]
    PairDevice {
        #[param(secret, "Grayjay pairing invite; redacted from logs and history")]
        invite: String,
    },
    /// Sync approved devices now.
    #[effect(device_only)]
    SyncDevices,
    /// Approve device.
    ///
    /// Lets a device that asked to pair read and write your subscriptions,
    /// history and settings.
    #[effect(destructive, device_only)]
    ApproveDevice {
        #[param("The device's id")]
        device: String,
    },
    /// Revoke device access.
    #[effect(destructive, device_only)]
    RevokeDevice {
        #[param("The device's id")]
        device: String,
    },
    /// Set a playback preference.
    ///
    /// Your own choice, which follows you to your other devices and never
    /// reaches collaborators.
    #[effect(kind = "read")]
    SetPreference {
        #[param(
            "The preference",
            choices = [
                "autoplay",
                "comments",
                "sponsorblock",
                "dearrow",
                "dearrow-titles",
                "dearrow-thumbnails",
                "dislikes"
            ]
        )]
        preference: String,
        #[param("true to turn it on, false to turn it off")]
        enabled: bool,
    },
    /// Set preferred quality.
    ///
    /// Your own choice, which follows you to your other devices.
    #[effect(kind = "read")]
    SetPreferredQuality {
        #[param("The quality", choices = ["1080p", "720p", "480p", "auto"])]
        quality: String,
    },
    /// Set segment handling.
    ///
    /// How playback treats one category of marked segments: skip it, show it
    /// with a skip button, or ignore it. Your own choice, which follows you to
    /// your other devices.
    #[effect(kind = "read")]
    SetSegmentHandling {
        #[param(
            "The SponsorBlock category",
            choices = [
                "sponsor",
                "selfpromo",
                "interaction",
                "intro",
                "outro",
                "preview",
                "music_offtopic",
                "filler"
            ]
        )]
        category: String,
        #[param("How it is handled", choices = ["skip", "show", "disabled"])]
        mode: String,
    },
}

impl TubeEffect {
    pub(crate) fn screen(name: &str) -> Option<Route> {
        Some(match name {
            "home" => Route::Home,
            "trending" => Route::Trending,
            "subscriptions" => Route::Subscriptions,
            "queue" => Route::Queue,
            "history" => Route::History,
            "settings" => Route::Settings,
            _ => return None,
        })
    }

    /// The preferences `settings` becomes under this effect, or why it
    /// cannot; `None` for an effect that sets no preference.
    pub(crate) fn preferences(&self, settings: &Settings) -> Option<Result<Settings, String>> {
        let mut changed = settings.clone();
        match self {
            TubeEffect::SetPreference {
                preference,
                enabled,
            } => {
                let Some(flag) = SettingFlag::from_id(preference) else {
                    return Some(Err(format!("there is no preference `{preference}`")));
                };
                flag.set(&mut changed, *enabled);
            }
            TubeEffect::SetPreferredQuality { quality } => {
                if !QUALITY_OPTIONS.contains(&quality.as_str()) {
                    return Some(Err(format!("unsupported preferred quality: {quality}")));
                }
                changed.preferred_quality.clone_from(quality);
            }
            TubeEffect::SetSegmentHandling { category, mode } => {
                let Some(category) = Category::from_api_key(category) else {
                    return Some(Err(format!(
                        "there is no SponsorBlock category `{category}`"
                    )));
                };
                let Some(mode) = SkipMode::from_key(mode) else {
                    return Some(Err(format!("there is no segment handling `{mode}`")));
                };
                changed
                    .categories
                    .insert(category.api_key().to_owned(), mode);
            }
            _ => return None,
        }
        Some(Ok(changed))
    }
}

/// Stable content identity for a comment. The upstream model has no comment
/// id, so hash all immutable source fields with an explicitly fixed FNV-1a
/// algorithm rather than addressing a row by its current list position.
pub(crate) fn comment_key(video_id: &str, comment: &crate::model::Comment) -> String {
    let mut hasher = fnv::FnvHasher::default();
    for field in [
        video_id,
        comment.author.as_str(),
        comment.initials.as_str(),
        comment.text.as_str(),
        comment.time_ago.as_str(),
    ] {
        std::hash::Hasher::write(&mut hasher, field.as_bytes());
        std::hash::Hasher::write_u8(&mut hasher, 0xff);
    }
    format!("{video_id}#{:016x}", std::hash::Hasher::finish(&hasher))
}

/// Remote metadata shown in global surfaces, kept single-line and bounded so a
/// hostile title cannot dominate the palette or force unbounded shaping.
pub(crate) fn resource_label(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    for _ in 0..MAX_RESOURCE_LABEL_CHARS {
        let Some(ch) = chars.next() else {
            return out;
        };
        out.push(if ch.is_control() { ' ' } else { ch });
    }
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

/// The quality after `current` in [`QUALITY_OPTIONS`], for a control that
/// cycles through them.
pub(crate) fn next_quality(current: &str) -> String {
    let index = QUALITY_OPTIONS
        .iter()
        .position(|quality| *quality == current)
        .unwrap_or(0);
    QUALITY_OPTIONS
        .get((index + 1) % QUALITY_OPTIONS.len())
        .copied()
        .unwrap_or("1080p")
        .to_string()
}
