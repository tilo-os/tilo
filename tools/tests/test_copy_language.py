import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


TOOLS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "check_copy_language", TOOLS / "policy/check_copy_language.py")
copy = importlib.util.module_from_spec(spec)
sys.modules["check_copy_language"] = copy
spec.loader.exec_module(copy)

GLOSSARY = '''
[[term]]
reason = "One secret, one name."
en = { use = "Password", avoid = ["Passcode"] }
de = { use = "Passwort", avoid = ["Kenncode"] }

[[term]]
reason = "The power panel."
en = { use = "Power", avoid = ["Session"], keys = "^power-" }
'''


def rules(locale, text, glossary=GLOSSARY):
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "glossary.toml"
        path.write_text(glossary)
        terms = copy.load_glossary(path)
    return [
        (message.key, rule)
        for message in copy.messages(text)
        for rule, _ in copy.violations(locale, message, terms)
    ]


class CopyLanguageTests(unittest.TestCase):
    def test_each_rule_catches_its_class(self):
        text = "\n".join([
            "a = Your files appear here",
            "b = This window belongs to an app the compositor doesn't host",
            "c = The workspace document did not accept the change",
            "d = Online · identity-certified",
            "e = Action job { $job } was admitted",
            "f = surface { $surface }",
            "g = AGENT CONTROLS · { $name }",
            "h = Direct when reachable · no vendor cloud · trusted only",
            "i = Streaming with { $peer } · select to stop",
            "window-transfer-confirm = Continue",
            "j = Enter your passcode.",
            "power-menu-title = Session",
        ]) + "\n"
        self.assertEqual(rules("en-US", text), [
            ("a", "narration"),
            ("b", "implementation"),
            ("c", "implementation"),
            ("d", "implementation"),
            ("d", "glyph"),
            ("e", "implementation"),
            ("e", "raw-id"),
            ("f", "implementation"),
            ("f", "raw-id"),
            ("g", "all-caps"),
            ("g", "glyph"),
            ("h", "chain"),
            ("h", "glyph"),
            ("i", "glyph"),
            ("i", "instruction"),
            ("window-transfer-confirm", "vague-button"),
            ("j", "glossary"),
            ("power-menu-title", "glossary"),
        ])

    def test_plain_copy_passes(self):
        text = "\n".join([
            "a = Couldn't move the tab. Try again.",
            "b = Online, 80%",
            "c = Wi-Fi, VPN and USB over HDMI",
            "d = Tap to click",
            "e = Click a window to focus it",
            "f = Hardware H.264/HEVC is ready",
            "g = Set HOME or XDG_STATE_HOME",
            "h = { $count ->",
            "        [one] { $count } speaker playing",
            "       *[other] { $count } speakers playing",
            "    }",
            "search-kw-power = session, compositor, AGENT",
            "window-transfer-confirm = Move",
            "terminal-session = Session ended",
        ]) + "\n"
        self.assertEqual(rules("en-US", text), [])

    def test_german_rules(self):
        text = "\n".join([
            "a = AGENT STEUERT · { $name }",
            "b = Zum Beenden auswählen, um fortzufahren",
            "c = Ein zertifiziertes Gerät",
            "d = Gib deinen Kenncode ein",
            "x-confirm = Weiter",
            "e = Als Agent zugelassen",
        ]) + "\n"
        self.assertEqual(rules("de-DE", text), [
            ("a", "all-caps"),
            ("a", "glyph"),
            ("b", "instruction"),
            ("c", "implementation"),
            ("d", "glossary"),
            ("x-confirm", "vague-button"),
        ])

    def test_separator_glyphs_and_symbol_icons_are_refused(self):
        text = "\n".join([
            "a = { $who } · { $when }",
            "b = Enter to save • Esc to cancel",
            "c = Settings | Network",
            "d = Offline — reconnecting",
            "e = Offline – reconnecting",
            "f = { $done } / { $total }",
            "g = ✓ Connected",
            "h = Next →",
            "i = ★ Favourites",
            "ok-range = Mon–Fri",
            "ok-pair = Read/write",
            "ok-sentence = Your files are safe, and the copy is kept.",
            "ok-dimension = { $w }×{ $h }",
        ]) + "\n"
        self.assertEqual(rules("en-US", text), [
            ("a", "glyph"),
            ("b", "glyph"),
            ("c", "glyph"),
            ("d", "glyph"),
            ("e", "glyph"),
            ("f", "glyph"),
            ("g", "glyph"),
            ("h", "glyph"),
            ("i", "glyph"),
        ])

    def test_attributes_and_terms_are_checked(self):
        text = "a = Fine\n    .title = DESKTOP ESSENTIALS\n-brand = Tilo will keep it\n"
        self.assertEqual(rules("en-US", text), [("a.title", "all-caps"), ("-brand", "narration")])

    def test_baseline_only_shrinks(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("old = YOU LEFT OFF\nfine = Synced\n")
            glossary = root / "glossary.toml"
            glossary.write_text(GLOSSARY)
            baseline = root / "baseline"
            baseline.mkdir()
            shard = baseline / "apps-demo.txt"

            self.assertEqual(len(copy.check(root, glossary, baseline)), 1)

            shard.write_text("# old copy\nen-US old all-caps\n")
            self.assertEqual(copy.check(root, glossary, baseline), [])

            (locales / "en-US.ftl").write_text("old = Where you left off\nfine = Synced\n")
            errors = copy.check(root, glossary, baseline)
            self.assertEqual(len(errors), 1)
            self.assertIn("no longer applies", errors[0])

            (locales / "en-US.ftl").write_text("old = YOU LEFT OFF\nfine = SAVED NOW\n")
            shard.write_text("en-US old all-caps\n")
            errors = copy.check(root, glossary, baseline)
            self.assertEqual(len(errors), 1)
            self.assertIn("fine", errors[0])

    def test_routine_saving_is_caught_and_outcomes_pass(self):
        text = "\n".join([
            "a = Saving…",
            "b = Saved",
            "c = Saved { $file }",
            "d = { $file } saved",
            "e = Draft saved",
            "f = Saving latest changes…",
            "g = Saved — waiting for the agent to apply it",
            "h = All changes saved.",
            "ok-copy = Saved a copy as { $name }",
            "ok-button = Save",
            "ok-failure = Couldn't save. Try again.",
            "ok-place = Saved places",
            "ok-where = Saved as { $name }.",
        ]) + "\n"
        expected = [(key, "save-status") for key in "abcdefgh"]
        expected.insert(6, ("g", "glyph"))
        self.assertEqual(rules("en-US", text), expected)
        german = "\n".join([
            "a = Gespeichert.",
            "b = { $file } gespeichert",
            "c = Wird gespeichert…",
            "d = Änderungen werden gespeichert…",
            "ok-button = Speichern",
        ]) + "\n"
        self.assertEqual(rules("de-DE", german), [(key, "save-status") for key in "abcd"])

    def test_save_exceptions_are_narrow_and_shrink(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-saved = Saved\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text(
                'fn label() -> &\'static str { "Saving…" }\n'
                'fn id() -> &\'static str { "saved" }\n'
                '#[cfg(test)]\nmod tests { fn t() { let _ = "Saved"; } }\n')
            glossary = root / "glossary.toml"
            glossary.write_text(GLOSSARY)
            baseline = root / "baseline"
            baseline.mkdir()
            exceptions = root / "exceptions.toml"
            exceptions.write_text("")
            errors = copy.check(root, glossary, baseline, exceptions)
            self.assertEqual(len(errors), 2, errors)
            self.assertIn("demo-saved", errors[0])
            self.assertIn("apps/demo/src/lib.rs:1", errors[1])

            exceptions.write_text(
                '[[allow]]\nunit = "apps-demo"\nkey = "demo-saved"\nreason = "A name."\n'
                '[[allow]]\nfile = "apps/demo/src/lib.rs"\ntext = "Saving…"\nreason = "A name."\n')
            self.assertEqual(copy.check(root, glossary, baseline, exceptions), [])

            (locales / "en-US.ftl").write_text("demo-saved = Saved a copy\n")
            errors = copy.check(root, glossary, baseline, exceptions)
            self.assertEqual(len(errors), 1)
            self.assertIn("no longer matches", errors[0])

            exceptions.write_text('[[allow]]\nunit = "apps-demo"\nkey = "demo-saved"\n')
            with self.assertRaises(ValueError):
                copy.check(root, glossary, baseline, exceptions)

    def test_glossary_terms_need_a_reason(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "glossary.toml"
            path.write_text('[[term]]\nen = { use = "A", avoid = ["B"] }\n')
            with self.assertRaises(ValueError):
                copy.load_glossary(path)

    def test_a_label_a_closure_picks_is_caught_and_a_call_it_feeds_is_not(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn dialog(step: Option<&Step>, open: bool) -> Node {",
                "    Node::new(Role::Dialog)",
                "        .with_name(step.map_or_else(",
                '            || if open { "Action confirmation" } else { "Search" },',
                "            |step| step.question.as_str(),",
                "        ))",
                '        .with_label(if open { "Shown" } else { "Hidden" })',
                '        .with_title("Plain")',
                '        .with_hint(pick(|word| say(word, "Grid")))',
                '        .with_description(crate::i18n::t("demo-name"))',
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Action confirmation", "Search", "Hidden", "Shown", "Plain"],
                "the label a step names itself is painted as much through a closure "
                "as in the argument itself; a word only fed to a call is not")


    def test_a_sentence_built_around_a_value_is_copy_too(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn facts(host: &str, track: Track, count: usize) -> Node {",
                '    let changes = format!("{count} changes to review");',
                "    let list = Node::new(Role::List).with_description(changes);",
                "    Node::new(Role::Canvas)",
                '        .with_name(format!("Play {}", track.title))',
                '        .with_description(crate::i18n::t("demo-name"))',
                "        .with_facts(vec![",
                '            Property::new("Host name", host.to_owned()),',
                '            Section::titled("Details", list),',
                '            SectionHeader::group("Saved accounts"),',
                "        ])",
                '        .with_hint(pick(|word| say(word, "Grid")))',
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["{count} changes to review", "Play {}", "Host name", "Details", "Saved accounts"],
                "a sentence built around a value is copy whether it reaches the "
                "constructor directly or through the local it is stored in; a "
                "property's name and a section's title are copy as well")

    def test_a_wrapped_converted_or_defaulted_word_is_painted(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn sheet(origin: Option<&str>, open: bool) -> Node {",
                "    Node::new(Role::Dialog)",
                '        .with_name(String::from("Move"))',
                '        .with_label("Shown".to_string())',
                '        .with_description(origin.unwrap_or("No secure page"))',
                '        .with_hint(pick(word, "Grid"))',
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Move", "Shown", "No secure page"],
                "a word reaches the label the same through a wrapper or a "
                "conversion, and the default of an `unwrap_or` is the words the "
                "constructor gets; a word only fed to a call is still not painted")

    def test_a_modal_sheets_buttons_are_copy(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn actions(&self) -> Vec<ModalAction<'static>> {",
                "    if self.export_running() {",
                '        return vec![ModalAction::secondary("Cancel")];',
                "    }",
                "    vec![",
                '        ModalAction::secondary("Close"),',
                '        ModalAction::primary("Export").enabled(self.export_is_offered()),',
                '        ModalAction::alternate_danger("Overwrite"),',
                "    ]",
                "}",
                "fn done(&self) -> Vec<ModalAction<'_>> {",
                '    let label = crate::i18n::t("demo-name");',
                "    vec![ModalAction::primary(&label)]",
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Cancel", "Close", "Export", "Overwrite"],
                "a sheet's buttons are the words a person presses: every answer "
                "it offers paints its label, whichever way the others are "
                "emphasised, and one resolved through the catalogue is not copy")

    def test_a_modal_sheets_title_is_copy(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-settings = Document settings\n")
            (locales / "de-DE.ftl").write_text("demo-settings = Dokumenteinstellungen\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn draw(&self) -> ModalSpec<'_> {",
                '    ModalSpec::new("Document settings").body_height(self.body_h())',
                "}",
                "fn catalogued(&self) -> ModalSpec<'_> {",
                '    let title = crate::i18n::t("demo-settings");',
                "    ModalSpec::new(&title)",
                "}",
                "fn line_height(&self, text: &mut TextRenderer) -> f64 {",
                "    let line = f64::from(",
                '        text.measure_style("Ag", None, &self.style).1,',
                "    );",
                "    line",
                "}",
                "fn token(&self) -> Token {",
                '    Token::new("Bearer token")',
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Document settings"],
                "a sheet's title is the words a person reads above it, so a "
                "literal handed to `ModalSpec::new` is copy, while the words "
                "at another type's `new` are not: there they are a measurement "
                "sample, an identifier or a message a person never reads. A "
                "title the crate resolved through its catalogue is not copy "
                "either")

    def test_a_locals_words_are_read_in_the_function_that_binds_them(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-settings = Document settings\n")
            (locales / "de-DE.ftl").write_text("demo-settings = Dokumenteinstellungen\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn empty_state(&self) -> Node {",
                '    let title = "No folders yet";',
                "    EmptyState::paint(self.area(), title)",
                "}",
                "fn sheet(&self) -> ModalSpec<'_> {",
                '    let title = crate::i18n::t("demo-settings");',
                "    ModalSpec::new(&title)",
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)], [],
                "a local lives in the body that declares it: the words one "
                "function stores in `title` are not what the sheet further down "
                "the file hands its constructor")

    def test_a_call_handed_fewer_arguments_than_a_label_paints_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn message(&self, anchor: &str) -> String {",
                '    format!("Files opens no `{anchor}`")',
                "}",
                "fn close_button(&self) -> Button {",
                '    let label = "Close";',
                '    Button::new("demo.close")',
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)], [],
                "a builder handed the id alone has no label to paint, so the "
                "words before it in the file — a local it does not take, a "
                "`format!` it does not take — are another call's")

    def test_a_plain_local_and_an_action_label_are_copy(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn pane(scene: &mut Scene, name: String) {",
                '    let label = "Waiting for the first screen update";',
                "    Node::new(Role::Canvas).draw_text(scene, label);",
                "}",
                "fn actions(mut record: E, mut open: E, mut keep: E) {",
                '    record.type_mut().label = "Stop recording".into();',
                '    open.type_mut().label = format!("Open {}", name);',
                '    keep.type_mut().label = crate::i18n::t("demo-name");',
                "}",
                "fn note() {",
                "    let mut story = Note::default();",
                '    story.title = "Quiet hours";',
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Waiting for the first screen update", "Stop recording", "Open {}"],
                "a local holds the words it is given, and an action's own label "
                "is what the menu and the action directory show; a field that "
                "holds a person's own content is not copy")

    def test_a_table_of_names_reaches_the_accessible_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn node(&self, action: &Action) -> Node {",
                "    let (role, name, state) = match action {",
                '        Action::Back => (Role::Button, "Back".into(), State::new()),',
                "        Action::Shutter => (",
                "            Role::Button,",
                '            if self.counting { "Cancel capture timer" } else { "Take photo" }',
                "                .into(),",
                "            State::new(),",
                "        ),",
                '        Action::Photos => (Role::Button, "Open Photos".into(), State::new()),',
                "    };",
                "    AccessNode::new(role).with_name(name).with_state(state)",
                "}",
                "fn reference(&self) -> ResourceSpec {",
                '    ResourceSpec::new("demo.thing", "demo.thing", CATALOG.t("demo-name"))',
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Back", "Cancel capture timer", "Take photo", "Open Photos"],
                "a name the table of a match picks is what the accessible name "
                "shows, whether the pattern names it alone or in a tuple")

    def test_a_helper_that_forwards_its_argument_paints_the_callers_words(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn button(&self, id: &str, label: &str, action: Action) -> Button {",
                "    Button::new(id, label).on_effect(self.command(action))",
                "}",
                "fn action_button(&self, label: &str) -> Button {",
                '    self.button("demo.action", label, Action::Go)',
                "}",
                "fn treated(&self, text: &str) -> Box<dyn Widget> {",
                "    Box::new(Text::new(text).with_role(TextRole::Body))",
                "}",
                "fn panel(&self) -> Node {",
                '    self.button("demo.fill", "Fill login", Action::Fill);',
                '    self.action_button("Delete account");',
                '    self.treated(CATALOG.t("demo-name"));',
                '    self.treated(word);',
                "    Node::new(Role::Panel)",
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Fill login", "Delete account"],
                "a helper hands its parameter to a label, so a literal at that "
                "position at any call site is painted — also through a second "
                "helper; a catalogue key or a value is not")

    def test_a_helper_that_takes_a_key_is_handed_a_key(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "impl ActionButton {",
                "    pub fn new(key: &str, control: Control) -> Self {",
                "        let label = crate::strings::t(key);",
                "        Self { label, control }",
                "    }",
                "}",
                "impl Panel {",
                "    fn nav_row(&self, forward: Control, forward_key: &str) -> Row {",
                "        Row::new().push(ActionButton::new(forward_key, forward))",
                "    }",
                "    fn page(&self) -> Row {",
                '        ActionButton::new("demo.action-back", self.back());',
                '        ActionButton::new("Refresh disks", self.refresh());',
                '        self.nav_row(self.next(), "demo.action-next");',
                '        self.nav_row(self.next(), "Next");',
                "        self.nav_row(self.next(), self.key());",
                "        Row::new()",
                "    }",
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Refresh disks", "Next"],
                "a helper that hands its parameter to a catalogue lookup takes "
                "a key, so a call site filling it with words paints the source "
                "language inside a translated window")

    def test_a_resource_title_and_an_action_reason_are_copy(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn resources(&self) -> Vec<ResourceSpec> {",
                '    let session = ResourceSpec::new("demo.session", "demo.session",',
                '        if self.paused() { "History paused" } else { "History recording" });',
                "    let clear = E::ClearHistory",
                '        .bound()',
                '        .unavailable("already_empty", "Browsing history is already empty");',
                '    let history = ResourceSpec::new("demo.history", "demo.history",',
                '        CATALOG.t("demo-name"));',
                "    vec![session, clear, history]",
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["History paused", "History recording",
                 "Browsing history is already empty"],
                "a resource's title and the reason an action is unavailable are "
                "rows a person reads in the action directory")

    def test_the_words_a_function_keeps_are_copy_where_the_crate_paints_them(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn friendly_reason(reason: &str) -> &'static str {",
                "    if reason.is_empty() {",
                '        "Add paper to continue."',
                "    } else {",
                '        "Clear the paper jam, then resume the printer."',
                "    }",
                "}",
                "fn state() -> String {",
                '    format!("Printing {}", pages)',
                "}",
                "fn treated(text: &str) -> Box<dyn Widget> {",
                "    Box::new(Text::new(text).with_role(TextRole::Body))",
                "}",
                "fn pane(reason: &str) -> Node {",
                "    Node::new(Role::Panel)",
                '        .with_description(treated(friendly_reason("x"))),',
                '        .with_hint(treated(state()))',
                "}",
                "fn spec() -> ResourceSpec {",
                '    ResourceSpec::new("demo.thing", "demo.thing", friendly_reason("y"))',
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Add paper to continue.", "Clear the paper jam, then resume the printer.",
                 "Printing {}"],
                "a function hands its value back rather than a caller's argument on, "
                "so the words it returns are what the helper or the resource paints")

    def test_a_models_own_name_and_a_reused_name_are_not_copy(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn name() -> &'static str {",
                '    match self.kind { Camera => "CameraApp", Files => "file-explorer" }',
                "}",
                "fn treated(text: &str) -> Box<dyn Widget> {",
                "    Box::new(Text::new(text).with_role(TextRole::Body))",
                "}",
                "fn pane(&self) -> Node {",
                "    Node::new(Role::Panel).with_description(treated(name()))",
                "}",
                "fn line(text: &str) -> Box<dyn Widget> {",
                "    Box::new(Text::new(text).with_role(TextRole::Body))",
                "}",
                "fn dialog(&self) -> String {",
                "    format!(\"{}\", self.request)",
                "}",
                "fn panel(&self) -> Node {",
                '    Node::new(Role::Panel).with_description(dialog())',
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)], [],
                "a name the model answers with is not copy, and a crate that gives "
                "one name two jobs (`line` here and `line` in another module) says "
                "nothing about the word a call site hands it")

    def test_the_words_a_function_keeps_are_copy_at_a_constructor_too(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn kind_label(kind: Kind) -> &'static str {",
                "    match kind {",
                '        Kind::Login => "Login",',
                '        Kind::Wifi => "Wi-Fi network",',
                "    }",
                "}",
                "fn basis(pages: usize) -> String {",
                '    format!("{} pages scanned", pages)',
                "}",
                "fn detail(kind: Kind) -> Box<dyn Widget> {",
                "    Box::new(Text::new(kind_label(kind)))",
                "}",
                "fn hint(kind: Kind, pages: usize) -> Node {",
                "    Node::new(Role::Panel)",
                "        .with_name(kind_label(kind))",
                "        .with_description(basis(pages))",
                "}",
                "fn heading() -> &'static str {",
                '    "Last backup"',
                "}",
                "fn section() -> Section {",
                "    Section::titled(heading(), rows())",
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Login", "Wi-Fi network", "{} pages scanned", "Last backup"],
                "a function that keeps the words it hands back paints them at "
                "every constructor its value reaches, not only at a helper or "
                "a resource title")

    def test_a_function_that_answers_with_a_machine_name_is_not_copy(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn name() -> &'static str {",
                '    match self.kind { Camera => "CameraApp", Files => "file-explorer" }',
                "}",
                "fn target() -> &'static str {",
                '    match self.what { Thing => "demo.thing", Other => "utf-8" }',
                "}",
                "fn key(&self) -> &'static str {",
                '    match self.page { Page => "AC", Clear => "esc" }',
                "}",
                "fn row(&self) -> Box<dyn Widget> {",
                "    Box::new(Text::new(name()))",
                "}",
                "fn pane(&self) -> Node {",
                "    Node::new(Role::Panel)",
                "        .with_name(target())",
                "        .with_description(key())",
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)], [],
                "the model's own names and a key's name are not words a person "
                "reads, so a function that answers with one says nothing")

    def test_a_painted_function_word_can_be_allowed_and_the_entry_shrinks(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn cap() -> &'static str {",
                '    "Backspace"',
                "}",
                "fn key(&self) -> Box<dyn Widget> {",
                "    Box::new(Text::new(cap()))",
                "}",
            ]) + "\n")
            glossary = root / "glossary.toml"
            glossary.write_text(GLOSSARY)
            baseline = root / "baseline"
            baseline.mkdir()
            save = root / "save.toml"
            save.write_text("")
            exceptions = root / "literals.toml"
            exceptions.write_text("")
            errors = copy.check(root, glossary, baseline, save, exceptions)
            self.assertEqual(len(errors), 1, errors)
            self.assertIn("apps/demo/src/lib.rs:2", errors[0])
            self.assertIn("[uncatalogued]", errors[0])

            exceptions.write_text(
                '[[allow]]\nfile = "apps/demo/src/lib.rs"\ntext = "Backspace"\n'
                'reason = "A key'"'"'s name, as the keyboard prints it."\n')
            self.assertEqual(copy.check(root, glossary, baseline, save, exceptions), [])

            (source / "lib.rs").write_text("\n".join([
                "fn cap() -> &'static str {",
                '    match self.kind { Key => "Delete", Other => "Erase" }',
                "}",
                "fn key(&self) -> Box<dyn Widget> {",
                "    Box::new(Text::new(cap()))",
                "}",
            ]) + "\n")
            errors = copy.check(root, glossary, baseline, save, exceptions)
            self.assertEqual(len(errors), 3, errors)
            self.assertTrue(any("no longer matches" in error for error in errors), errors)

    def test_a_copy_field_bound_to_a_painted_local_is_copy(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn row(&self) -> Node {",
                '    let row = Row { title: "Open settings".into(), detail: "Stopped".into() };',
                "    Node::new(Role::List).with_facts(vec![Text::new(row.title)])",
                "}",
                "fn sample(&self) -> Note {",
                '    let note = Note { title: "Quiet hours".into() };',
                "    note",
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Open settings"],
                "a copy field that a builder reads through the local it is bound to "
                "is copy; the field of a struct nothing painted is a person's own "
                "content, and so is one built as sample data")


    def test_a_task_runs_update_is_copy_too(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn run(&mut self) -> Result<()> {",
                "    self.report(TaskRunUpdate::Progress {",
                '        message: "Reading the actions this workspace publishes".into(),',
                "        presentation: None,",
                "    })?;",
                "    self.report(TaskRunUpdate::Progress {",
                "        message: format!(\"Running {} of {}\", index, total),",
                "        presentation: None,",
                "    })?;",
                "    self.report(TaskRunUpdate::Progress {",
                "        message: crate::i18n::t(\"demo-name\"),",
                "        presentation: None,",
                "    })?;",
                "    self.report(TaskRunUpdate::Failed {",
                "        message: self.why(),",
                "    })",
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Reading the actions this workspace publishes", "Running {} of {}"],
                "a headless participant has no window: the words it hands a run's "
                "update are what the person reads, and a sentence built around a "
                "value is copy like any other; what it resolved through its own "
                "catalogue, or holds in its own words, is not")

    def test_a_field_a_structs_own_type_paints_is_copy_inline(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "impl<'a> HudItem<'a> {",
                "    fn face(&self, rect: Rect) -> ButtonFace<'_> {",
                "        match (self.label.is_empty(), self.icon) {",
                "            (true, Some(icon)) => ButtonFace::icon_only(rect, icon, self.name),",
                "            (_, None) => ButtonFace::new(rect, self.label),",
                "            _ => ButtonFace::new(rect, self.label),",
                "        }",
                "    }",
                "}",
                "fn sample(&self) -> Note {",
                '    let note = Note { title: "Quiet hours".into() };',
                "    note",
                "}",
                "fn draw(&mut self) {",
                "    let items = vec![",
                '        HudItem { label: "", icon: Some(Icon::Timer), name: "Self-timer" },',
                '        HudItem { label: "", icon: Some(Icon::Grid), name: "Composition" },',
                '        Note { title: "Quiet hours" },',
                "    ];",
                "    self.draw_cluster(&items);",
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Self-timer", "Composition"],
                "a struct literal the type itself paints the field of is copy however "
                "it is built, a hyphenated word included; a type that paints no field "
                "keeps its own content")

    def test_a_name_with_one_job_says_which_word_is_copy(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn copy_line(text: &str) -> Box<dyn Widget> {",
                "    Box::new(Text::new(text).with_role(TextRole::Body))",
                "}",
                "fn layout_line(outline: &str) -> String {",
                '    format!("- {outline}")',
                "}",
                "fn panel(&self) -> Node {",
                '    let row = copy_line("Keep the address");',
                '    let token = layout_line("dialog confirm");',
                '    Node::new(Role::Panel).with_description(copy_line("Delete address?"))',
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Keep the address", "Delete address?"],
                "a name a crate gives one job says which of its arguments is the word "
                "a person reads, so the English handed to it is found")

    def test_a_test_doubles_method_decides_no_helpers_job(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn button(id: &str, label: &str) -> Widget {",
                "    Button::new(id, label)",
                "}",
                "fn pane(&self) -> Node {",
                '    Node::new(Role::Panel).with_child(button("demo.open", "Open settings"))',
                "}",
                "#[cfg(test)]",
                "mod tests {",
                "    struct Double;",
                "    impl Clipboard for Double {",
                "        fn button(&self, id: &str, label: &str) -> Widget {",
                "            Widget::none()",
                "        }",
                "    }",
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Open settings"],
                "a method a test double only stubs says nothing about the job a name "
                "has in the window code")

    def test_the_name_a_reader_hears_is_copy_in_every_branch(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn control(&self, hit: &Hit) -> Option<(String, String, Role, State)> {",
                "    let (target, name, role, state) = match hit {",
                '        Hit::Close => ("close".into(), "Done".into(), Role::Button,',
                "                       State::new()),",
                '        Hit::Nothing => ("none".into(), tr("demo-name"), Role::Group,',
                "                          State::new()),",
                "        Hit::Open => {",
                "            let label = self.title();",
                "            (",
                '                "open".into(),',
                '                format!("Open {}", label),',
                "                Role::Button,",
                "                State::new(),",
                "            )",
                "        }",
                "        Hit::Other => return None,",
                "    };",
                "    Some((target, name, role, state))",
                "}",
                "fn node(&self) -> Node {",
                "    let (target, name, role, state) = self.control(&Hit::Close)?;",
                "    Node::new(role).with_name(name).with_action_target(target)",
                "}",
                "fn measured(&self) -> Node {",
                "    let (width, height) = self.pane_size();",
                "    Node::new(Role::Group).with_bounds(Rect::new(0.0, 0.0, width, height))",
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Done", "Open {}"],
                "a widget names each of its nodes from one element of the tuple it "
                "projects, so that element is what a German reader hears \u2014 in the "
                "arms the tuple is written in as well as the value it returns")

    def test_a_value_interpolated_into_a_message_is_copy_too(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn status(filesystem: &str, count: usize, who: &str) -> String {",
                '    let state = "Unavailable";',
                '    CATALOG.t_args("demo-state", &[("state", ArgValue::Text("Error".to_owned()))]);',
                '    CATALOG.t_args("demo-state", &[("state", ArgValue::Text(state.into()))]);',
                '    CATALOG.t_args("demo-state", &[("state", ArgValue::Text(format!("Mounted {}", filesystem)))]);',
                '    CATALOG.t_args("demo-state", &[("state", "Idle".into())]);',
                '    CATALOG.t_args("demo-shared", &[("who", ArgValue::Text(who.to_owned()))]);',
                '    CATALOG.t_args("demo-count", &[("count", ArgValue::Number(count as f64))]);',
                '    crate::i18n::t("demo-name")',
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Unavailable", "Error", "Mounted {}", "Idle"],
                "a message's variables are the person's data: an English word "
                "interpolated into the sentence, directly or through the local "
                "it is stored in, reads in English in every language, so it is "
                "copy; a name the person gave, a count, or a word the app "
                "resolved through its catalogue is not")
    def _root(self, files: dict[str, str]) -> Path:
        root = Path(tempfile.mkdtemp())
        self.addCleanup(__import__("shutil").rmtree, root)
        locales = root / "apps/demo/locales"
        locales.mkdir(parents=True)
        (locales / "en-US.ftl").write_text("demo-name = Demo\n")
        (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
        for name, text in files.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        return root

    def test_a_constant_that_holds_words_is_copy_where_it_is_written(self):
        root = self._root({
            "apps/demo/src/lib.rs": "\n".join([
                "const INSTALL_METHODS: [&str; 2] = [\"Erase disk\", \"Install alongside\"];",
                "const BUTTON_IDS: [&str; 2] = [\"erase\", \"alongside\"];",
                "fn choice() -> Bound {",
                "    seg_choice(&INSTALL_METHODS, controls(), 0, \"method\")",
                "}",
                "fn ids() -> Bound {",
                "    seg_choice(&BUTTON_IDS, controls(), 0, \"ids\")",
                "}",
                "fn seg_choice(labels: &[&str], controls: Vec<Control>, selected: usize,",
                "              test_id: &str) -> Bound {",
                "    let segments = labels.iter().map(|label| Segment::new(*label)).collect();",
                "    Bound::new(Box::new(ToggleGroup::new(segments, selected)), None)",
                "}",
                "fn controls() -> Vec<Control> { Vec::new() }",
            ]) + "\n",
        })
        self.assertEqual(
            [text for _, _, text in copy.source_copy_literals(root)],
            ["Erase disk", "Install alongside"],
            "a label array reaches a segment through its name alone, so the words "
            "are reported where the constant writes them, and a table of ids is not")

    def test_a_sentence_behind_a_type_alias_still_hands_its_words_back(self):
        root = self._root({
            "apps/demo/src/lib.rs": "\n".join([
                "type Blocked = Option<(&'static str, String)>;",
                "fn off_step(step: Step, current: Step) -> Blocked {",
                "    (step != current).then(|| {",
                "        (",
                '            "other_step",',
                '            format!("This belongs to the {} step; the wizard is on {}.",',
                "                    step_name(step), step_name(current)),",
                "        )",
                "    })",
                "}",
                "fn step_name(step: Step) -> &'static str { step.key() }",
                "fn row(app: &App) -> Row {",
                "    Row::new().with_description(",
                "        off_step(Step::Disk, app.step)",
                "            .map(|(_, reason)| reason)",
                "            .unwrap_or_default(),",
                "    )",
                "}",
            ]) + "\n",
        })
        found = [text for _, _, text in copy.source_copy_literals(root)]
        self.assertEqual(
            found,
            ["This belongs to the {} step; the wizard is on {}."],
            "a reason behind a type alias reaches the constructor it is painted "
            "through, so the sentence is copy even though its signature names no "
            "string type")

    def test_a_label_table_the_crate_maps_to_keys_is_not_copy(self):
        root = self._root({
            "apps/demo/src/lib.rs": "\n".join([
                "const TEXT_SIZES: &[(&str, f64)] = &[",
                '    ("Small", 0.85),',
                '    ("Large", 1.3),',
                "];",
                "fn text_size_ftl(english: &str) -> &'static str {",
                "    match english {",
                '        "Small" => "demo-text-small",',
                '        _ => "demo-text-large",',
                "    }",
                "}",
                "fn labels() -> Vec<String> {",
                "    TEXT_SIZES.iter()",
                "        .map(|(label, _)| crate::strings::t(text_size_ftl(label)))",
                "        .collect()",
                "}",
                "fn row() -> Node {",
                "    Node::new(Role::List).with_description(Segment::new(labels()[0].clone()))",
                "}",
            ]) + "\n",
        })
        self.assertEqual(
            [text for _, _, text in copy.source_copy_literals(root)],
            [],
            "the words are keys into the catalogue through the crate's own "
            "mapping, so the window paints a translation, not the table")

    def test_a_refusal_reported_to_the_caller_is_not_copy(self):
        root = self._root({
            "apps/demo/src/lib.rs": "\n".join([
                "fn answer(name: &str) -> Result<(), String> {",
                "    let value = lookup(name).ok_or_else(|| {",
                '        format!("`{name}` is not a name this panel has")',
                "    })?;",
                "    if value.stale {",
                '        return Err("the panel has moved on".to_string());',
                "    }",
                "    Ok(())",
                "}",
                "fn caption() -> String {",
                '    "Nothing is selected.".to_string()',
                "}",
                "fn row() -> Node {",
                "    Node::new(Role::List).with_description(caption());",
                "    Node::new(Role::List).with_hint(answer(\"x\"));",
                "}",
            ]) + "\n",
        })
        self.assertEqual(
            [text for _, _, text in copy.source_copy_literals(root)],
            ["Nothing is selected."],
            "a refusal answers the code that asked, in the words that code "
            "shows; the sentence a function keeps is what a window paints")

    def test_the_window_bars_label_fields_are_copy(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "app.rs").write_text("\n".join([
                "pub struct DemoApp {",
                "    bar: ui::framework::BarCommands<Command>,",
                "}",
                "impl DemoApp {",
                "    fn bar(&self) -> WindowToolbar {",
                "        let entries = &self.bar;",
                "        let (id, icon, name) = match self.more_open {",
                '            true => (BAR_MORE, "download", "Importing…"),',
                '            false => (BAR_MORE, "download", "Import contacts"),',
                "        };",
                "        WindowToolbar {",
                "            actions: vec![",
                '                self.bar.entry("plus", "New event", Command::New),',
                '                entries.entry("trash", "Delete", Command::Delete),',
                '                ToolbarAction::new("export", "upload", "Export"),',
                "                ToolbarAction {",
                '                    name: "Copy".to_owned(),',
                "                    ..ToolbarAction::new(id, icon, name)",
                "                },",
                "            ],",
                "            location: Some(ToolbarLocation {",
                '                label: "Inbox".to_owned(),',
                '                name: crate::i18n::t("demo-name"),',
                '                value: "Library".to_owned(),',
                "                choices: vec![LocationChoice {",
                '                    label: "Your shell on this device".to_owned(),',
                '                    value: "shell".to_owned(),',
                "                    ..LocationChoice::default()",
                "                }],",
                "                segments: vec![LocationSegment {",
                '                    label: "Library".to_owned(),',
                '                    value: "/library".to_owned(),',
                "                }],",
                "                ..ToolbarLocation::default()",
                "            }),",
                "            views: Some(ToolbarViews {",
                '                name: "View".to_owned(),',
                "                choices: vec![ToolbarView {",
                '                    id: "month".to_owned(),',
                '                    label: "Month".to_owned(),',
                "                }],",
                '                chosen: "month".to_owned(),',
                "            }),",
                "            notice: Some(ToolbarNotice {",
                '                text: "Page captured".to_owned(),',
                "                ..ToolbarNotice::default()",
                "            }),",
                "            ..WindowToolbar::default()",
                "        }",
                "    }",
                "}",
                "struct Row {",
                "    label: String,",
                "}",
                "fn rows() -> Vec<Row> {",
                '    vec![Row { label: "Resolution".to_owned() }]',
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Importing…", "Import contacts", "New event", "Delete", "Export",
                 "Copy", "Inbox", "Your shell on this device", "Library", "View",
                 "Month", "Page captured"],
                "the window's bar draws what an app declares for it, so the "
                "words in its own tables' label fields — an action's name, a "
                "place's, a view's, a notice's — are painted where a person "
                "reads them, whether the app writes them into the field or "
                "makes the entry with them; a field of another type, a "
                "location's value (the address a person typed) and a word "
                "resolved through the catalogue are not copy")

    def test_the_words_a_paint_helper_takes_first_are_copy(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn paint(&mut self, ctx: &mut PaintCtx<'_>) {",
                '    ctx.text_role("Colours", TextRole::Subheading, ctx.theme.text_primary, x, None);',
                '    ctx.text_role_in_band("Preview", TextRole::Body, ctx.theme.text_primary, 0.0, band, None);',
                '    ctx.text_role(&crate::CATALOG.t("demo-name"), TextRole::Body, ink, x, None);',
                '    let face = ButtonFace::new(rect, "Duplicate").control_size(size);',
                '    let alone = ButtonFace::icon_only(rect, Icon::Trash2, "Remove font");',
                '    let swatch = ColorSwatchFace::new(rect, colour, "Key colour");',
                '    let named = ButtonFace::new(rect, label).accessible_name("Rename scheme");',
                "    paint(face, alone, swatch, named);",
                "}",
                "fn menu(&self, intents: Intents) {",
                '    let open = MenuEntry::new("Open in Files", move || intents.push(Open));',
                '    let away = MenuEntry::destructive("Delete scheme", move || intents.push(Delete));',
                '    let why = MenuEntry::caption("The firewall is stopped");',
                "    show(open, away, why);",
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Colours", "Preview", "Duplicate", "Remove font", "Key colour",
                 "Rename scheme", "Open in Files", "Delete scheme",
                 "The firewall is stopped"],
                "a context paints its words through the helper it holds them "
                "first in, and a button face, a swatch and a menu entry are the "
                "words a person reads; what the catalogue answered is not")

    def test_a_helpers_struct_says_the_words_of_the_fields_it_paints(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn draw_row(ctx: &mut PaintCtx<'_>, row: &Row<'_>) {",
                "    let &Row { title, secondary, trailing } = row;",
                "    let mut face = ListRowFace::new(title);",
                "    if let Some(secondary) = secondary {",
                "        face = face.secondary(secondary);",
                "    }",
                "    face.paint(ctx, row_rect(trailing));",
                "}",
                "fn font_row(&self, ctx: &mut PaintCtx<'_>) {",
                "    let kind = match &f.kind {",
                '        Kind::Default => "Default face".to_string(),',
                '        Kind::Mono => "Monospace".to_string(),',
                "    };",
                '    self.draw_row(ctx, &Row { title: &name, secondary: Some(&kind), trailing: 0.0 });',
                "    let status = if in_use {",
                '        "In use"',
                "    } else {",
                '        "Built-in"',
                "    };",
                '    self.draw_row(ctx, &Row { title: &name, secondary: Some(status), trailing: 0.0 });',
                '    self.draw_row(ctx, &Note { title: "Quiet hours".into(), secondary: None, trailing: 0.0 });',
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Default face", "Monospace", "In use", "Built-in"],
                "a helper that takes a struct apart and paints its fields says "
                "the words of those fields at every call site, whether a local or "
                "a selection names them; another type's field there is not")

    def test_a_tuple_parameter_paints_its_element_of_the_callers_tuple(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "pub(super) fn icon_button(",
                "    &mut self,",
                "    ctx: &mut PaintCtx<'_>,",
                "    center: Point,",
                "    size: ControlSize,",
                "    (icon, name): (Icon, &str),",
                "    action: Action,",
                ") -> Rect {",
                "    let rect = Rect::from_center_size(center, size.height(ctx.theme));",
                "    let face = ButtonFace::icon_only(rect, icon, name);",
                "    self.button(ctx, face, action);",
                "    rect",
                "}",
                "fn row(&mut self, ctx: &mut PaintCtx<'_>) {",
                '    self.icon_button(ctx, at(0.0), ControlSize::Small, (Icon::Trash2, "Remove font"), Action::Remove);',
                '    self.icon_button(ctx, at(1.0), ControlSize::Small, (Icon::Pencil, "Edit"), Action::Edit);',
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Remove font", "Edit"],
                "the element of a tuple parameter a helper names a control with is "
                "the word at that element of the tuple its caller hands it, and the "
                "icon beside it is no word at all")

    def test_an_action_label_a_helper_sets_is_copy_at_its_call_site(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-name = Demo\n")
            (locales / "de-DE.ftl").write_text("demo-name = Demo\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text("\n".join([
                "fn row_operation(effect: &DemoEffect, label: &str) -> ActionSpec {",
                "    let mut spec = effect.bound();",
                "    spec.type_mut().label.clone_from(&label.to_owned());",
                "    spec",
                "}",
                "fn partition_row(part: &Partition) -> LiveRow {",
                "    row.operation(row_operation(",
                "        &DemoEffect::OpenFolder { device: part.name.clone() },",
                '        "Open in Files",',
                "    ))",
                "}",
            ]) + "\n")
            self.assertEqual(
                [text for _, _, text in copy.source_copy_literals(root)],
                ["Open in Files"],
                "an action's label names it in its menu row and in the action "
                "directory, so a helper that sets one paints its callers' words")

    def test_words_another_crate_writes_for_a_window_are_copy_where_written(self):
        root = self._root({
            "crates/backend/src/disk.rs": "\n".join([
                "pub enum Filesystem { Btrfs, Ext4 }",
                "impl Filesystem {",
                "    pub fn label(&self) -> &'static str {",
                "        match self {",
                '            Self::Btrfs => "Btrfs",',
                '            Self::Ext4 => "Ext4",',
                "        }",
                "    }",
                "}",
            ]) + "\n",
            "apps/demo/src/lib.rs": "\n".join([
                "fn row(filesystem: Filesystem) -> Bound {",
                "    Node::new(Role::List).with_description(filesystem.label())",
                "}",
            ]) + "\n",
        })
        self.assertEqual(
            [(file, text) for file, _, text in copy.source_copy_literals(root)],
            [("crates/backend/src/disk.rs", "Btrfs")],
            "the window paints the backend's words, so the backend is where "
            "the source language is written and where the catalogue goes")

    def test_a_name_the_painting_crate_defines_itself_is_not_another_crates(self):
        root = self._root({
            "crates/backend/src/tool.rs": "\n".join([
                "pub fn kind_label(kind: Kind) -> &'static str {",
                '    "Stored in a database"',
                "}",
            ]) + "\n",
            "apps/demo/src/lib.rs": "\n".join([
                "fn kind_label() -> &'static str {",
                '    "A page of notes"',
                "}",
                "fn row() -> Bound {",
                "    Node::new(Role::List).with_description(kind_label())",
                "}",
            ]) + "\n",
        })
        self.assertEqual(
            [text for _, _, text in copy.source_copy_literals(root)],
            ["A page of notes"],
            "the call is the window's own helper, judged as one")

    def test_another_types_constant_is_not_the_crates_constant(self):
        root = self._root({
            "apps/demo/src/lib.rs": "\n".join([
                'const TITLE: &str = "Disks";',
                "fn row() -> Bound {",
                "    Row::new(RowShape::TITLE)",
                "}",
            ]) + "\n",
        })
        self.assertEqual(
            [text for _, _, text in copy.source_copy_literals(root)],
            [],
            "`RowShape::TITLE` names another type's own TITLE, not the "
            "constant the crate defines")

    def test_the_tree_passes(self):
        self.assertEqual(
            copy.check(copy.ROOT, copy.GLOSSARY, copy.BASELINE, copy.SAVE_EXCEPTIONS,
                       copy.SOURCE_LITERALS), [])


if __name__ == "__main__":
    unittest.main()
