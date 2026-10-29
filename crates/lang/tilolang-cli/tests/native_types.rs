//! A native app's published types read as tilolang: `tilolang describe` prints native Chat's
//! resources as tilolang types, and a package that `use`s Chat is type-checked against them.

use std::path::{Path, PathBuf};

use tilolang::ids::SeededEntropy;
use tilolang::source::{compile_dirs, dependency_order_with};
use tilolang_tilo::catalogue;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn chat_dir() -> PathBuf {
    repo().join("apps/chat")
}

/// The block of `text` that starts with the line `head` and ends at the closing brace.
fn block<'a>(text: &'a str, head: &str) -> &'a str {
    let start = text.find(head);
    assert!(start.is_some(), "no `{head}` in:\n{text}");
    let rest = text.get(start.unwrap_or_default()..).unwrap_or_default();
    let end = rest.find("\n}\n").map_or(rest.len(), |end| end + 2);
    rest.get(..end).unwrap_or_default()
}

#[test]
fn native_chat_describes_as_tilolang_types() {
    let text = tilolang_cli::describe::describe(&chat_dir(), &[]).expect("chat describes");
    println!("{text}");
    assert!(text.starts_with("app so.tilo.chat \"Chat\"\n"), "{text}");
    let workspace = block(&text, "type Workspace {");
    for line in [
        "    categories: Collection<Category>",
        "    channels: Collection<Channel>",
        "    people: Collection<Person>",
        "    agent_names: Map<Text>",
    ] {
        assert!(workspace.contains(line), "`{line}` in:\n{workspace}");
    }
    let channel = block(&text, "type Channel {");
    for line in [
        "    category: Option<Text>",
        "    name: Text",
        "    kind: ChannelKind",
        "    archived: Bool",
    ] {
        assert!(channel.contains(line), "`{line}` in:\n{channel}");
    }
    assert!(
        text.contains("enum ChannelKind {\n    text\n    voice\n}"),
        "{text}"
    );
    let messages = block(&text, "type Messages {");
    assert!(
        messages.contains("    messages: Collection<Message>"),
        "{messages}"
    );
    let message = block(&text, "type Message {");
    for line in [
        "    channel: Text",
        "    ts: Int",
        "    text: Text",
        "    edited: Bool",
        "    parent: Option<Text>",
    ] {
        assert!(message.contains(line), "`{line}` in:\n{message}");
    }
    // Reactions are the platform's and not read by tilolang code.
    assert!(!message.contains("reactions"), "{message}");
    assert!(
        text.contains("action create_category(workspace: Workspace, name: Text)"),
        "{text}"
    );
}

#[test]
fn a_tilolang_package_describes_through_the_same_catalogue() {
    let todo = repo().join("apps/tl/todo");
    let text = tilolang_cli::describe::describe(&todo, &[]).expect("todo describes");
    assert!(text.starts_with("app "), "{text}");
    assert!(text.contains("type "), "{text}");
    assert!(text.contains("action "), "{text}");
}

fn check(dir: &Path) -> Result<(), String> {
    let search = vec![chat_dir()];
    let natives = catalogue::native_apps(&search)?;
    let provided: Vec<String> = natives.iter().map(|meta| meta.app.clone()).collect();
    let dirs = dependency_order_with(dir, &search, &provided)?;
    compile_dirs(&dirs, &natives, &mut SeededEntropy(0))
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[test]
fn a_package_using_chat_reads_its_fields_in_a_typed_view() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/chat-reader");
    check(&dir).expect("the package type-checks against Chat's published types");
}

#[test]
fn a_field_chat_does_not_publish_is_an_error() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let dir = scratch.path().join("wrong");
    std::fs::create_dir(&dir).expect("package directory");
    std::fs::write(
        dir.join("app.tl"),
        "app so.tilo.test.wrong \"Wrong\"\n\nuse so.tilo.chat as chat\n\n\
         fn topic(c: chat.Channel) -> Int {\n    c.topic\n}\n\n\
         fn color(w: chat.Workspace) -> Text {\n    w.colour\n}\n",
    )
    .expect("source");
    let error = check(&dir).expect_err("wrong reads do not type-check");
    assert!(error.contains("colour"), "{error}");
    assert!(error.contains("Int") || error.contains("Text"), "{error}");
}
