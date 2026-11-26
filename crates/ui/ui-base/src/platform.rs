//! Semantic platform conventions for app-authored controls and shortcuts. Apps describe intent
//! (`command + K`, primary vs cancel); the host chooses the modifier label, event modifier, and dialog
//! ordering for its platform.

/// A row of a shortcut reference, as this platform spells it: `{cmd:K}` is the command key with K
/// (Ctrl+K, or the Command key on a Mac), `{cmd+shift:Enter}` adds Shift, `{alt:Up}` is Alt with Up and
/// `{F2}` is a key on its own, each in the active keyboard's legends.
#[must_use]
pub fn spell_shortcut(template: &str) -> String {
    let conventions = PlatformConventions::default();
    let mut out = String::new();
    let mut rest = template;
    while let Some((before, after)) = rest.split_once('{') {
        out.push_str(before);
        let Some((spec, tail)) = after.split_once('}') else {
            out.push('{');
            rest = after;
            break;
        };
        out.push_str(&shortcut(spec).label(conventions));
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// The same row as [`spell_shortcut`], one keycap per key, for a reference sheet that draws its keys
/// with [`crate::widget::InputHint`]: `{cmd+shift:S}` is the command key, the shift key and S.
#[must_use]
pub fn spell_shortcut_keycaps(template: &str) -> Vec<String> {
    let conventions = PlatformConventions::default();
    let mut caps: Vec<String> = Vec::new();
    let mut plain = String::new();
    let mut rest = template;
    while let Some((before, after)) = rest.split_once('{') {
        plain.push_str(before);
        let Some((spec, tail)) = after.split_once('}') else {
            plain.push('{');
            rest = after;
            break;
        };
        push_caps(&mut caps, &mut plain, conventions);
        caps.extend(shortcut(spec).keycap_labels(conventions));
        rest = tail;
    }
    plain.push_str(rest);
    push_caps(&mut caps, &mut plain, conventions);
    caps
}

fn push_caps(caps: &mut Vec<String>, plain: &mut String, conventions: PlatformConventions) {
    let loose = std::mem::take(plain);
    if !loose.trim().is_empty() {
        caps.extend(Shortcut::key(loose.trim()).keycap_labels(conventions));
    }
}

/// The chord a template's `{…}` names: `cmd` is the platform's command modifier, `shift` and `alt` add
/// theirs.
fn shortcut(spec: &str) -> Shortcut {
    let (modifiers, key) = spec.split_once(':').unwrap_or(("", spec));
    let mut shortcut = if modifiers.contains("cmd") {
        Shortcut::command(key)
    } else {
        Shortcut::key(key)
    };
    if modifiers.contains("shift") {
        shortcut = shortcut.shift();
    }
    if modifiers.contains("alt") {
        shortcut = shortcut.alt();
    }
    shortcut
}

/// Platform family relevant to human-interface conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    Linux,
    Android,
    Other,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "android") {
            Self::Android
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

/// Visual order of a primary confirmation and its secondary/cancel action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogActionOrder {
    /// Secondary/cancel first, primary at the trailing edge (macOS/GNOME/Material).
    PrimaryLast,
    /// Primary first, secondary/cancel at the trailing edge (Windows).
    PrimaryFirst,
}

/// Host-selected conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformConventions {
    platform: Platform,
}

impl Default for PlatformConventions {
    fn default() -> Self {
        Self::new(Platform::current())
    }
}

impl PlatformConventions {
    pub const fn new(platform: Platform) -> Self {
        Self { platform }
    }

    pub const fn platform(self) -> Platform {
        self.platform
    }

    pub const fn dialog_action_order(self) -> DialogActionOrder {
        match self.platform {
            Platform::Windows => DialogActionOrder::PrimaryFirst,
            Platform::MacOs | Platform::Linux | Platform::Android | Platform::Other => {
                DialogActionOrder::PrimaryLast
            }
        }
    }

    /// The platform's primary command modifier: Command (Super) on macOS, Control elsewhere.
    pub const fn command_modifier(self) -> winit::keyboard::ModifiersState {
        match self.platform {
            Platform::MacOs => winit::keyboard::ModifiersState::SUPER,
            Platform::Windows | Platform::Linux | Platform::Android | Platform::Other => {
                winit::keyboard::ModifiersState::CONTROL
            }
        }
    }

    /// Whether the platform's primary command modifier is currently held.
    pub fn command_modifier_down(self, modifiers: winit::keyboard::ModifiersState) -> bool {
        modifiers.contains(self.command_modifier())
    }

    /// The modifiers of a shortcut sent to a web page: the platform's command modifier
    /// alone (Command on macOS, Control elsewhere), for select all, copy and the page's
    /// own keys.
    #[must_use]
    pub fn command_key_modifiers(self) -> tilo_webview::KeyModifiers {
        let command = self.command_modifier();
        tilo_webview::KeyModifiers {
            ctrl: command.control_key(),
            meta: command.super_key(),
            shift: false,
            alt: false,
        }
    }
}

/// A semantic shortcut whose display and matching use platform conventions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    key: String,
    command: bool,
    shift: bool,
    alt: bool,
    system: bool,
    /// The other chords that press the same thing, when one label names several
    /// (`Right,Space`): each presses it as the first does.
    others: Vec<Shortcut>,
}

impl Shortcut {
    pub fn command(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            command: true,
            shift: false,
            alt: false,
            system: false,
            others: Vec::new(),
        }
    }

    /// A key pressed on its own, without the command modifier: F3, Delete, or a single letter a view
    /// answers while it has focus.
    pub fn key(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            command: false,
            shift: false,
            alt: false,
            system: false,
            others: Vec::new(),
        }
    }

    /// A chord as the shell's shortcut table stores it: modifier names (`Super`, `Ctrl`, `Alt`,
    /// `Shift`) and a key, spelled for display in the active keyboard layout's legends.
    pub fn chord<'a>(modifiers: impl IntoIterator<Item = &'a str>, key: &str) -> Self {
        let mut shortcut = Self::key(crate::keycaps::active().key(key));
        for modifier in modifiers {
            match modifier.trim().to_ascii_lowercase().as_str() {
                "super" | "meta" | "logo" | "win" => shortcut.system = true,
                "ctrl" | "control" | "cmd" | "command" => shortcut.command = true,
                "alt" | "option" => shortcut.alt = true,
                "shift" => shortcut.shift = true,
                _ => {}
            }
        }
        shortcut
    }

    /// Held with the shell's own modifier (Super).
    #[must_use]
    pub fn system(mut self) -> Self {
        self.system = true;
        self
    }

    pub fn has_system(&self) -> bool {
        self.system
    }

    #[must_use]
    pub fn shift(mut self) -> Self {
        self.shift = true;
        self
    }

    #[must_use]
    pub fn alt(mut self) -> Self {
        self.alt = true;
        self
    }

    /// The key this shortcut is pressed with, as a keycap legend.
    pub fn key_name(&self) -> &str {
        &self.key
    }

    pub fn has_command(&self) -> bool {
        self.command
    }

    pub fn has_shift(&self) -> bool {
        self.shift
    }

    pub fn has_alt(&self) -> bool {
        self.alt
    }

    pub fn label(&self, conventions: PlatformConventions) -> String {
        let parts = self.keycap_labels(conventions);
        if conventions.platform() == Platform::MacOs {
            parts.concat()
        } else {
            parts.join("+")
        }
    }

    /// Labels to paint as individual physical-looking caps, in the legends of the active keyboard
    /// layout ([`crate::keycaps::active`]).
    pub fn keycap_labels(&self, conventions: PlatformConventions) -> Vec<String> {
        use crate::input::KeyboardModifier;
        let keycaps = crate::keycaps::active();
        let keycaps = if conventions.platform() == Platform::MacOs {
            keycaps.on_family(crate::interaction::KeyboardFamily::Apple)
        } else {
            keycaps
        };
        [
            (self.system, KeyboardModifier::System),
            (self.command, KeyboardModifier::Command),
            (self.alt, KeyboardModifier::Alt),
            (self.shift, KeyboardModifier::Shift),
        ]
        .into_iter()
        .filter(|(held, _)| *held)
        .map(|(_, modifier)| keycaps.modifier(modifier).to_owned())
        .chain(std::iter::once(keycaps.key(&self.key)))
        .collect()
    }

    /// Whether the press of `key` with `modifiers` asks for this shortcut: the same key, with exactly
    /// the modifiers it names.
    #[must_use]
    pub fn matches(
        &self,
        key: &winit::keyboard::Key,
        modifiers: winit::keyboard::ModifiersState,
        conventions: PlatformConventions,
    ) -> bool {
        Self::pressed(key, modifiers, conventions).is_some_and(|pressed| self.same_press(&pressed))
    }

    /// The shortcut a live press asks for, in `conventions`' spelling, or `None` for a key no shortcut
    /// can name.
    #[must_use]
    pub fn pressed(
        key: &winit::keyboard::Key,
        modifiers: winit::keyboard::ModifiersState,
        conventions: PlatformConventions,
    ) -> Option<Self> {
        let key = crate::keys::token(key)?;
        let command = conventions.command_modifier_down(modifiers);
        Some(Self {
            key,
            command,
            shift: modifiers.shift_key(),
            alt: modifiers.alt_key(),
            // On a Mac the command modifier is Super, so it is named once.
            system: modifiers.super_key() && !command,
            others: Vec::new(),
        })
    }

    /// Whether both ask for one press: one key, in one spelling, with the same modifiers. A label
    /// that names several chords matches a press of any of them (`Right,Space`).
    #[must_use]
    pub fn same_press(&self, other: &Self) -> bool {
        self.same_one(other) || self.others.iter().any(|also| also.same_one(other))
    }

    /// Whether this chord is exactly `other`.
    fn same_one(&self, other: &Self) -> bool {
        crate::keys::canonical(&self.key) == crate::keys::canonical(&other.key)
            && self.command == other.command
            && self.shift == other.shift
            && self.alt == other.alt
            && self.system == other.system
    }
}

/// A host service every app answers by shortcut, whichever chrome shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostShortcut {
    Search,
    History,
    Undo,
    Redo,
    /// Show or hide the window's sidebar (`NavShell`), in every app.
    Sidebar,
}

impl HostShortcut {
    /// The one shortcut table. Plain Ctrl+H stays with apps (Find and Replace), so History takes Shift
    /// where ⌘Y is not the convention.
    pub fn shortcut(self, conventions: PlatformConventions) -> Shortcut {
        let mac = conventions.platform() == Platform::MacOs;
        match self {
            Self::Search => Shortcut::command("K"),
            Self::History if mac => Shortcut::command("Y"),
            Self::History => Shortcut::command("H").shift(),
            Self::Undo => Shortcut::command("Z"),
            Self::Redo if conventions.platform() == Platform::Windows => Shortcut::command("Y"),
            Self::Redo => Shortcut::command("Z").shift(),
            Self::Sidebar => Shortcut::command("L").shift(),
        }
    }

    /// The host service an action is, by the action's id: a row that lists one of the toolkit's own
    /// commands shows the chord pressing it means, read from the same table the key press resolves
    /// through.
    #[must_use]
    pub fn of_action(id: &str) -> Option<Self> {
        match id {
            "undo" => Some(Self::Undo),
            "redo" => Some(Self::Redo),
            "history" => Some(Self::History),
            _ => None,
        }
    }

    /// The picture a host service is drawn with where a row lists it, beside the chord in the same
    /// table: an undo is an arrow turning back, whichever app lists it.
    #[must_use]
    pub fn icon(self) -> crate::icons::Icon {
        use crate::icons::Icon;
        match self {
            Self::Search => Icon::Search,
            Self::History => Icon::History,
            Self::Undo => Icon::Undo2,
            Self::Redo => Icon::Redo2,
            Self::Sidebar => Icon::PanelLeft,
        }
    }

    /// Which host service a command-modified character means.
    pub fn resolve(conventions: PlatformConventions, character: &str, shift: bool) -> Option<Self> {
        let mac = conventions.platform() == Platform::MacOs;
        match (character.to_ascii_lowercase().as_str(), shift) {
            ("k", false) => Some(Self::Search),
            ("y", false) if mac => Some(Self::History),
            ("h", true) if !mac => Some(Self::History),
            ("z", false) => Some(Self::Undo),
            ("z", true) | ("y", false) => Some(Self::Redo),
            ("l", true) => Some(Self::Sidebar),
            _ => None,
        }
    }
}

/// Best-effort and non-blocking — a launch failure is logged, not surfaced.
pub fn os_open(url: &str) {
    crate::portal::open_uri_detached(url);
}

/// Hand a URL to the current host's URI handler and report the outcome.
/// May block until the handler answers; call it from a worker thread.
///
/// # Errors
/// See [`crate::portal::open_uri`].
pub fn try_os_open(url: &str) -> std::io::Result<()> {
    crate::portal::open_uri(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn shortcuts_are_labelled_and_listed_by_each_platforms_conventions;

        phase a_shell_chord_shows_its_super_key_first;
        // A shell chord from the shortcut table (Super+F) reads with its Super key, ahead of the others,
        // in the standard menu spelling.
        {
            let _us = crate::keycaps::scoped(crate::keycaps::Keycaps::US);
            let linux = PlatformConventions::new(Platform::Linux);
            assert_eq!(Shortcut::chord(["Super"], "f").label(linux), "Super+F");
            assert_eq!(
                Shortcut::chord(["Shift", "Super"], "pageup").label(linux),
                "Super+Shift+Page Up"
            );
            assert!(Shortcut::from("Super+W").has_system());
            assert!(!Shortcut::command("W").has_system());
        }

        phase host_shortcuts_resolve_to_the_label_each_platform_shows;
        {
            for platform in [Platform::MacOs, Platform::Windows, Platform::Linux] {
                let conventions = PlatformConventions::new(platform);
                for service in [
                    HostShortcut::Search,
                    HostShortcut::History,
                    HostShortcut::Undo,
                    HostShortcut::Redo,
                    HostShortcut::Sidebar,
                ] {
                    let shortcut = service.shortcut(conventions);
                    assert_eq!(
                        HostShortcut::resolve(conventions, shortcut.key_name(), shortcut.has_shift()),
                        Some(service),
                        "{platform:?} {service:?}"
                    );
                }
            }
            let mac = PlatformConventions::new(Platform::MacOs);
            let linux = PlatformConventions::new(Platform::Linux);
            assert_eq!(HostShortcut::History.shortcut(mac).label(mac), "⌘Y");
            assert_eq!(
                HostShortcut::Sidebar.shortcut(linux).label(linux),
                "Ctrl+Shift+L"
            );
            assert_eq!(
                HostShortcut::History.shortcut(linux).label(linux),
                "Ctrl+Shift+H"
            );
            assert_eq!(
                HostShortcut::resolve(linux, "y", false),
                Some(HostShortcut::Redo)
            );
            assert_eq!(
                HostShortcut::resolve(linux, "h", false),
                None,
                "Ctrl+H stays with apps"
            );
            assert_eq!(
                HostShortcut::resolve(mac, "y", false),
                Some(HostShortcut::History)
            );
        }

        phase a_shortcut_row_is_one_keycap_per_key;
        // A reference sheet paints one drawn keycap per key: a chord is its modifiers and its key, never
        // one cap holding a joined string.
        {
            let _us = crate::keycaps::scoped(crate::keycaps::Keycaps::US);
            let conventions = PlatformConventions::default();
            assert_eq!(
                spell_shortcut_keycaps("{cmd+shift:S}"),
                Shortcut::command("S").shift().keycap_labels(conventions)
            );
            assert_eq!(
                spell_shortcut_keycaps("{cmd:X}"),
                Shortcut::command("X").keycap_labels(conventions)
            );
            assert_eq!(
                spell_shortcut_keycaps("Page Up"),
                ["Page Up"],
                "a plain key name is one cap"
            );
        }

        phase the_toolkits_own_actions_are_listed_on_their_own_chords;
        {
            let linux = PlatformConventions::new(Platform::Linux);
            for (id, service) in [
                ("undo", HostShortcut::Undo),
                ("redo", HostShortcut::Redo),
                ("history", HostShortcut::History),
            ] {
                let shortcut = HostShortcut::of_action(id).expect(id);
                assert_eq!(shortcut, service);
                assert_eq!(shortcut.shortcut(linux), service.shortcut(linux));
            }
            assert_eq!(
                HostShortcut::of_action("rename").map(|it| it.shortcut(linux)),
                None,
                "an app's own action declares its own shortcut"
            );
        }

        phase shortcuts_modifiers_and_dialog_order_follow_each_platform_hig;
        {
            {
                let shortcut = Shortcut::command("K").shift();
                assert_eq!(
                    shortcut.label(PlatformConventions::new(Platform::MacOs)),
                    "⌘⇧K"
                );
                assert_eq!(
                    shortcut.label(PlatformConventions::new(Platform::Windows)),
                    "Ctrl+Shift+K"
                );
                assert_eq!(
                    shortcut.label(PlatformConventions::new(Platform::Linux)),
                    "Ctrl+Shift+K"
                );
                assert_eq!(
                    shortcut.label(PlatformConventions::new(Platform::Android)),
                    "Ctrl+Shift+K"
                );
            }

            {
                let control = winit::keyboard::ModifiersState::CONTROL;
                let command = winit::keyboard::ModifiersState::SUPER;
                let mac = PlatformConventions::new(Platform::MacOs);
                let windows = PlatformConventions::new(Platform::Windows);
                assert!(mac.command_modifier_down(command));
                assert!(!mac.command_modifier_down(control));
                assert!(windows.command_modifier_down(control));
                assert!(!windows.command_modifier_down(command));
                assert_eq!(mac.dialog_action_order(), DialogActionOrder::PrimaryLast);
                assert_eq!(
                    windows.dialog_action_order(),
                    DialogActionOrder::PrimaryFirst
                );
                let android = PlatformConventions::new(Platform::Android);
                assert!(android.command_modifier_down(control));
                assert!(!android.command_modifier_down(command));
                assert_eq!(
                    android.dialog_action_order(),
                    DialogActionOrder::PrimaryLast
                );
            }
        }
    }

    /// A page's own shortcut goes with the platform's command modifier: Command on macOS,
    /// Control everywhere else, never both.
    #[test]
    fn a_pages_shortcut_carries_the_platforms_command_modifier() {
        let mac = PlatformConventions::new(Platform::MacOs).command_key_modifiers();
        assert!(mac.meta && !mac.ctrl && !mac.shift && !mac.alt);
        for platform in [Platform::Linux, Platform::Windows, Platform::Android] {
            let other = PlatformConventions::new(platform).command_key_modifiers();
            assert!(other.ctrl && !other.meta && !other.shift && !other.alt);
        }
    }
}

/// A chord as an app, a menu or the person's table writes it (`Ctrl+Enter`, `Super+W`, `F2`, `e`): one
/// table for the modifiers and the key names it parses with ([`tilo_ui_graph::keys::Chord`]), the same
/// one the compiler checks an app's declared shortcut against.
impl From<&str> for Shortcut {
    /// One chord, or several a control answers alike, separated by commas (`Right,Space`): the
    /// first is the one its keys show.
    fn from(label: &str) -> Self {
        let mut chords = label
            .split(',')
            .map(str::trim)
            .filter(|chord| !chord.is_empty());
        let mut shortcut = Self::one(chords.next().unwrap_or(""));
        shortcut.others = chords.map(Self::one).collect();
        shortcut
    }
}

impl Shortcut {
    /// The one chord `label` names.
    fn one(label: &str) -> Self {
        let Ok(chord) = tilo_ui_graph::keys::Chord::parse(label) else {
            return Self::key(label.trim());
        };
        Self {
            key: chord.key,
            command: chord.command,
            shift: chord.shift,
            alt: chord.alt,
            system: chord.system,
            others: Vec::new(),
        }
    }
}

impl From<String> for Shortcut {
    fn from(label: String) -> Self {
        Self::from(label.as_str())
    }
}

#[cfg(test)]
mod more_keys_tests {
    use super::*;

    /// A label naming several keys presses the same thing as any of them, and shows the first:
    /// a presentation steps on Right and on Space and names the pair once.
    #[test]
    fn a_label_naming_two_keys_matches_either_press() {
        let both = Shortcut::from("Right,Space");
        assert!(both.same_press(&Shortcut::key("Right")), "Right presses it");
        assert!(both.same_press(&Shortcut::key("Space")), "Space presses it");
        assert!(!both.same_press(&Shortcut::key("Left")), "Left does not");
        assert!(
            !both.same_press(&Shortcut::command("Space")),
            "a modifier the label does not name is another press"
        );
        assert_eq!(
            both.label(PlatformConventions::default()),
            Shortcut::from("Right").label(PlatformConventions::default()),
            "its keys show the first chord"
        );
    }
}

/// Public because it is half of a protocol: what a pointer published in this
/// surface's content coordinates is scoped to
/// ([`crate::presence::PointerSpace::Surface`]) — both ends must spell the
/// surface the same way by calling this same function.
pub fn local_surface_id() -> String {
    let app_id = crate::process::environment().identity.app_id.clone();
    let binary = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    resolve_app_id(app_id.as_deref(), binary.as_deref())
}

pub fn resolve_app_id(package_id: Option<&str>, binary: Option<&str>) -> String {
    let non_blank = |s: &str| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    if let Some(id) = package_id.and_then(non_blank) {
        return id;
    }
    if let Some(bin) = binary.and_then(non_blank) {
        return bin;
    }
    "org.tilo.app".to_string()
}

/// What the Android activity this process runs hands out: the APK's assets.
#[cfg(all(target_os = "android", feature = "native"))]
pub mod android {
    use std::ffi::CString;
    use std::sync::OnceLock;

    use winit::platform::android::activity::AndroidApp;

    pub(crate) static APP: OnceLock<AndroidApp> = OnceLock::new();

    /// The activity this process runs, once `android_main` has started.
    pub fn app() -> Option<&'static AndroidApp> {
        APP.get()
    }

    /// The APK's asset `name` (`tilo/manifest.toml`), read: what an app reads of itself from the APK
    /// it was installed from, which no file of the app's own can stand in for.
    ///
    /// # Errors
    /// No activity runs yet, or the APK has no such asset.
    pub fn read_asset(name: &str) -> std::io::Result<Vec<u8>> {
        let app = app().ok_or_else(|| std::io::Error::other("no Android activity"))?;
        let c_name = CString::new(name).map_err(std::io::Error::other)?;
        let mut asset = app.asset_manager().open(&c_name).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "missing from the APK")
        })?;
        let mut bytes = Vec::new();
        std::io::copy(&mut asset, &mut bytes)?;
        Ok(bytes)
    }

    /// Copy the APK's asset `name` (`tilo/fonts/manifest.toml`) to `to`, making its directory: what
    /// an app copies out of its APK on first start, since Android hands assets out as streams only.
    ///
    /// # Errors
    /// No activity runs yet, the APK has no such asset, or `to` cannot be written.
    pub fn copy_asset(name: &str, to: &std::path::Path) -> std::io::Result<()> {
        use std::io::Write as _;
        let app = app().ok_or_else(|| std::io::Error::other("no Android activity"))?;
        let c_name = CString::new(name).map_err(std::io::Error::other)?;
        let mut asset = app.asset_manager().open(&c_name).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "missing from the APK")
        })?;
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(to)?;
        std::io::copy(&mut asset, &mut file)?;
        file.flush()
    }
}
