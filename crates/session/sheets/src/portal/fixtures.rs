//! A question of every kind, for screenshots and headless tests.

use super::{
    Body, Device, Dialog, DraftRow, DraftValue, Prompt, PromptField, PromptKind, Remember, Shortcut,
};

pub(super) const SCENARIOS: &[&str] = &[
    "review",
    "app-choice",
    "action-grant",
    "store-review",
    "draft",
    "usb",
    "shortcuts",
    "launcher",
    "window-transfer",
    "drive",
    "shortcut-step",
    "authentication",
    "network",
    "sim-pin",
    "sim-puk",
    "cannot-join",
    "pairing",
    "allowed",
];

fn base(handle: &str, title: &str, heading: &str, detail: &str) -> Dialog {
    Dialog {
        handle: handle.to_owned(),
        title: title.to_owned(),
        heading: heading.to_owned(),
        detail: detail.to_owned(),
        confirm_label: "Allow".to_owned(),
        cancel_label: "Cancel".to_owned(),
        ..Dialog::default()
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

/// The question `scenario` shows.
#[allow(
    clippy::too_many_lines,
    reason = "one sample per kind of question, side by side"
)]
pub(super) fn dialog(scenario: Option<&str>) -> Dialog {
    match scenario {
        Some("app-choice") => Dialog {
            body: Body::Choices(strings(&["Text Editor", "Writer", "Browser"])),
            confirm_label: "Open".to_owned(),
            ..base(
                "app-choice",
                "Choose an app",
                "Files",
                "Files wants to open notes.md",
            )
        },
        Some("action-grant") => Dialog {
            body: Body::Choices(strings(&["Allow once", "Allow for 1 hour", "Always allow"])),
            ..base(
                "action-grant",
                "Let Scribe use Mail?",
                "Mail",
                "Sensitive: it acts for you where others can see it.",
            )
        },
        Some("store-review") => Dialog {
            body: Body::Choices(strings(&["Allow once"])),
            page: Some((1, 3)),
            ..base(
                "store-review",
                "Publish to the Store?",
                "Store",
                "Files\nso.tilo.notes 2.4.1\nhttps://store.tilo.so/apps/notes\nsigning key ed25519:4f2a…",
            )
        },
        Some("draft") => Dialog {
            body: Body::Draft {
                rows: vec![
                    DraftRow {
                        index: 0,
                        label: "To".to_owned(),
                        value: DraftValue::Text {
                            text: "ada@example.org".to_owned(),
                            multiline: false,
                            number: false,
                            withheld: false,
                        },
                        editable: true,
                    },
                    DraftRow {
                        index: 1,
                        label: "Message".to_owned(),
                        value: DraftValue::Text {
                            text: "See you at nine.".to_owned(),
                            multiline: true,
                            number: false,
                            withheld: false,
                        },
                        editable: true,
                    },
                    DraftRow {
                        index: 2,
                        label: "Urgent".to_owned(),
                        value: DraftValue::Flag(false),
                        editable: true,
                    },
                ],
                choices: strings(&["Allow once"]),
            },
            ..base(
                "draft",
                "Confirm Scribe's request",
                "Mail",
                "Sensitive. Check what it would send, and change it if you like.",
            )
        },
        Some("usb") => Dialog {
            body: Body::Devices(vec![
                Device {
                    label: "YubiKey 5C".to_owned(),
                    writable: true,
                    selected: true,
                },
                Device {
                    label: "SanDisk Ultra".to_owned(),
                    writable: false,
                    selected: false,
                },
            ]),
            ..base(
                "usb",
                "Use USB devices?",
                "Chromium",
                "Chromium asks to use these devices.",
            )
        },
        Some("shortcuts") => Dialog {
            body: Body::Shortcuts(vec![
                Shortcut {
                    name: "Push to talk".to_owned(),
                    keys: Some("Ctrl + Shift + T".to_owned()),
                    status: None,
                },
                Shortcut {
                    name: "Mute".to_owned(),
                    keys: None,
                    status: Some("Tilo keeps that shortcut".to_owned()),
                },
            ]),
            confirm_label: "Save shortcuts".to_owned(),
            ..base(
                "shortcuts",
                "Global shortcuts",
                "Voice",
                "Press the keys for a shortcut; Backspace clears it.",
            )
        },
        Some("launcher") => Dialog {
            body: Body::Launcher {
                name: Some("Tilo Docs".to_owned()),
                icons: strings(&["Supplied image (validated)", "Tilo monogram"]),
                icon_editable: true,
            },
            confirm_label: "Add".to_owned(),
            ..base(
                "launcher",
                "Add to launcher?",
                "Browser",
                "https://docs.tilo.so",
            )
        },
        Some("window-transfer") => Dialog {
            body: Body::Choices(strings(&[
                "Move it: the document leaves Design",
                "Copy it: Design keeps the original",
            ])),
            confirm_label: "Carry over".to_owned(),
            ..base(
                "window-transfer",
                "Carry Roadmap into Home?",
                "Notes",
                "People in Design lose access to what moves.",
            )
        },
        Some("drive") => Dialog {
            body: Body::Answers {
                choices: strings(&["Open read-only", "Open and allow changes"]),
                suggested: 0,
                remember: Some(Remember {
                    label: "Remember for this drive".to_owned(),
                    on: false,
                }),
            },
            cancel_label: "Not now".to_owned(),
            ..base(
                "drive",
                "Open this Windows drive?",
                "External drive",
                "Changes made here can confuse Windows if it was not shut down.",
            )
        },
        Some("shortcut-step") => Dialog {
            body: Body::Entry {
                masked: false,
                required: true,
                choices: Vec::new(),
            },
            confirm_label: "Verify".to_owned(),
            ..base(
                "shortcut-step",
                "Check this device",
                "Pairing",
                "Type the six digits the other device shows.",
            )
        },
        Some("authentication") => Dialog {
            body: Body::Secret(Prompt {
                icon: Some(ui::icons::Icon::Shield),
                resource: "Authenticating as ada\norg.freedesktop.packagekit.package-install"
                    .to_owned(),
                fields: vec![PromptField {
                    label: "Password".to_owned(),
                    masked: true,
                    required: false,
                    initial: String::new(),
                }],
                primary_label: "Authenticate".to_owned(),
                ..Prompt::default()
            }),
            ..base(
                "authentication",
                "Authentication is required to install software",
                "Administrator authentication",
                "",
            )
        },
        Some("network") => Dialog {
            body: Body::Secret(Prompt {
                icon: Some(ui::icons::Icon::Wifi),
                resource: "Home Wi-Fi".to_owned(),
                fields: vec![PromptField {
                    label: "Password".to_owned(),
                    masked: true,
                    required: true,
                    initial: String::new(),
                }],
                option: Some(("Remember on this device".to_owned(), true)),
                primary_label: "Connect".to_owned(),
                ..Prompt::default()
            }),
            ..base("network", "Wi-Fi password needed", "Network", "")
        },
        Some("sim-pin") => Dialog {
            body: Body::Secret(Prompt {
                kind: PromptKind::SimPin {
                    attempts_left: Some(3),
                },
                resource: "Mobile broadband".to_owned(),
                fields: vec![PromptField {
                    label: "SIM PIN".to_owned(),
                    masked: true,
                    required: true,
                    initial: String::new(),
                }],
                primary_label: "Unlock".to_owned(),
                ..Prompt::default()
            }),
            ..base("sim-pin", "Unlock the SIM", "Network", "")
        },
        Some("sim-puk") => Dialog {
            body: Body::Secret(Prompt {
                kind: PromptKind::SimPuk {
                    attempts_left: Some(10),
                },
                resource: "Mobile broadband".to_owned(),
                fields: vec![PromptField::default(); 3],
                primary_label: "Unlock".to_owned(),
                ..Prompt::default()
            }),
            ..base("sim-puk", "The SIM is locked", "Network", "")
        },
        Some("cannot-join") => Dialog {
            body: Body::Choices(strings(&["Enter password"])),
            confirm_label: "Enter password".to_owned(),
            ..base(
                "cannot-join",
                "Couldn't join Cafe",
                "Cafe",
                "Enter the password to connect.",
            )
        },
        Some("pairing") => Dialog {
            title: "Does Keyboard show this code?".to_owned(),
            heading: "Keyboard".to_owned(),
            detail: "Compare it with the code on the other device. If they differ, \
                     something else is trying to pair, so say no."
                .to_owned(),
            code: Some("123456".to_owned()),
            body: Body::Answers {
                choices: Vec::new(),
                suggested: 0,
                remember: None,
            },
            confirm_label: "They match".to_owned(),
            cancel_label: "Don't pair".to_owned(),
            ..Dialog::default()
        },
        Some("allowed") => Dialog {
            title: "Allowed for good".to_owned(),
            heading: "Meet".to_owned(),
            detail: "Meet may now use your screen without asking. \
                     Revoke it to make Tilo ask again next time."
                .to_owned(),
            confirm_label: "Revoke".to_owned(),
            cancel_label: "Keep".to_owned(),
            ..Dialog::default()
        },
        _ => Dialog {
            confirm_label: "Share".to_owned(),
            ..base(
                "review",
                "Share your screen?",
                "Meet",
                "Meet asks to record your screen\nEntire screen",
            )
        },
    }
}
