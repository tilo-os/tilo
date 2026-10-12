//! Which input method owns the seat.
//!
//! `input-method-v2` admits one input method per seat: the first instance
//! bound receives text-input focus and every later one is told it is
//! unavailable. Two trusted programs want that slot — the on-screen keyboard
//! and the composing input method (Fcitx5, which provides Rime, Anthy and
//! Hangul). Neither is reachable from the public app socket; each is started
//! by the compositor on a private display whose connections carry an
//! immutable [`TrustedInputMethod`] credential.
//!
//! The rule that keeps exactly one of them active is [`arbitrate`]:
//!
//! * the active input source is an input-method engine → the composing input
//!   method owns the seat, and the on-screen keyboard is stopped;
//! * otherwise, the on-screen keyboard is wanted → the on-screen keyboard
//!   owns the seat, and the composing input method is not running;
//! * otherwise nobody does.
//!
//! Whether the on-screen keyboard runs is [`osk_service_wanted`]: its
//! preference is `auto`, `always` or `never`, and `auto` keeps it running
//! wherever a touchscreen or a convertible's tablet mode can need it. Whether
//! it is shown for a focused field is [`osk_wanted`], which follows the input
//! in use — a touch with no key press or pointer use since, or a
//! convertible folded into tablet mode. Keeping the service alive while it is
//! merely hidden means a tap into a field never waits for, or races, a fresh
//! keyboard process binding the input method.
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
//!
//! An idle seat — nobody owns it — can be lent to the compositor itself for a
//! single commit ([`TrustedInputMethod::Compositor`], see
//! [`super::text_commit`]): text the shell inserts on the user's behalf, such
//! as a picked emoji, reaches the focused field as an input method's commit,
//! never as synthesised key events.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use settings_store::accessibility::OnScreenKeyboard;
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
    /// The compositor's own in-process connection, which borrows an idle
    /// seat for one commit. No socket accepts this kind: only the compositor
    /// creates such a connection.
    Compositor,
}

impl TrustedInputMethod {
    fn code(self) -> u8 {
        match self {
            Self::OnScreenKeyboard => 1,
            Self::Composer => 2,
            Self::Compositor => 3,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::OnScreenKeyboard),
            2 => Some(Self::Composer),
            3 => Some(Self::Compositor),
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

/// The input facts [`osk_wanted`] reads in `auto`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OskInput {
    /// A touchscreen is attached.
    pub touchscreen: bool,
    /// The screen was touched after the most recent key press or pointer
    /// use.
    pub touch_is_current: bool,
    /// A convertible reports that its keyboard is folded away.
    pub tablet_mode: bool,
}

impl OskInput {
    pub fn from_environment(input: &super::input_environment::InputEnvironment) -> Self {
        Self {
            touchscreen: input.available().contains(ui::InputDevice::Touch),
            touch_is_current: input.touch_is_current(),
            tablet_mode: input.tablet_mode(),
        }
    }
}

/// Whether the on-screen keyboard should run and own an otherwise idle seat:
/// in `Auto`, whenever the hardware has a touchscreen or reports tablet mode,
/// whether or not it is currently shown (see [`osk_wanted`]).
pub fn osk_service_wanted(preference: OnScreenKeyboard, input: OskInput) -> bool {
    match preference {
        OnScreenKeyboard::Always => true,
        OnScreenKeyboard::Never => false,
        OnScreenKeyboard::Auto => input.tablet_mode || input.touchscreen,
    }
}

/// Whether the on-screen keyboard should be shown for a focused field.
///
/// In `Auto`, a convertible in tablet mode always gets it: its keyboard is
/// folded away. Otherwise it is shown only while touch is the input in use:
/// nothing before the first touch. A key press, pointer click, scroll, gesture
/// or pen use hides it until the screen is touched again; pointer hover does
/// not dismiss a touch-started keyboard.
pub fn osk_wanted(preference: OnScreenKeyboard, input: OskInput) -> bool {
    match preference {
        OnScreenKeyboard::Always => true,
        OnScreenKeyboard::Never => false,
        OnScreenKeyboard::Auto => {
            input.tablet_mode || (input.touchscreen && input.touch_is_current)
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
    /// credential and never can; the compositor's own connection only while
    /// nobody owns the seat.
    pub fn admits(&self, credential: Option<TrustedInputMethod>) -> bool {
        match (credential, self.get()) {
            (None, _) => false,
            (Some(TrustedInputMethod::Compositor), owner) => owner.is_none(),
            (Some(kind), owner) => Some(kind) == owner,
        }
    }

    /// Whether a connection of `kind` survives the seat moving to `owner`.
    pub fn keeps(kind: TrustedInputMethod, owner: Option<TrustedInputMethod>) -> bool {
        Some(kind) == owner || (kind == TrustedInputMethod::Compositor && owner.is_none())
    }
}

/// Where the composing input method is installed. Its private display admits
/// only a process running exactly this executable.
pub const COMPOSER_EXECUTABLE: &str = "/usr/bin/fcitx5";

/// Addons of the composer that Tilo does not use. The tray and panel addons
/// need services the session does not run, the clipboard addon claims a
/// global chord, and the IBus/Fcitx4 frontends would claim bus names nobody
/// asked for. XIM stays:
/// it serves X11 apps on the workspace displays the session connects it to.
const DISABLED_COMPOSER_ADDONS: &str = "clipboard,kimpanel,notificationitem,\
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
        use InputMethodEngine::{Hangul, Rime};
        use InputMethodOwner::{Composer, Nobody, OnScreenKeyboard};
        // (active engine, composer installed, OSK preference) → owner
        let table = [
            (None, true, false, Nobody),
            (None, true, true, OnScreenKeyboard),
            (None, false, true, OnScreenKeyboard),
            (Some(Rime), true, false, Composer(Rime)),
            (Some(Hangul), true, true, Composer(Hangul)),
            (Some(Rime), false, true, OnScreenKeyboard),
            (Some(Rime), false, false, Nobody),
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
    fn automatic_on_screen_keyboard_follows_touch_typing_and_tablet_mode() {
        use OnScreenKeyboard::{Always, Auto, Never};
        // (preference, touchscreen, touch is current, tablet mode) → wanted
        let table = [
            ((Auto, false, false, false), false),
            ((Auto, true, false, false), false),
            ((Auto, true, true, false), true),
            ((Auto, false, true, false), false),
            ((Auto, false, false, true), true),
            ((Auto, true, false, true), true),
            ((Always, false, false, false), true),
            ((Always, true, false, false), true),
            ((Never, true, true, false), false),
            ((Never, true, true, true), false),
        ];
        for ((preference, touchscreen, touch_is_current, tablet_mode), expected) in table {
            let input = OskInput {
                touchscreen,
                touch_is_current,
                tablet_mode,
            };
            assert_eq!(
                osk_wanted(preference, input),
                expected,
                "{preference:?} {input:?}"
            );
            // Whatever hides it, a keyboard that can be shown keeps running.
            if expected {
                assert!(
                    osk_service_wanted(preference, input),
                    "{preference:?} {input:?}"
                );
            }
        }

        // Nothing shows it before the first touch; a key press or pointer
        // use hides it until the next touch.
        let mut environment = super::super::input_environment::InputEnvironment::default();
        environment.device_added(
            "panel",
            ui::InputAvailability::NONE.with(ui::InputDevice::Touch),
        );
        let wanted = |environment: &super::super::input_environment::InputEnvironment| {
            osk_wanted(Auto, OskInput::from_environment(environment))
        };
        assert!(!wanted(&environment), "a field focused before any input");
        environment.note_used(ui::InputModality::Keyboard);
        assert!(!wanted(&environment), "typed before any touch");
        environment.note_used(ui::InputModality::Touch);
        assert!(wanted(&environment));
        environment.note_used(ui::InputModality::Pointer);
        assert!(!wanted(&environment), "the pointer was activated");
        environment.note_used(ui::InputModality::Touch);
        assert!(wanted(&environment));
        environment.note_used(ui::InputModality::Keyboard);
        assert!(!wanted(&environment));
        assert!(
            osk_service_wanted(Auto, OskInput::from_environment(&environment)),
            "typing hides the keyboard but keeps it running for the next touch"
        );
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
    fn only_an_idle_seat_is_lent_to_the_compositor() {
        use TrustedInputMethod::{Composer, Compositor, OnScreenKeyboard};
        let seat = SeatInputMethod::default();
        assert!(seat.admits(Some(Compositor)), "nobody owns the seat");
        assert!(SeatInputMethod::keeps(Compositor, None));
        for owner in [OnScreenKeyboard, Composer] {
            seat.set(Some(owner));
            assert!(
                !seat.admits(Some(Compositor)),
                "{owner:?} owns the seat; its commits go through it"
            );
            assert!(!SeatInputMethod::keeps(Compositor, Some(owner)));
            assert!(SeatInputMethod::keeps(owner, Some(owner)));
        }
    }

    #[test]
    fn composer_runs_the_engine_on_the_private_display_without_cloud_or_chords() {
        let launch = composer_launch(
            InputMethodEngine::Anthy,
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
        assert!(disabled.split(',').any(|addon| addon == "clipboard"));
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
        assert!(profile.contains("DefaultIM=anthy"));
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
