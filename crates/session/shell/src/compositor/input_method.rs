//! Which input method owns the seat.
//!
//! `input-method-v2` admits one input method per seat: the first instance
//! bound receives text-input focus and every later one is told it is
//! unavailable. Two trusted programs want that slot — the on-screen keyboard
//! and the composing input method (Fcitx5, which provides Pinyin, Mozc and
//! Hangul). Neither is reachable from the public app socket; each is started
//! by the compositor on a private display whose connections carry an
//! immutable [`TrustedInputMethod`] credential.
//!
//! The rule that keeps exactly one of them active is [`arbitrate`]:
//!
//! * the active input source is an input-method engine → the composing input
//!   method owns the seat, and the on-screen keyboard is stopped;
//! * otherwise, the on-screen keyboard preference is on → the on-screen
//!   keyboard owns the seat, and the composing input method is not running;
//! * otherwise nobody does.
//!
//! The on-screen keyboard does not try to feed the composer: its keys are
//! committed text, not key events a composer could compose, and splitting one
//! text field between two input methods would leave preedit owned by neither.
//! Switching back to a keyboard layout brings the on-screen keyboard back.
//!
//! [`SeatInputMethod`] enforces the decision on the wire: the input-method
//! and virtual-keyboard globals are visible only to connections of the
//! current owner's kind, and the compositor disconnects the previous owner's
//! connections when ownership moves, so a slow-exiting process can never hold
//! the slot the new owner needs.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use settings_store::hardware::InputMethodEngine;

/// The kind of trusted input method a private-display connection belongs to.
/// Set once by the socket acceptor that authenticated the peer; a client can
/// never choose or change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrustedInputMethod {
    /// The Tilo on-screen keyboard (`so.tilo.osk`).
    OnScreenKeyboard,
    /// The composing input method for Chinese, Japanese and Korean.
    Composer,
}

impl TrustedInputMethod {
    fn code(self) -> u8 {
        match self {
            Self::OnScreenKeyboard => 1,
            Self::Composer => 2,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::OnScreenKeyboard),
            2 => Some(Self::Composer),
            _ => None,
        }
    }
}

/// Who owns the seat's input method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMethodOwner {
    Nobody,
    OnScreenKeyboard,
    Composer(InputMethodEngine),
}

impl InputMethodOwner {
    pub fn kind(self) -> Option<TrustedInputMethod> {
        match self {
            Self::Nobody => None,
            Self::OnScreenKeyboard => Some(TrustedInputMethod::OnScreenKeyboard),
            Self::Composer(_) => Some(TrustedInputMethod::Composer),
        }
    }

    pub fn engine(self) -> Option<InputMethodEngine> {
        match self {
            Self::Composer(engine) => Some(engine),
            _ => None,
        }
    }
}

/// The ownership rule; see the module documentation. `composer_available` is
/// false when the image has no composing input method, in which case an
/// engine source falls back to plain typing on the primary layout and the
/// on-screen keyboard keeps working.
pub fn arbitrate(
    active_engine: Option<InputMethodEngine>,
    composer_available: bool,
    osk_enabled: bool,
) -> InputMethodOwner {
    match active_engine {
        Some(engine) if composer_available => InputMethodOwner::Composer(engine),
        _ if osk_enabled => InputMethodOwner::OnScreenKeyboard,
        _ => InputMethodOwner::Nobody,
    }
}

/// The seat's current input-method owner, shared with the global filters.
#[derive(Clone, Debug, Default)]
pub struct SeatInputMethod(Arc<AtomicU8>);

impl SeatInputMethod {
    pub fn set(&self, owner: Option<TrustedInputMethod>) {
        self.0
            .store(owner.map_or(0, TrustedInputMethod::code), Ordering::Release);
    }

    pub fn get(&self) -> Option<TrustedInputMethod> {
        TrustedInputMethod::from_code(self.0.load(Ordering::Acquire))
    }

    /// Whether a connection with `credential` may see and bind the
    /// input-method and virtual-keyboard globals. Public connections carry no
    /// credential and never can.
    pub fn admits(&self, credential: Option<TrustedInputMethod>) -> bool {
        credential.is_some() && credential == self.get()
    }
}

/// Where the composing input method is installed. Its private display admits
/// only a process running exactly this executable.
pub const COMPOSER_EXECUTABLE: &str = "/usr/bin/fcitx5";

/// Addons of the composer that Tilo does not use. Cloud Pinyin sends typed
/// text to a remote server, the tray and panel addons need services the
/// session does not run, the clipboard addon claims a global chord, and the
/// IBus/Fcitx4 frontends would claim bus names nobody asked for. XIM stays:
/// it serves X11 apps on the workspace displays the session connects it to.
const DISABLED_COMPOSER_ADDONS: &str = "cloudpinyin,clipboard,kimpanel,notificationitem,\
ibusfrontend,fcitx4frontend,virtualkeyboard";

/// What the composer needs from the session: its user, locale, session bus,
/// and the directories its dictionaries and engines live in.
const COMPOSER_INHERITED_ENV: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "PATH",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "XDG_RUNTIME_DIR",
    "XDG_DATA_HOME",
    "XDG_DATA_DIRS",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
];

/// Whether the composer and the engine's addon are both installed, so the
/// arbiter only hands the seat to an engine that can actually run.
pub fn engine_installed(engine: InputMethodEngine) -> bool {
    Path::new(COMPOSER_EXECUTABLE).is_file() && engine.is_installed()
}

/// The composer's process description for one engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerLaunch {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Variables set for the child.
    pub env: Vec<(String, String)>,
    /// The only variables inherited from the compositor. Everything else —
    /// the privileged socket's name, X11 displays, Tilo session state — stays
    /// out of the composer's reach.
    pub inherit: Vec<&'static str>,
    /// Configuration files to write under `config_home` before starting.
    pub files: Vec<(PathBuf, String)>,
}

/// Describe the composer run for `engine`, typing on `layout` (an XKB source
/// id, `layout` or `layout:variant`), connected to `socket`, configured from
/// `config_home`, and reaching X11 workspace displays with the cookies in
/// `x11_authority`. The configuration is regenerated on every start: the
/// session owns which engine is active and how it is switched, so the
/// composer's own switching chords are cleared (Super+Space is Tilo's
/// search, Ctrl+Space is an application chord). User dictionaries stay in the
/// composer's ordinary data directory.
pub fn composer_launch(
    engine: InputMethodEngine,
    layout: &str,
    socket: &Path,
    config_home: &Path,
    x11_authority: &Path,
) -> ComposerLaunch {
    let layout = layout.replace(':', "-");
    let profile = format!(
        "[Groups/0]\nName=Tilo\nDefault Layout={layout}\nDefaultIM={engine}\n\n\
         [Groups/0/Items/0]\nName=keyboard-{layout}\nLayout=\n\n\
         [Groups/0/Items/1]\nName={engine}\nLayout=\n\n\
         [GroupOrder]\n0=Tilo\n",
        engine = engine.name(),
    );
    // Shift toggles the engine's own Latin mode, and the dedicated
    // Hangul/Zenkaku keys keep working; every enumerate chord is empty.
    let config = "[Hotkey]\nEnumerateWithTriggerKeys=False\n\n\
         [Hotkey/TriggerKeys]\n0=Zenkaku_Hankaku\n1=Hangul\n\n\
         [Hotkey/AltTriggerKeys]\n0=Shift_L\n\n\
         [Hotkey/EnumerateForwardKeys]\n\n[Hotkey/EnumerateBackwardKeys]\n\n\
         [Hotkey/EnumerateGroupForwardKeys]\n\n[Hotkey/EnumerateGroupBackwardKeys]\n\n\
         [Hotkey/ActivateKeys]\n\n[Hotkey/DeactivateKeys]\n\n\
         [Behavior]\nActiveByDefault=True\nShareInputState=All\n\
         ShowInputMethodInformation=False\nshowInputMethodInformationWhenFocusIn=False\n"
        .to_string();
    let dir = config_home.join("fcitx5");
    ComposerLaunch {
        program: PathBuf::from(COMPOSER_EXECUTABLE),
        // Replace a composer a previous session left on the bus.
        args: vec![
            "--replace".into(),
            "--disable".into(),
            DISABLED_COMPOSER_ADDONS.into(),
        ],
        env: vec![
            ("WAYLAND_DISPLAY".into(), socket.display().to_string()),
            ("XDG_CONFIG_HOME".into(), config_home.display().to_string()),
            ("XDG_SESSION_TYPE".into(), "wayland".into()),
            ("XAUTHORITY".into(), x11_authority.display().to_string()),
        ],
        inherit: COMPOSER_INHERITED_ENV.to_vec(),
        files: vec![(dir.join("profile"), profile), (dir.join("config"), config)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_one_input_method_owns_the_seat() {
        use InputMethodEngine::{Hangul, Pinyin};
        use InputMethodOwner::{Composer, Nobody, OnScreenKeyboard};
        // (active engine, composer installed, OSK preference) → owner
        let table = [
            (None, true, false, Nobody),
            (None, true, true, OnScreenKeyboard),
            (None, false, true, OnScreenKeyboard),
            (Some(Pinyin), true, false, Composer(Pinyin)),
            (Some(Hangul), true, true, Composer(Hangul)),
            (Some(Pinyin), false, true, OnScreenKeyboard),
            (Some(Pinyin), false, false, Nobody),
        ];
        for (engine, available, osk, expected) in table {
            assert_eq!(
                arbitrate(engine, available, osk),
                expected,
                "engine {engine:?}, composer installed {available}, OSK {osk}"
            );
        }
    }

    #[test]
    fn only_the_current_owner_kind_is_admitted() {
        let seat = SeatInputMethod::default();
        assert!(!seat.admits(None));
        assert!(!seat.admits(Some(TrustedInputMethod::OnScreenKeyboard)));
        assert!(!seat.admits(Some(TrustedInputMethod::Composer)));

        seat.set(Some(TrustedInputMethod::Composer));
        assert!(seat.admits(Some(TrustedInputMethod::Composer)));
        assert!(!seat.admits(Some(TrustedInputMethod::OnScreenKeyboard)));
        assert!(
            !seat.admits(None),
            "a public client never binds an input method"
        );

        seat.set(Some(TrustedInputMethod::OnScreenKeyboard));
        assert!(seat.admits(Some(TrustedInputMethod::OnScreenKeyboard)));
        assert!(!seat.admits(Some(TrustedInputMethod::Composer)));

        seat.set(None);
        assert_eq!(seat.get(), None);
    }

    #[test]
    fn composer_runs_the_engine_on_the_private_display_without_cloud_or_chords() {
        let launch = composer_launch(
            InputMethodEngine::Mozc,
            "jp:kana",
            Path::new("/run/user/1000/tilo-im-1"),
            Path::new("/run/user/1000/tilo-session/input-method"),
            Path::new("/run/user/1000/tilo-session/xwayland/composer-Xauthority"),
        );
        assert_eq!(launch.program, Path::new(COMPOSER_EXECUTABLE));
        let disabled = launch
            .args
            .iter()
            .skip_while(|arg| *arg != "--disable")
            .nth(1)
            .expect("disabled addons");
        assert!(disabled.split(',').any(|addon| addon == "cloudpinyin"));
        assert!(
            !disabled.split(',').any(|addon| addon == "xim"),
            "X11 apps compose through XIM"
        );
        assert!(launch.env.contains(&(
            "XAUTHORITY".into(),
            "/run/user/1000/tilo-session/xwayland/composer-Xauthority".into()
        )));
        assert!(
            launch
                .env
                .contains(&("WAYLAND_DISPLAY".into(), "/run/user/1000/tilo-im-1".into()))
        );
        for private in [
            "DISPLAY",
            "XAUTHORITY",
            "TILO_WAYLAND_PRIVILEGED",
            "WAYLAND_SOCKET",
        ] {
            assert!(
                !launch.inherit.contains(&private),
                "{private} must not reach the composer"
            );
        }
        assert!(launch.inherit.contains(&"DBUS_SESSION_BUS_ADDRESS"));
        let file = |name: &str| {
            launch
                .files
                .iter()
                .find(|(path, _)| path.ends_with(name))
                .map(|(_, body)| body.as_str())
                .expect(name)
        };
        let profile = file("fcitx5/profile");
        assert!(profile.contains("DefaultIM=mozc"));
        assert!(profile.contains("Name=keyboard-jp-kana"));
        let config = file("fcitx5/config");
        assert!(config.contains("ActiveByDefault=True"));
        assert!(
            !config.to_lowercase().contains("super+space")
                && !config.to_lowercase().contains("control+space"),
            "the composer must not claim Tilo's search chord or an app chord"
        );
    }
}
