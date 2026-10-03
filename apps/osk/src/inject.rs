//! Text-injection backends for the on-screen keyboard (client half).
//!
//! The widget ([`crate::OskWidget`]) reduces input into an ordered
//! [`crate::OskOutput`] stream; [`crate::output`] turns that stream into
//! keysyms where keysyms are needed. This module presses them in whatever
//! surface currently holds keyboard focus, so the OSK behaves like a real
//! keyboard for any app.
//!
//! ## Backend: `virtual-keyboard-v1` (wtype-style)
//!
//! [`VirtualKeyboardInjector`] opens a **second, independent** `wayland-client`
//! connection (winit, behind `ui::App`, owns the first one — running two
//! connections in one process is fine), binds a `wl_seat` and the
//! `zwp_virtual_keyboard_manager_v1` global, and creates a virtual keyboard.
//!
//! Injection mirrors [`wtype`](https://github.com/atx/wtype): rather than
//! reusing the host's keymap (whose layout we don't know), we *synthesise* a
//! throwaway XKB keymap on the fly that maps every symbol we need to send to
//! its own dedicated keycode. Printable characters become Unicode keysyms
//! (`U0041` for `A`, `U20AC` for `€`, …); the special editing keys map to the
//! named XKB keysyms `BackSpace`, `Return`, `space`, `Tab`. We write that
//! keymap to a `memfd`, hand the fd to the compositor via `keymap(xkb_v1, …)`,
//! then for each symbol emit `key(press)` + `key(release)` and flush. Because
//! each symbol owns its keycode and the keysym already encodes the exact
//! character, no modifier juggling is needed — we always send modifiers = 0.
//!
//! ## What is and isn't unit-tested
//!
//! The pure logic — keymap-string generation, [`KeyAction`] → keysym mapping,
//! and the [`NullInjector`] recorder — is unit-tested in this file (see the
//! `tests` module). The live Wayland path (binding globals, creating the
//! virtual keyboard, sending key events) **cannot run headlessly**: it needs a
//! compositor that implements `virtual-keyboard-v1`. It is exercised here only
//! at the type/compile level behind the [`TextInjector`] trait.
//!
//! ### Manual verification
//!
//! Run the OSK under a compositor that supports `virtual-keyboard-v1` —
//! `sway`/`wlroots`, `KWin`, `Hyprland`, `river`, … (GNOME/Mutter does **not**
//! expose it). Focus a text field in another window, then tap/swipe keys on the
//! OSK; the characters should appear in the focused field. If the protocol is
//! absent, [`default_injector`] silently degrades to [`NullInjector`] and the
//! OSK still runs (no injection).

use std::fmt::Write as _;

/// A sink for committed keyboard input. Implementors push text / key actions
/// into the focused surface (real backend) or record them (test backend).
pub trait TextInjector {
    /// Inject a literal string, character by character. Equivalent to typing
    /// each grapheme; the backend is responsible for newlines, spaces, etc.
    fn commit_text(&mut self, text: &str);

    /// Press each symbol in order.
    fn send(&mut self, symbols: &[Symbol]);

    /// Wait until the compositor has processed every key sent so far. Needed
    /// before a request on another connection that must land after them.
    fn sync(&mut self) {}

    /// Short, stable identifier for the active backend (`"virtual-keyboard-v1"`
    /// or `"null"`). Used in logs and tests.
    fn backend_name(&self) -> &'static str;
}

/// One injectable symbol: either a literal Unicode scalar (most keys) or one of
/// the named special editing keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Symbol {
    /// A printable Unicode character (letters, digits, punctuation, emoji).
    Unicode(char),
    BackSpace,
    Return,
    Space,
    Tab,
    /// Move the caret one character left (the space-bar cursor drag).
    Left,
    /// Move the caret one character right.
    Right,
}

impl Symbol {
    /// Map one source character to a [`Symbol`]. `'\n'`/`'\r'` → [`Return`],
    /// `'\t'` → [`Tab`], `' '` → [`Space`], `'\u{8}'` (BS) → [`BackSpace`];
    /// everything else is a literal [`Unicode`].
    ///
    /// [`Return`]: Symbol::Return
    /// [`Tab`]: Symbol::Tab
    /// [`Space`]: Symbol::Space
    /// [`BackSpace`]: Symbol::BackSpace
    /// [`Unicode`]: Symbol::Unicode
    pub fn from_char(c: char) -> Self {
        match c {
            '\n' | '\r' => Symbol::Return,
            '\t' => Symbol::Tab,
            ' ' => Symbol::Space,
            '\u{8}' => Symbol::BackSpace,
            other => Symbol::Unicode(other),
        }
    }

    /// The XKB keysym name for this symbol, as it appears on the right-hand
    /// side of a `key <CODE> { [ NAME ] };` line in a generated keymap.
    ///
    /// Printable characters use the Unicode keysym form `UXXXX` (uppercase hex,
    /// at least 4 digits) understood by libxkbcommon and X11; the specials use
    /// their canonical keysym names.
    pub fn keysym_name(self) -> String {
        match self {
            Symbol::BackSpace => "BackSpace".to_string(),
            Symbol::Return => "Return".to_string(),
            Symbol::Space => "space".to_string(),
            Symbol::Tab => "Tab".to_string(),
            Symbol::Left => "Left".to_string(),
            Symbol::Right => "Right".to_string(),
            Symbol::Unicode(c) => format!("U{:04X}", c as u32),
        }
    }
}

/// Build the throwaway XKB keymap string (wtype-style) that assigns each symbol
/// in `symbols` its own keycode, deduplicating repeats so a repeated character
/// reuses one keycode.
///
/// Returns the keymap text plus the keycode assigned to each *distinct* symbol,
/// in first-seen order. Keycodes start at [`KEYCODE_BASE`] (the XKB convention
/// of `evdev keycode + 8`) and increase by one per distinct symbol.
///
/// The produced keymap is a complete, self-contained `xkb_keymap { … }` with a
/// `default_partial`-free explicit `xkb_symbols` section, so any compositor can
/// `mmap` and compile it without needing `xkbcommon` rules on disk.
pub fn build_keymap(symbols: &[Symbol]) -> (String, Vec<(Symbol, u32)>) {
    // Deduplicate, preserving first-seen order.
    let mut assigned: Vec<(Symbol, u32)> = Vec::new();
    for &sym in symbols {
        if !assigned.iter().any(|(s, _)| *s == sym) {
            let code = KEYCODE_BASE + assigned.len() as u32;
            assigned.push((sym, code));
        }
    }

    // The highest keycode we declare. The keycodes range must cover every code
    // we emit; we always declare at least one so the keymap is well-formed.
    let max_code = assigned
        .iter()
        .map(|(_, c)| *c)
        .max()
        .unwrap_or(KEYCODE_BASE);

    let mut keymap = String::new();
    keymap.push_str("xkb_keymap {\n");

    // --- keycodes: name every code we use; <I N> = code N. ---
    keymap.push_str("xkb_keycodes \"tilo-osk\" {\n");
    // `write!` into a `String` is infallible; the `Result` is intentionally
    // discarded rather than `unwrap()`-ed.
    let _ = writeln!(keymap, "minimum = {KEYCODE_BASE};");
    let _ = writeln!(keymap, "maximum = {};", max_code.max(KEYCODE_BASE));
    for (_, code) in &assigned {
        let _ = writeln!(keymap, "<I{code}> = {code};");
    }
    keymap.push_str("};\n");

    // --- types / compat: pull in the stock complete definitions. ---
    keymap.push_str("xkb_types \"tilo-osk\" { include \"complete\" };\n");
    keymap.push_str("xkb_compat \"tilo-osk\" { include \"complete\" };\n");

    // --- symbols: one single-level key per code → exactly one keysym. ---
    keymap.push_str("xkb_symbols \"tilo-osk\" {\n");
    for (sym, code) in &assigned {
        let _ = writeln!(keymap, "key <I{code}> {{ [ {} ] }};", sym.keysym_name());
    }
    keymap.push_str("};\n");

    keymap.push_str("};\n");

    (keymap, assigned)
}

/// XKB keycode base: evdev keycodes are offset by 8 in the X11/XKB world, so
/// the first usable code is 8. We start our synthetic codes here.
pub const KEYCODE_BASE: u32 = 8;

/// A [`TextInjector`] that records everything it's asked to inject, for tests
/// and headless runs. Never touches Wayland.
#[derive(Default, Debug)]
pub struct NullInjector {
    /// Strings passed to [`commit_text`](TextInjector::commit_text).
    pub committed: Vec<String>,
    /// Keysym names passed to [`send`](TextInjector::send), in order.
    pub keys: Vec<String>,
}

impl NullInjector {
    pub fn new() -> Self {
        Self::default()
    }
}

impl TextInjector for NullInjector {
    fn commit_text(&mut self, text: &str) {
        self.committed.push(text.to_string());
    }

    fn send(&mut self, symbols: &[Symbol]) {
        self.keys
            .extend(symbols.iter().map(|sym| sym.keysym_name()));
    }

    fn backend_name(&self) -> &'static str {
        "null"
    }
}

/// Try the live `virtual-keyboard-v1` backend; fall back to a [`NullInjector`]
/// if the compositor lacks the protocol. Never panics — the OSK must keep
/// running even with no injection path.
pub fn default_injector() -> Box<dyn TextInjector> {
    match wayland::VirtualKeyboardInjector::new() {
        Some(vk) => Box::new(vk),
        None => Box::new(NullInjector::new()),
    }
}

pub use wayland::VirtualKeyboardInjector;

/// Live Wayland backend. Kept in a submodule so the protocol plumbing is in one
/// place; the public type is re-exported above.
mod wayland {
    #[cfg(not(target_os = "macos"))]
    use std::os::fd::AsFd;

    #[cfg(not(target_os = "macos"))]
    use wayland_client::globals::registry_queue_init;
    use wayland_client::{
        Connection, Dispatch, QueueHandle,
        globals::GlobalListContents,
        protocol::{wl_registry, wl_seat},
    };
    use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
        zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
    };

    use super::{Symbol, TextInjector, build_keymap};

    /// `wl_keyboard.key_state` value for a press / release.
    const KEY_PRESSED: u32 = 1;
    const KEY_RELEASED: u32 = 0;
    /// `wl_keyboard.keymap_format` XKB-v1 text format (this protocol declares
    /// the `keymap` arg as a bare `uint` rather than importing the enum).
    #[cfg(not(target_os = "macos"))]
    const KEYMAP_FORMAT_XKB_V1: u32 = 1;

    /// Injects text into the focused surface via a synthetic virtual keyboard.
    ///
    /// Holds its own Wayland connection + event queue, independent of winit's.
    pub struct VirtualKeyboardInjector {
        conn: Connection,
        queue: wayland_client::EventQueue<State>,
        state: State,
        keyboard: ZwpVirtualKeyboardV1,
        /// Monotonic millisecond clock for `key` event timestamps.
        time: u32,
        /// Keysyms currently described by the keyboard's active keymap, so we
        /// only re-upload a keymap when a new symbol appears.
        current_keymap: Vec<(Symbol, u32)>,
    }

    /// Dispatch sink. We don't react to any events (no keymap/modifier
    /// round-trips are required to *send* keys), so this stays empty.
    struct State;

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<wl_seat::WlSeat, ()> for State {
        fn event(
            _: &mut Self,
            _: &wl_seat::WlSeat,
            _: wl_seat::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for State {
        fn event(
            _: &mut Self,
            _: &ZwpVirtualKeyboardManagerV1,
            _: <ZwpVirtualKeyboardManagerV1 as wayland_client::Proxy>::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ZwpVirtualKeyboardV1, ()> for State {
        fn event(
            _: &mut Self,
            _: &ZwpVirtualKeyboardV1,
            _: <ZwpVirtualKeyboardV1 as wayland_client::Proxy>::Event,
            (): &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl VirtualKeyboardInjector {
        /// Connect to the compositor and create a virtual keyboard. Returns
        /// `None` (so the caller degrades to `NullInjector`) if there is no
        /// Wayland display, no `wl_seat`, or the compositor doesn't advertise
        /// `zwp_virtual_keyboard_manager_v1`. Never panics.
        #[cfg(target_os = "macos")]
        pub fn new() -> Option<Self> {
            None
        }

        #[cfg(not(target_os = "macos"))]
        pub fn new() -> Option<Self> {
            let conn = Connection::connect_to_env().ok()?;
            let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
            let qh = queue.handle();

            // The manager is the protocol that may be missing — its absence is
            // the expected "degrade to null" path.
            let manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).ok()?;
            // A seat is required to create the keyboard.
            let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=8, ()).ok()?;

            let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());

            let mut state = State;
            // Flush the creation requests.
            queue.roundtrip(&mut state).ok()?;

            Some(Self {
                conn,
                queue,
                state,
                keyboard,
                time: 0,
                current_keymap: Vec::new(),
            })
        }

        /// Ensure the active keymap can express every symbol in `needed`,
        /// re-uploading a fresh keymap (via a `memfd`) if any is missing.
        /// Returns the keycode assigned to each requested symbol, in order.
        fn ensure_keymap(&mut self, needed: &[Symbol]) -> Option<Vec<u32>> {
            let all_present = needed
                .iter()
                .all(|s| self.current_keymap.iter().any(|(sym, _)| sym == s));
            if !all_present || self.current_keymap.is_empty() {
                let (keymap_str, assigned) = build_keymap(needed);
                self.upload_keymap(&keymap_str)?;
                self.current_keymap = assigned;
            }
            // Map each requested symbol to its keycode in the active keymap.
            needed
                .iter()
                .map(|s| {
                    self.current_keymap
                        .iter()
                        .find(|(sym, _)| sym == s)
                        .map(|(_, code)| *code)
                })
                .collect()
        }

        /// Write `keymap_str` to an anonymous, sealed-ish memfd and hand it to
        /// the compositor via `keymap(xkb_v1, fd, size)`.
        #[cfg(target_os = "macos")]
        #[allow(
            clippy::unused_self,
            reason = "same platform-specific backend method signature"
        )]
        fn upload_keymap(&self, _keymap_str: &str) -> Option<()> {
            None
        }

        #[cfg(not(target_os = "macos"))]
        fn upload_keymap(&self, keymap_str: &str) -> Option<()> {
            use std::io::{Seek, SeekFrom, Write};

            let name = c"tilo-osk-keymap";
            // SAFETY: `memfd_create` with a valid C string and a documented
            // flag set; the returned fd is owned by `File` below.
            let raw = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
            if raw < 0 {
                return None;
            }
            // Take ownership so the fd is closed on drop / error.
            let mut file = unsafe {
                use std::os::fd::FromRawFd;
                std::fs::File::from_raw_fd(raw)
            };
            let bytes = keymap_str.as_bytes();
            file.write_all(bytes).ok()?;
            // The compositor mmaps `size` bytes; many implementations expect
            // the trailing NUL to be counted, so include it.
            file.write_all(&[0u8]).ok()?;
            file.flush().ok()?;
            file.seek(SeekFrom::Start(0)).ok()?;

            let size = (bytes.len() + 1) as u32;
            self.keyboard
                .keymap(KEYMAP_FORMAT_XKB_V1, file.as_fd(), size);
            // `file` (and its fd) is dropped here; the compositor has already
            // dup'd it during the request marshalling.
            self.flush()
        }

        /// Press then release a single keycode with modifiers cleared.
        fn tap(&mut self, keycode: u32) {
            // Clear modifiers — each keysym is encoded directly, no shift etc.
            self.keyboard.modifiers(0, 0, 0, 0);
            self.keyboard.key(self.time, keycode, KEY_PRESSED);
            self.time = self.time.wrapping_add(1);
            self.keyboard.key(self.time, keycode, KEY_RELEASED);
            self.time = self.time.wrapping_add(1);
        }

        /// Flush queued requests to the compositor.
        fn flush(&self) -> Option<()> {
            self.queue.flush().ok()?;
            // A blocking dispatch isn't needed to *send*; a flush suffices.
            let _ = self.conn;
            Some(())
        }

        /// Inject an ordered list of symbols: (re)upload a keymap covering them,
        /// then tap each in turn and flush.
        fn inject_symbols(&mut self, symbols: &[Symbol]) {
            if symbols.is_empty() {
                return;
            }
            let Some(codes) = self.ensure_keymap(symbols) else {
                return;
            };
            for code in codes {
                self.tap(code);
            }
            let _ = self.flush();
            // Keep the (otherwise unused-in-tests) state field live so the
            // event queue stays attached to it.
            let _ = &mut self.state;
        }
    }

    impl TextInjector for VirtualKeyboardInjector {
        fn commit_text(&mut self, text: &str) {
            let symbols: Vec<Symbol> = text.chars().map(Symbol::from_char).collect();
            self.inject_symbols(&symbols);
        }

        fn send(&mut self, symbols: &[Symbol]) {
            self.inject_symbols(symbols);
        }

        fn sync(&mut self) {
            let _ = self.queue.roundtrip(&mut self.state);
        }

        fn backend_name(&self) -> &'static str {
            "virtual-keyboard-v1"
        }
    }

    impl Drop for VirtualKeyboardInjector {
        fn drop(&mut self) {
            self.keyboard.destroy();
            let _ = self.queue.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysyms_map_specials_and_names() {
        eprintln!("phase symbol_from_char_maps_specials");
        {
            assert_eq!(Symbol::from_char('\n'), Symbol::Return);
            assert_eq!(Symbol::from_char('\r'), Symbol::Return);
            assert_eq!(Symbol::from_char('\t'), Symbol::Tab);
            assert_eq!(Symbol::from_char(' '), Symbol::Space);
            assert_eq!(Symbol::from_char('\u{8}'), Symbol::BackSpace);
            assert_eq!(Symbol::from_char('a'), Symbol::Unicode('a'));
            assert_eq!(Symbol::from_char('€'), Symbol::Unicode('€'));
        }

        eprintln!("phase keysym_names_are_correct");
        {
            // ASCII 'A' = U+0041.
            assert_eq!(Symbol::Unicode('A').keysym_name(), "U0041");
            // lowercase 'a' = U+0061.
            assert_eq!(Symbol::Unicode('a').keysym_name(), "U0061");
            // Euro sign U+20AC — five hex digits round to the natural width.
            assert_eq!(Symbol::Unicode('€').keysym_name(), "U20AC");
            // An emoji above the BMP keeps full width.
            assert_eq!(Symbol::Unicode('😀').keysym_name(), "U1F600");
            assert_eq!(Symbol::BackSpace.keysym_name(), "BackSpace");
            assert_eq!(Symbol::Return.keysym_name(), "Return");
            assert_eq!(Symbol::Space.keysym_name(), "space");
            assert_eq!(Symbol::Tab.keysym_name(), "Tab");
        }
    }

    #[test]
    fn build_keymap_emits_deduplicated_compilable_keysyms() {
        eprintln!("phase build_keymap_emits_keysym_lines_and_codes");
        {
            let symbols = vec![
                Symbol::Unicode('h'),
                Symbol::Unicode('i'),
                Symbol::Space,
                Symbol::BackSpace,
                Symbol::Return,
            ];
            let (keymap, assigned) = build_keymap(&symbols);

            // Well-formed wrapper.
            assert!(keymap.starts_with("xkb_keymap {"));
            assert!(keymap.trim_end().ends_with("};"));
            assert!(keymap.contains("xkb_keycodes"));
            assert!(keymap.contains("xkb_symbols"));
            assert!(keymap.contains("include \"complete\""));

            // Codes start at the evdev+8 base and increment per distinct symbol.
            assert_eq!(assigned.len(), 5);
            assert_eq!(assigned[0].1, KEYCODE_BASE);
            assert_eq!(assigned[4].1, KEYCODE_BASE + 4);

            // Each symbol gets a keycode declaration and a symbols line with the
            // right keysym name.
            assert!(keymap.contains(&format!("<I{KEYCODE_BASE}> = {KEYCODE_BASE}")));
            assert!(keymap.contains(&format!("key <I{KEYCODE_BASE}> {{ [ U0068 ] }}")));
            assert!(keymap.contains(&format!("key <I{}> {{ [ U0069 ] }}", KEYCODE_BASE + 1)));
            assert!(keymap.contains(&format!("key <I{}> {{ [ space ] }}", KEYCODE_BASE + 2)));
            assert!(keymap.contains(&format!("key <I{}> {{ [ BackSpace ] }}", KEYCODE_BASE + 3)));
            assert!(keymap.contains(&format!("key <I{}> {{ [ Return ] }}", KEYCODE_BASE + 4)));
        }

        eprintln!("phase build_keymap_deduplicates_repeats");
        {
            // "aa" should produce ONE distinct keycode for 'a'.
            let (_, assigned) = build_keymap(&[Symbol::Unicode('a'), Symbol::Unicode('a')]);
            assert_eq!(assigned.len(), 1);
            assert_eq!(assigned[0], (Symbol::Unicode('a'), KEYCODE_BASE));
        }

        eprintln!("phase build_keymap_handles_empty_input");
        {
            // No symbols → still a well-formed, compilable keymap (no key lines).
            let (keymap, assigned) = build_keymap(&[]);
            assert!(assigned.is_empty());
            assert!(keymap.contains("xkb_keycodes"));
            assert!(keymap.contains("xkb_symbols"));
            // Range is well-formed even with nothing declared.
            assert!(keymap.contains(&format!("minimum = {KEYCODE_BASE};")));
            assert!(keymap.contains(&format!("maximum = {KEYCODE_BASE};")));
        }

        // The strongest pure-logic check available without a compositor: feed the
        // generated keymap to **libxkbcommon itself** (the same library a real
        // wlroots/KWin compositor compiles it with) and confirm (a) it parses and
        // (b) each assigned keycode resolves to exactly the intended keysym. If
        // this passes, the wire bytes a real compositor receives are correct.
        #[cfg(not(target_os = "macos"))]
        {
            use xkbcommon::xkb;
            eprintln!("phase generated_keymap_compiles_with_libxkbcommon_and_resolves_keysyms");

            let symbols = vec![
                Symbol::Unicode('a'),
                Symbol::Unicode('Z'),
                Symbol::Unicode('€'),
                Symbol::Space,
                Symbol::Return,
                Symbol::BackSpace,
                Symbol::Tab,
            ];
            let (keymap_str, assigned) = build_keymap(&symbols);

            let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
            let keymap = xkb::Keymap::new_from_string(
                &ctx,
                keymap_str,
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
            .expect("generated keymap should compile with libxkbcommon");

            // Each assigned keycode's single (layout 0, level 0) keysym must equal
            // the symbol we asked for.
            for (sym, code) in &assigned {
                let kc = xkb::Keycode::new(*code);
                let syms = keymap.key_get_syms_by_level(kc, 0, 0);
                assert_eq!(syms.len(), 1, "one keysym per synthetic key ({sym:?})");
                let got = xkb::keysym_get_name(syms[0]);
                let want = sym.keysym_name();
                // libxkbcommon canonicalises Unicode keysyms to their named form
                // when one exists (e.g. U0061 → "a", U0020 wouldn't appear since we
                // use the named `space`). Accept either the requested name or the
                // canonical character/name for that codepoint.
                let ok = got == want
                    || matches!(sym, Symbol::Unicode(c) if got == c.to_string())
                    || matches!(sym, Symbol::Unicode(c)
                        if got == format!("U{:04X}", *c as u32));
                assert!(
                    ok,
                    "keycode {code} resolved to {got:?}, expected {want:?} (for {sym:?})"
                );
            }
        }
    }

    #[test]
    fn injectors_record_actions_and_default_never_panics() {
        eprintln!("phase null_injector_records_commit_and_send");
        {
            let mut inj = NullInjector::new();
            assert_eq!(inj.backend_name(), "null");

            inj.commit_text("hi");
            inj.commit_text(" there");
            assert_eq!(inj.committed, vec!["hi".to_string(), " there".to_string()]);

            inj.send(&[Symbol::Unicode('Z'), Symbol::Space]);
            inj.send(&[Symbol::Return, Symbol::BackSpace]);
            assert_eq!(
                inj.keys,
                vec![
                    "U005A".to_string(), // 'Z'
                    "space".to_string(),
                    "Return".to_string(),
                    "BackSpace".to_string(),
                ]
            );
        }

        eprintln!("phase default_injector_never_panics_and_names_a_backend");
        {
            // In CI / headless there is usually no virtual-keyboard-v1 compositor,
            // so this should degrade to "null"; under a supporting compositor it
            // may bind the real backend. Either way it must not panic and must
            // report a known backend name.
            let inj = default_injector();
            let name = inj.backend_name();
            assert!(name == "null" || name == "virtual-keyboard-v1");
        }
    }
}
