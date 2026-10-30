//! Keeps English prose out of the shell's copy builders; the scan itself
//! lives in `tilo-copy-lint`, shared with the desktop.

/// Literals that look like prose but are not copy: ids, brand names,
/// protocol values. Each entry is `(path under src/, literal)`.
const ALLOWED: &[(&str, &str)] = &[
    // Default app, workspace and tab names are written to the replicated
    // desktop tree, then shown as the user's own.
    ("shell/state.rs", "Document"),
    ("shell/state.rs", "Files"),
    ("shell/state.rs", "Notes"),
    ("shell/state.rs", "Terminal"),
    // wlr-output-management head description; display tools match on it.
    ("compositor/output_heads.rs", "Virtual display"),
    // The developer window backend's title.
    ("compositor/backend/winit.rs", "tilo compositor blit"),
    // Error payloads returned to apps, peers and callers, not painted.
    (
        "compositor/render_node.rs",
        "set but empty (dmabuf advertisement disabled)",
    ),
    (
        "shell/collab_broker.rs",
        "the application broker is not running",
    ),
    (
        "shell/remote_action_link.rs",
        "the peer answered with an unexpected message",
    ),
    (
        "shell/remote_action_link.rs",
        "the peer did not answer in time",
    ),
    (
        "shell/remote_action_link.rs",
        "the peer did not finish the action in time",
    ),
    ("shell/foreign_ask.rs", "the device did not answer in time"),
    (
        "shell/launcher.rs",
        "the command does not fit a terminal launch",
    ),
    (
        "shell/launcher.rs",
        "selected open cannot borrow a host path or URI",
    ),
    // A launch context the desktop only builds for one web address: a
    // caller's broken invariant, refused before anything starts.
    (
        "shell/launcher.rs",
        "a visitor window opens one web address",
    ),
    (
        "shell/doc_open.rs",
        "this device's identity cannot vouch for the document's read",
    ),
    (
        "shell/remote_actions.rs",
        "native action sessions unavailable",
    ),
    (
        "shell/remote_actions.rs",
        "native action worker unavailable",
    ),
    (
        "shell/remote_actions.rs",
        "native action session limit reached",
    ),
    ("shell/tool_intents.rs", "invalid tool request"),
    ("shell/tool_intents.rs", "tool request is too large"),
    ("shell/tool_intents.rs", "unsupported tool protocol"),
    // Secret Service / Keychain item label, not shell UI copy.
    ("shell/mls_vault.rs", "Tilo MLS key"),
];

#[test]
fn copy_builders_take_catalogue_strings_not_english_literals() {
    tilo_copy_lint::CopyLint::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
        .allow(ALLOWED)
        // Developer binaries and test trees are not desktop copy.
        .skip_dirs(&["bin", "tests", "one_host_tests"])
        .skip_files(&[
            "tests.rs",
            "test_support.rs",
            // The test control sockets speak to test drivers, not people.
            "test_control.rs",
            "test_control_socket.rs",
            "dev_test_identity.rs",
        ])
        .min_files(100)
        .check();
}

/// A message that interpolates a variable is never resolved with `t`, which
/// passes none: it would show its `{ $… }` placeable to the person. Such
/// messages go through `t_args` (or a typed builder that always passes them).
#[test]
fn messages_with_variables_are_never_resolved_without_arguments() {
    let catalogue = include_str!("../../locales/en-US.ftl");
    let mut needs_arguments = std::collections::HashSet::new();
    let mut current: Option<&str> = None;
    for line in catalogue.lines() {
        if line.starts_with(char::is_whitespace) {
            if let Some(key) = current
                && line.contains('$')
            {
                needs_arguments.insert(key);
            }
            continue;
        }
        current = line
            .split_once('=')
            .map(|(key, _)| key.trim())
            .filter(|key| {
                !key.is_empty()
                    && key
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            });
        if let (Some(key), Some((_, value))) = (current, line.split_once('='))
            && value.contains('$')
        {
            needs_arguments.insert(key);
        }
    }
    assert!(needs_arguments.contains("launch-error-app-authorship-refused"));
    let source = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let mut offenders = Vec::new();
    for entry in walkdir::WalkDir::new(source) {
        let entry = entry.expect("the shell's sources are readable");
        if entry.path().extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(entry.path()).expect("a source file reads");
        let mut rest = text.as_str();
        while let Some(at) = rest.find("t(\"") {
            let before = rest[..at].chars().next_back();
            let after = &rest[at + 3..];
            rest = after;
            if before.is_some_and(|ch| ch.is_alphanumeric() || ch == '_') {
                continue;
            }
            let Some((key, tail)) = after.split_once('"') else {
                continue;
            };
            if tail.trim_start().starts_with(')') && needs_arguments.contains(key) {
                offenders.push(format!("{}: {key}", entry.path().display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "messages with variables resolved without them: {offenders:#?}"
    );
}
