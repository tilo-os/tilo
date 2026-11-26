//! What a call writes besides the record it names: the shell's question card says each declared
//! effect in the person's words, one line each, and says nothing at all for an action that
//! writes only its own.

use tilolang_runtime::{Actor, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const SHELL: &str = "so.tilo.shell";

/// The toolkit's own words, which a shared view (`std.action_effects`) says its line in, and a
/// package's own for everything else.
struct Toolkit;

impl tilolang_runtime::Words for Toolkit {
    fn add(&self, catalogues: &[(String, String)]) {
        tilo_app_window::i18n::add_catalogues(catalogues);
    }

    fn add_used(&self, catalogues: &[(String, String)]) {
        tilo_app_window::i18n::add_shared_catalogues(catalogues);
    }

    fn word(&self, key: &str, args: &[(String, String)], source: &str) -> String {
        tilo_app_window::i18n::word(key, args, source)
    }

    fn revision(&self) -> u64 {
        tilo_app_window::i18n::revision()
    }
}

fn ada() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// The shell's question card, staged: one waiting call that declares an effect, one that writes
/// only its own record, and one that declares ten. They are the shell's shipped package with
/// [`SCENARIOS`] beside it: its scenarios import apps whose documents they stage, which this
/// test has no need of.
fn shell(name: &str) -> Host {
    let files: Vec<(String, String)> = tilolang_tilo::shell::sources()
        .iter()
        .map(|(path, text)| (format!("apps/tl/shell/{path}"), (*text).to_owned()))
        .chain(std::iter::once((
            "apps/tl/shell/asking_effects_scenarios.tl".to_owned(),
            SCENARIOS.to_owned(),
        )))
        .collect();
    let source = tilolang::PackageSource::new(files);
    let ids = tilolang::IdsLock::parse(include_str!("../../../../apps/tl/shell/app.ids"))
        .expect("the shell's ids parse");
    let mut imports: Vec<tilolang::PackageMeta> = tilolang_tilo::platform::platform_packages()
        .expect("the platform's packages read")
        .into_iter()
        .filter(|meta| meta.app != tilolang_tilo::platform::SESSION_APP)
        .collect();
    imports.insert(
        0,
        tilolang_tilo::platform::session_package().expect("the session's package reads"),
    );
    let compiled = tilolang::compile_package_source(&source, &ids, &imports)
        .unwrap_or_else(|diagnostics| panic!("the shell: {diagnostics:#?}"));
    let host = Host::new(ada(), || None).unwrap();
    host.set_words(Toolkit);
    for meta in tilolang_tilo::platform::platform_packages().unwrap() {
        let app = meta.app.clone();
        host.runtime()
            .load_foreign(meta)
            .unwrap_or_else(|why| panic!("{app}: {why}"));
    }
    host.load_compiled(&compiled).unwrap();
    host.root_of(SHELL).unwrap();
    let scenario = host
        .package(SHELL)
        .unwrap()
        .meta
        .scenarios
        .iter()
        .find(|scenario| scenario.name == name)
        .unwrap_or_else(|| panic!("the shell stages {name}"))
        .action
        .clone();
    host.runtime()
        .invoke_action(&ada(), (scenario.as_str(), vec![]))
        .unwrap();
    host
}

/// The scenarios this test stages, in the shape `apps/tl/shell/scenarios.tl` writes them: the
/// records stand in as the workspace's listing, and `question` draws each card, which is the
/// shell's own view. The list here asks for every waiting call, not only the invoker's, so a
/// staged record names no person.
const SCENARIOS: &str = r#"
fn waiting_record(
    asked_in: Text,
    record: Text,
    name: Text,
    by: invocations.Asker,
    asked_at_ms: Int,
    words: Text,
    on: Option<Text>,
    facts: List<invocations.Fact>,
    choices: List<Text>,
    destructive: Bool,
    verb: Text,
    plural: Option<Text>,
    pressed: Option<invocations.Pressed>,
    conversation: Option<Text>,
    form: List<invocations.FormField>,
    effects: List<invocations.DeclaredEffect>,
) -> invocations.Invocation {
    return invocations.Invocation(
        id: .some(record),
        by: invocations.By(name: "Ada Lovelace", kind: "person"),
        status: .waiting,
        started_ms: asked_at_ms,
        changes: 0,
        undone: 0,
        undoable: false,
        recorded: true,
        remembered: true,
        person: .none,
        asked: .some(
            invocations.Asked(
                record: invocations.AskedRecord(workspace: asked_in, record: record),
                workspace: asked_in,
                workspace_name: "Studio",
                app: "",
                action: "",
                name: name,
                asked_at_ms: asked_at_ms,
                pressed: pressed,
                offers: [.approve, .decline],
                by: by,
                words: words,
                on: on,
                facts: facts,
                effects: effects,
                destructive: destructive,
                choices: choices,
                verb: verb,
                plural: plural,
                conversation: conversation,
                takes: [],
                many: false,
                kind: "",
                form: form,
            ),
        ),
    )
}

view asking_effects_list() {
    let asked = query(invocations.Invocation, status: .waiting).filter(q -> q.asked != .none)
    ui.Section("Waiting for you") {
        for q in asked {
            if let a = q.asked {
                question(asked_key(q), true, a)
            }
        }
    }
}

fn asked_key(q: invocations.Invocation) -> Text {
    if let a = q.asked {
        return "{a.record.workspace}/{a.record.record}"
    }
    return q.id
}

scenario "a call that writes another record" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(device.overlays, device.Overlays(here: "centre"))
    stage(
        [
            waiting_record(
                "studio",
                "invocations/mail-1",
                "Add mail account from vault",
                invocations.Asker(agent: .some(invocations.Named(id: "scout", name: "Scout"))),
                2,
                "",
                .some("Work mail"),
                [invocations.Fact(label: "Incoming server", value: "imap.work.example:993")],
                [],
                false,
                "add",
                .some("accounts"),
                .none,
                .none,
                [],
                [
                    invocations.DeclaredEffect(
                        app: "Passwords",
                        what: "Record where an entry was used",
                        app_id: "so.tilo.passwords",
                        kind: "passwords.workspace-vault",
                        verb: "keep-uses",
                    ),
                ],
            ),
            waiting_record(
                "studio",
                "invocations/alarm-3",
                "Set alarm",
                invocations.Asker(agent: .some(invocations.Named(id: "scout", name: "Scout"))),
                1,
                "",
                .some("Stand-up"),
                [invocations.Fact(label: "At", value: "09:30 every weekday")],
                [],
                false,
                "set",
                .some("alarms"),
                .none,
                .none,
                [],
                [],
            ),
        ],
    )
}

scenario "a call that writes ten records" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(device.overlays, device.Overlays(here: "centre"))
    stage(
        [
            waiting_record(
                "studio",
                "invocations/sweep-1",
                "Archive the quarter",
                invocations.Asker(agent: .some(invocations.Named(id: "scout", name: "Scout"))),
                2,
                "",
                .some("Studio"),
                [],
                [],
                false,
                "archive",
                .some("quarters"),
                .none,
                .none,
                [],
                [
                    invocations.DeclaredEffect(
                        app: "Passwords for the Clayton–Lovelace household",
                        what: "Record where an entry was used",
                        app_id: "so.tilo.passwords",
                        kind: "passwords.workspace-vault",
                        verb: "keep-uses",
                    ),
                    invocations.DeclaredEffect(
                        app: "The household's shared photo library",
                        what: "File the quarter's photographs under the year",
                        app_id: "so.tilo.photos",
                        kind: "photos.library",
                        verb: "file",
                    ),
                    invocations.DeclaredEffect(
                        app: "Notes shared with everyone in the studio",
                        what: "Record that the quarter was archived",
                        app_id: "so.tilo.notes",
                        kind: "notes.notebook",
                        verb: "record",
                    ),
                    invocations.DeclaredEffect(
                        app: "Calendar for the studio and the household",
                        what: "Close the quarter's past events",
                        app_id: "so.tilo.calendar",
                        kind: "calendar.calendar",
                        verb: "close",
                    ),
                    invocations.DeclaredEffect(
                        app: "Tasks of the studio's release programme",
                        what: "Mark every finished task of the quarter done",
                        app_id: "so.tilo.tasks",
                        kind: "tasks.tracker",
                        verb: "finish",
                    ),
                    invocations.DeclaredEffect(
                        app: "Mail of the household's two accounts",
                        what: "Move the quarter's read mail into the archive",
                        app_id: "so.tilo.mail",
                        kind: "mail.mailbox",
                        verb: "archive",
                    ),
                    invocations.DeclaredEffect(
                        app: "Chat, with every channel of the studio",
                        what: "Say in the release channel that the quarter is archived",
                        app_id: "so.tilo.chat",
                        kind: "chat.channels",
                        verb: "say",
                    ),
                    invocations.DeclaredEffect(
                        app: "Files of the shared workspace folder",
                        what: "Move the quarter's reports into the archive folder",
                        app_id: "so.tilo.file-explorer",
                        kind: "files.folder",
                        verb: "move",
                    ),
                    invocations.DeclaredEffect(
                        app: "The workspace's activity record",
                        what: "Write down what the archive moved",
                        app_id: "so.tilo.history",
                        kind: "history.activity",
                        verb: "record",
                    ),
                    invocations.DeclaredEffect(
                        app: "Contacts of the studio and the household",
                        what: "Note the addresses the archive wrote to",
                        app_id: "so.tilo.contacts",
                        kind: "contacts.book",
                        verb: "note",
                    ),
                ],
            ),
        ],
    )
}

scenario "questions waiting" {
    let d = root Layout
    d.person = "Ada Lovelace"
    d.workspaces.insert(LayoutWorkspace(key: "studio", name: "Studio", current: true))
    stage(device.overlays, device.Overlays(here: "centre"))
    stage(
        [
            waiting_record(
                "studio",
                "invocations/alarm-4",
                "Set alarm",
                invocations.Asker(agent: .some(invocations.Named(id: "scout", name: "Scout"))),
                1,
                "",
                .some("Alarms"),
                [invocations.Fact(label: "At", value: "07:00")],
                [],
                false,
                "set",
                .some("alarms"),
                .none,
                .none,
                [],
                [],
            ),
            waiting_record(
                "studio",
                "invocations/forget-2",
                "Forget this device",
                invocations.Asker(
                    device: .some(invocations.Named(id: "phone", name: "Ada's phone")),
                ),
                2,
                "",
                .some("Ada's old ThinkPad X1 Carbon (6th generation)"),
                [],
                [],
                true,
                "",
                .none,
                .none,
                .none,
                [],
                [],
            ),
        ],
    )
}
"#;

/// The questions waiting, as the notification centre lists them.
fn waiting(host: &Host) -> ViewInstance {
    let view = host
        .package(SHELL)
        .unwrap()
        .meta
        .views
        .iter()
        .find(|view| view.name == "asking_effects_list")
        .expect("the shell lists what waits")
        .id
        .clone();
    host.runtime().open_view(&ada(), &view, vec![]).unwrap()
}

/// The words of every `ui.Text` in `tree`.
fn texts(tree: &UiNode) -> Vec<String> {
    tree.of_kind("ui.Text")
        .into_iter()
        .filter_map(|node| match node.props.get("value") {
            Some(Value::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The effect lines of the question card: what the action says it writes besides its own.
fn effect_lines(tree: &UiNode) -> Vec<String> {
    texts(tree)
        .into_iter()
        .filter(|text| text.starts_with("Also writes to "))
        .collect()
}

tilo_test_env::scenario! {
    fn an_asked_action_says_what_else_it_writes;

    phase the_declared_effect_reads_in_the_persons_words;
    {
        let host = shell("a call that writes another record");
        let mut view = waiting(&host);
        view.poll_patches().unwrap();
        assert_eq!(
            effect_lines(view.tree()),
            ["Also writes to Passwords: Record where an entry was used"],
            "the vault step the mail account's call declares: {:?}",
            texts(view.tree())
        );
    }

    phase an_action_that_writes_only_its_own_says_nothing_of_what_it_writes;
    {
        let host = shell("questions waiting");
        let mut view = waiting(&host);
        view.poll_patches().unwrap();
        assert!(
            effect_lines(view.tree()).is_empty(),
            "no declared effect, no line: {:?}",
            texts(view.tree())
        );
    }

    phase ten_effects_read_as_ten_lines_under_long_names;
    {
        let host = shell("a call that writes ten records");
        let mut view = waiting(&host);
        view.poll_patches().unwrap();
        let lines = effect_lines(view.tree());
        assert_eq!(lines.len(), 10, "one line each: {lines:?}");
        assert!(
            lines.iter().any(|line| line
                == "Also writes to Passwords for the Clayton–Lovelace household: Record where an \
                    entry was used"),
            "a long app name is the whole name: {lines:?}"
        );
    }
}
