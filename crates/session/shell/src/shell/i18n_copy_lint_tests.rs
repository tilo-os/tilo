//! Keeps English prose out of the shell's copy builders; the scan itself
//! lives in `tilo-copy-lint`, shared with the desktop.

/// Literals that look like prose but are not copy: ids, brand names,
/// protocol values. Each entry is `(path under src/, literal)`.
const ALLOWED: &[(&str, &str)] = &[
    // Default app, workspace and tab names are written to the replicated
    // desktop tree, then shown as the user's own.
    ("shell/state.rs", "Available on another device"),
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
        "shell/launcher.rs",
        "the command does not fit a terminal launch",
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
        .skip_dirs(&["bin", "tests"])
        .skip_files(&[
            "tests.rs",
            "test_support.rs",
            // The test control sockets speak to test drivers, not people.
            "test_control.rs",
            "test_control_socket.rs",
            "hosted_test_control.rs",
            "dev_test_identity.rs",
        ])
        .min_files(100)
        .check();
}
