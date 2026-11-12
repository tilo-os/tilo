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

    def test_the_tree_passes(self):
        self.assertEqual(
            copy.check(copy.ROOT, copy.GLOSSARY, copy.BASELINE, copy.SAVE_EXCEPTIONS,
                       copy.SOURCE_LITERALS), [])


if __name__ == "__main__":
    unittest.main()
