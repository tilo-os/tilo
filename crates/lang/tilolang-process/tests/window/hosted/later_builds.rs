//! A later build of the package, as `tilolang dev` hands one over when the source is saved: in a
//! development session on the host's api, without one to the window it runs. The instance takes
//! it over in its window while it runs.

use super::packages::{later, package_of};
use super::{NOTES, Session, press_new_note};

/// Notes with two notes made as a person makes them, the second one open.
fn two_notes() -> Session {
    let mut session = Session::notes();
    for title in ["Home", "Plan"] {
        press_new_note(&mut session);
        session.settle();
        session.window.type_text(title);
        session.settle();
    }
    session
}

/// `signature`'s parameter list with its last parameter — the tag a tag action takes — made
/// `Int`, whatever the source calls that parameter.
fn tag_takes_int(source: &str, signature: &str) -> String {
    let (head, rest) = source
        .split_once(signature)
        .unwrap_or_else(|| panic!("{signature} is in the notes package"));
    let (params, tail) = rest.split_once(')').expect("the parameters end");
    let (before, after) = params.rsplit_once(": Text").expect("the tag is Text");
    format!("{head}{signature}{before}: Int{after}){tail}")
}

/// The build arrives on the host's api, as a development session hands a reinstall over.
#[test]
fn a_later_build_takes_over_in_place_keeping_the_open_note_and_the_list() {
    let mut session = two_notes();
    let mut titles = session.titles();
    titles.sort();
    assert_eq!(titles, ["Home", "Plan"]);
    assert_eq!(session.field_value("title"), Some(text("Plan")));
    assert!(
        session
            .shown()
            .iter()
            .any(|shown| shown == "Write something")
    );

    let later = later(package_of(NOTES), |source| {
        source.replace("\"Write something\"", "\"Write anything\"")
    });
    session.take_build_over_the_api(&later);
    session.settle();

    let shown = session.shown();
    assert!(
        shown.iter().any(|shown| shown == "Write anything"),
        "the new build draws the page: {shown:?}"
    );
    assert_eq!(
        session.field_value("title"),
        Some(text("Plan")),
        "the same note is still open"
    );
    let mut after = session.titles();
    after.sort();
    assert_eq!(after, titles, "and the notebook still lists every note");
}

/// The build arrives as `tilolang dev` publishes it for a window without a session.
#[test]
fn a_later_build_that_changes_a_field_type_starts_over_on_the_same_data() {
    let mut session = two_notes();
    assert_eq!(session.field_value("title"), Some(text("Plan")));

    tilolang_process::window::updates::builds().hand_over(later(package_of(NOTES), |source| {
        let source = ["action tag(", "action untag(", "action file_note("]
            .iter()
            .fold(
                source.replace("tags: Set<Text>", "tags: Set<Int>"),
                |source, signature| tag_takes_int(&source, signature),
            );
        source
            // A note's tags are numbers now: what its actions write and compare is one, while
            // the notebook still lists each tag by the name a person reads.
            .replace("    require tag.trim() != \"\" \"A tag needs a name\"\n", "")
            .replace(
                "nb.tags.find(t -> t.name.lower() == tag.trim().lower())",
                "nb.tags.find(t -> t.name == \"0\")",
            )
            .replace(
                "let listed = known?.name ?? tag.trim()",
                "let listed = known?.name ?? \"0\"",
            )
            .replace("n.tags.add(listed)", "n.tags.add(0)")
            .replace("made.tags.add(listed)", "made.tags.add(0)")
            .replace("n.tags.remove(tag.trim())", "n.tags.remove(0)")
            .replace(
                "if n.tags.any(t -> t == before) {",
                "if n.tags.any(t -> t == 0) {",
            )
            .replace("n.tags.remove(before)", "n.tags.remove(0)")
            .replace("n.tags.add(wanted.trim())", "n.tags.add(0)")
            .replace(
                "if n.tags.any(t -> t == tag.name) { n.tags.remove(tag.name) }",
                "if n.tags.any(t -> t == 0) { n.tags.remove(0) }",
            )
            .replace(
                "if nb.tags.find(t -> t.name == name) == .none { nb.tags.insert(Tag(name: name)) }",
                "if nb.tags.find(t -> t.name == \"{name}\") == .none { nb.tags.insert(Tag(name: \"{name}\")) }",
            )
            .replace("n.tags.any(x -> x == t.name)", "n.tags.any(x -> x == 0)")
            .replace(
                "nb.tags.any(x -> x.name == t)",
                "nb.tags.any(x -> x.name == \"{t}\")",
            )
            .replace("ui.Chip(t)", "ui.Chip(\"{t}\")")
            .replace("invoke(tag(plan, \"planning\"))", "invoke(tag(plan, 0))")
    }));
    session.settle();

    let note = session
        .instance_host()
        .package(NOTES)
        .and_then(|package| package.meta.types.into_iter().find(|ty| ty.name == "Note"))
        .unwrap();
    assert!(
        note.fields.iter().any(|field| field.ty == "Set<Int>"),
        "the new build runs: {note:?}"
    );
    let mut titles = session.titles();
    titles.sort();
    assert_eq!(titles, ["Home", "Plan"], "every note is still there");
    assert_eq!(
        session.field_value("title"),
        Some(text("Home")),
        "the window started over: it opens on the notebook's first note, not the one open before"
    );
}

impl Session {
    /// Hand the host `bundle` on its api, as a development session hands a reinstall over.
    fn take_build_over_the_api(&mut self, bundle: &tilolang_tilo::bundle::Bundle) {
        let module = runtime_portal::process_link::Module {
            wasm: bundle.wasm.clone(),
            ids: bundle.ids.clone(),
            imports: bundle
                .imports
                .iter()
                .map(|used| used.wasm.clone())
                .collect(),
            ..runtime_portal::process_link::Module::default()
        }
        .encode();
        let build = runtime_portal::process_link::AppBuild {
            app: bundle.meta.app.clone(),
            module_len: module.len() as u64,
        };
        let module = tilo_render_protocol::shm::sealed_blob("tilolang-module", &module).unwrap();
        self.host.take_build(&build, &module).unwrap();
    }
}

fn text(text: &str) -> tilo_app_window::Value {
    tilo_app_window::Value::Text(text.to_owned())
}
