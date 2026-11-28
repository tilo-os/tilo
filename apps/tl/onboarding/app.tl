app so.tilo.onboarding "Setup" (
    icon: .user,
    category: .system,
    tagline: "Sets up who you are on this computer",
    keywords: ["setup", "onboarding", "identity", "recovery key", "backup"],
)

use so.tilo.session as session

// Setup is a view of the session's `setup` resource: the step the person is at and what that step
// shows. It holds nothing of its own but what is typed into its fields; every press asks the
// ceremony through setup's actions, which an agent or a script here calls the same way. A secret
// typed here goes to the ceremony as it is typed (`enter_secret`) and never comes back in the
// reading: when the ceremony forgets it, the session opens this view again with its fields empty.
// ---------------------------------------------------------------- sizes
// The card's measure and its preferred heights, in logical pixels. A short display, and large
// text, take what is left of the screen inside its gutter; the body scrolls inside the card when
// its words are taller.
fn panel_width() -> Float {
    760.0
}

fn gutter() -> Float {
    24.0
}

/// How tall the card stands on a step, or 0.0 where it takes the height of what it holds (the
/// hand-off, the choice of an existing identity).
fn panel_height(s: session.Setup, large: Bool) -> Float {
    if s.step == "tour" {
        if large { 672.0 } else { 640.0 }
    } else if s.step == "ready" or s.step == "existing" and not s.recovery_only {
        0.0
    } else if large {
        672.0
    } else if s.step == "intro" {
        520.0
    } else {
        // Restore, protection, backup, the recovery key and the formats offer hold dense
        // decisions and controls.
        560.0
    }
}

/// Where the step stands among setup's pages, for the slide from one to the next: a deeper page
/// arrives from the trailing side, going back brings the earlier one from the leading side.
fn page_level(s: session.Setup) -> Int {
    if s.step == "tour" {
        3 + s.tour
    } else if s.step == "intro" {
        0
    } else if s.step == "existing" or s.step == "protect" {
        1
    } else if s.step == "nearby" or s.step == "restore" or s.step == "recovery-key" { 2 } else { 3 }
}

// ---------------------------------------------------------------- what the words say
/// The first groups of a fingerprint: enough to tell identities apart by eye.
fn short_fingerprint(fingerprint: Text) -> Text {
    let groups = [0, 1, 2, 3]
        .filter(i -> i * 4 < fingerprint.len())
        .map(i -> fingerprint.slice(i * 4, (i * 4 + 4).min(fingerprint.len())))
    "{groups.join(" ")} …"
}

/// The step's number in the header, and its stage's name.
fn step_number(s: session.Setup) -> Int {
    if s.recovery_only and s.step == "existing" {
        1
    } else if s.recovery_only {
        2
    } else if s.step == "intro" {
        1
    } else if s.step == "codecs" {
        4
    } else if s.step == "ready" {
        s.total_steps
    } else if s.step == "backup" or s.step == "recovery-key-copy" { 3 } else { 2 }
}

fn stage_name(s: session.Setup) -> Text {
    if s.recovery_only and s.step == "existing" {
        "Choose"
    } else if s.recovery_only {
        "Restore"
    } else if s.step == "intro" {
        "Choose"
    } else if s.step == "existing" or s.step == "nearby" {
        "Connect"
    } else if s.step == "restore" or s.step == "recovery-key" {
        "Restore"
    } else if s.step == "protect" {
        "Protect"
    } else if s.step == "codecs" { "Media" } else if s.step == "ready" { "Ready" } else { "Backup" }
}

fn eyebrow(s: session.Setup) -> Text {
    if s.recovery_only {
        "Identity recovery"
    } else if s.step == "nearby" or s.joined { "Device setup" } else { "Tilo identity setup" }
}

fn tour_stage(s: session.Setup) -> Text {
    if s.tour == 1 {
        "Move"
    } else if s.tour == 2 { "Arrange" } else if s.tour == 3 { "Share" } else { "Begin" }
}

/// The position (from 1) of the `i`th word the person types back.
fn check_at(s: session.Setup, i: Int) -> Int {
    if let position = s.key_checks.at(i) { position } else { 0 }
}

// ---------------------------------------------------------------- the formats offer
fn codecs_failed(s: session.Setup) -> Bool {
    s.codecs_error != "" or s.codecs_state == "failed"
}

/// An install was asked for from this page, or the pack is past not being installed.
fn codecs_requested(s: session.Setup) -> Bool {
    s.codecs_running or s.codecs_error != "" or s.codecs_state != "not-installed"
}

/// The page offers its own choice while nothing runs: Install (or Try again) and the quiet way
/// past it. Once a request is under way or done, the footer offers Continue instead.
fn codecs_choice(s: session.Setup) -> Bool {
    codecs_failed(s) and not s.codecs_running or not codecs_requested(s)
}

/// What happened to the request, if anything: the banner's title.
fn codecs_outcome_title(s: session.Setup) -> Text {
    if s.codecs_error != "" {
        "Could not install more formats"
    } else if s.codecs_state == "not-installed" and s.codecs_running {
        "Starting…"
    } else if s.codecs_state == "installing" {
        "Installing more formats"
    } else if s.codecs_state == "waiting-for-network" {
        "Installs once you are online"
    } else if s.codecs_state == "installed" {
        "More video and audio formats are on"
    } else if s.codecs_state == "failed" { "Could not install more formats" } else { "" }
}

fn codecs_outcome_message(s: session.Setup) -> Text {
    if s.codecs_error != "" {
        s.codecs_error
    } else if s.codecs_state == "not-installed" and s.codecs_running {
        "Setup can continue while the download runs. You will get a notification when it is ready."
    } else if s.codecs_state == "installing" {
        "{s.codecs_summary} Setup can continue while the download runs. You will get a notification when it is ready."
    } else if s.codecs_state == "waiting-for-network" {
        "This device is not connected to the internet. Your choice is saved: the download starts by itself once it is online, and you will get a notification."
    } else if s.codecs_state == "installed" {
        s.codecs_when
    } else if s.codecs_state == "failed" {
        if s.codecs_last_error == "" {
            "Try again, or later from Settings, Updates."
        } else {
            "{s.codecs_last_error} Try again, or later from Settings, Updates."
        }
    } else { "" }
}

/// How far an install got, while it installs: 0 until the helper says.
fn codecs_progress(s: session.Setup) -> Float {
    if let fraction = s.codecs_fraction { fraction } else { 0.0 }
}

fn codecs_progress_shows(s: session.Setup) -> Bool {
    s.codecs_state == "installing" or s.codecs_state == "not-installed" and s.codecs_running
}

// ---------------------------------------------------------------- the view
/// Setup as the person goes through it: the trusted ground with the step's card on it, or the
/// tour's card over the desktop it teaches.
view setup(s: session.Setup) {
    // What is typed into setup's fields, each sent to the ceremony as it changes.
    state backup_passphrase: Text = ""
    // The backup file's path as typed, and how many picks in the system's picker it has taken in.
    state path: Text = ""
    state picked: Int = 0
    state code: Text = ""
    state new_passphrase: Text = ""
    state confirmation: Text = ""
    state words: Text = ""
    state first: Text = ""
    state second: Text = ""
    state third: Text = ""
    // Large text needs the room a dense page would otherwise scroll for.
    let large = space.rem > 16.0
    let room_width = space.width * space.rem - 2.0 * gutter()
    let room_height = space.height * space.rem - 2.0 * gutter()
    let width = room_width.min(panel_width()).max(0.0)
    let height = room_height.min(panel_height(s, large)).max(0.0)
    layers(fill) {
        if s.step == "tour" {
            // The tour is modal over the real desktop, which stays legible under the scrim.
            ui.Backdrop(ground: .scrim)
        } else {
            // Setup's own ground, which no app can draw.
            ui.Backdrop(ground: .trusted)
        }
        row(fill, align: .center, distribute: .center) {
            if height > 0.0 {
                stack(height: height * 1dp) {
                    setup_card(
                        s,
                        width,
                        true,
                        backup_passphrase,
                        path,
                        picked,
                        code,
                        new_passphrase,
                        confirmation,
                        words,
                        first,
                        second,
                        third,
                    )
                }
            } else {
                setup_card(
                    s,
                    width,
                    false,
                    backup_passphrase,
                    path,
                    picked,
                    code,
                    new_passphrase,
                    confirmation,
                    words,
                    first,
                    second,
                    third,
                )
            }
        }
    }
}

/// Where the person is in setup or its tour, in words: the stage the track over the page marks.
fn stage_described(s: session.Setup) -> Text {
    if s.step == "tour" {
        "Desktop tour, step {s.tour} of 4: {tour_stage(s)}"
    } else {
        "Identity setup, step {step_number(s)} of {s.total_steps}: {stage_name(s)}"
    }
}

/// The card: the progress header, the step's page, which scrolls when it is taller than the card,
/// and the footer of its ways forward, which stays in view.
view setup_card(
    s: session.Setup,
    width: Float,
    tall: Bool,
    backup_passphrase: State<Text>,
    path: State<Text>,
    picked: State<Int>,
    code: State<Text>,
    new_passphrase: State<Text>,
    confirmation: State<Text>,
    words: State<Text>,
    first: State<Text>,
    second: State<Text>,
    third: State<Text>,
) {
    ui.FloatingPanel(
        width: width,
        fill_height: tall,
        label: if s.step == "tour" { "Desktop tour" } else { eyebrow(s) },
        description: stage_described(s),
    ) {
        body: {
            // The guided page draws where the person is, the page under it that scrolls, and the
            // ways forward that stay in view.
            ui.GuidedPage(
                stage: if s.step == "tour" { "Desktop tour" } else { eyebrow(s) },
                position: if s.step == "tour" {
                    "Step {s.tour} of 4"
                } else {
                    "Step {step_number(s)} of {s.total_steps}"
                },
                progress: if s.step == "tour" {
                    s.tour.to_float() / 4.0
                } else {
                    step_number(s).to_float() / s.total_steps.max(1).to_float()
                },
                progress_name: stage_described(s),
                scroll_label: if s.step == "tour" { tour_stage(s) } else { stage_name(s) },
                shows_bar: s.step != "backup",
                reveals_focus: s.step == "backup",
                fills_height: tall,
            ) {
                // A form keeps the field Tab reaches in view; a page of explanation keeps its bar
                // showing while there is more below.
                body: {
                    ui.PageSlide(level: page_level(s)) {
                        stack(gap: .section) {
                            if s.progress_error != "" {
                                ui.Banner(
                                    "Setup progress needs attention",
                                    message: s.progress_error,
                                    kind: .error,
                                )
                            }
                            if s.step == "tour" {
                                tour_page(s)
                            } else if s.step == "intro" {
                                intro_page(s)
                            } else if s.step == "existing" {
                                existing_page(s)
                            } else if s.step == "nearby" {
                                nearby_page(s, code)
                            } else if s.step == "restore" {
                                restore_page(s, backup_passphrase, path, picked)
                            } else if s.step == "recovery-key" {
                                recovery_key_page(s, words)
                            } else if s.step == "protect" {
                                protect_page(s)
                            } else if s.step == "recovery-key-copy" {
                                key_copy_page(s, first, second, third)
                            } else if s.step == "backup" {
                                backup_page(s, new_passphrase, confirmation)
                            } else if s.step == "codecs" {
                                codecs_page(s)
                            } else {
                                done_page(s)
                            }
                        }
                    }
                }
                actions: {
                    footer(s)
                }
            }
        }
    }
}

/// The title at the top of a page.
view page_title(text: Text) {
    ui.Text(text, role: .large_title)
}

/// A page's explanation, in secondary ink.
view paragraph(text: Text) {
    ui.Text(text, secondary: true)
}

/// A right-aligned row of the page's ways forward, so the primary one lands where the eye
/// finishes reading.
view actions(content: View) {
    row(gap: .related, align: .center) {
        spacer
        content
    }
}

/// Back, which Escape presses too while the step can still go back.
view back_button(s: session.Setup) {
    ui.Button("Back", icon: .arrow_left, key: "Escape") {
        on press: session.back(s)
    }
}

// ---------------------------------------------------------------- the pages
/// What an identity is and what it enables.
view intro_page(s: session.Setup) {
    stack(gap: .section) {
        if s.host_desktop {
            page_title("Set up your Tilo identity")
            paragraph(
                "You are already signed in to this computer. Before the Tilo Desktop opens, it needs a local identity for collaboration and for invitations between devices. This does not replace your Linux login or create an online account. Individual app edits are not all cryptographically signed yet.",
            )
        } else {
            page_title("Set up your Tilo identity")
            paragraph(
                "You get an identity that belongs to you, and this device gets its own key. Collaboration and invitations rely on it. Not every edit in apps is signed yet.",
            )
        }
        stack(gap: .sibling) {
            row(gap: .sibling, align: .start) {
                ui.Icon(.users, secondary: true)
                ui.Text(
                    "Members see one stable identity for you, and it vouches for each of your devices.",
                )
            }
            row(gap: .sibling, align: .start) {
                ui.Icon(.hard_drive, secondary: true)
                ui.Text(
                    "It is created on this computer. Nothing is uploaded and no server keeps a copy.",
                )
            }
            row(gap: .sibling, align: .start) {
                ui.Icon(.lock, secondary: true)
                ui.Text("It is not an account and has no password. There is nothing to sign in to.")
            }
        }
    }
}

/// How to bring an identity that already exists to this computer, or, on a computer that already
/// belongs to someone, what happened to its identity and that nothing was replaced.
view existing_page(s: session.Setup) {
    stack(gap: .section) {
        if s.recovery_only {
            page_title("Bring your identity back to this computer")
            paragraph(
                if s.recovery_problem == "keyring" {
                    "Tilo couldn't reach the keyring that holds your identity's key and changed nothing. Unlock the keyring, then choose Try again."
                } else if s.recovery_problem == "missing" {
                    "This computer's copy of your identity is missing. Tilo changed nothing, and your workspaces and other devices are fine."
                } else if s.recovery_problem == "invalid" {
                    "Tilo couldn't verify this computer's copy of your identity and changed nothing. If the date or time is wrong, fix it, then choose Try again."
                } else {
                    "Tilo couldn't open this computer's copy of your identity and changed nothing. Your workspaces and other devices are fine. If your keyring is locked, unlock it, then choose Try again."
                },
            )
            if s.expected_identity != "" {
                // Matches the start of the fingerprint the person's other devices show.
                ui.Text(
                    "This computer's identity: {short_fingerprint(s.expected_identity)}",
                    role: .label,
                    secondary: true,
                    numeric: true,
                )
            }
        } else {
            page_title("Use your existing identity")
            paragraph("Choose how to bring the identity you already use to this computer.")
        }
        if s.error != "" {
            ui.Banner("That identity could not be connected", message: s.error, kind: .error)
        }
    }
}

/// Add this computer with the code another of the person's devices printed, and the wait while
/// that device adds it.
template view nearby_page(s: session.Setup, code: State<Text>) {
    stack(gap: .section) {
        page_title("Add this computer with a code")
        paragraph(
            "On a device that already has your identity, run tilo device add. It prints a code: type it here.",
        )
        if s.error != "" {
            // The banner's title names the cause, which a screen reader speaks first.
            if s.nearby_failure_title != "" {
                ui.Banner(s.nearby_failure_title, message: s.nearby_failure_message, kind: .error)
            } else {
                ui.Banner("This computer was not added", message: s.error, kind: .error)
            }
        }
        ui.Section {
            ui.Labelled("Code from your other device") {
                ui.TextInput(
                    bind: code,
                    placeholder: "tilo1…",
                    verbatim: true,
                    read_only: s.nearby_active,
                ) {
                    on changed(typed): session.enter_secret(s, field: "nearby-code", value: typed)
                }
            }
        }
        if s.nearby_active {
            row(gap: .related, align: .center) {
                ui.Spinner(beside: .body)
                ui.Text(
                    "Waiting for your other device to add this computer…",
                    secondary: true,
                    live: .polite,
                )
            }
        }
    }
}

/// Restore the identity from an encrypted backup file and its passphrase.
template view restore_page(
    s: session.Setup,
    backup_passphrase: State<Text>,
    path: State<Text>,
    picked: State<Int>,
) {
    stack(gap: .section) {
        page_title("Restore from a backup")
        paragraph("Choose the encrypted backup you saved from Tilo and enter its passphrase.")
        if s.error != "" {
            ui.Banner("That identity could not be connected", message: s.error, kind: .error)
        }
        stack(gap: .tight) {
            ui.Text("Backup file", role: .label)
            row(gap: .related, align: .center) {
                // Typed by hand or chosen in the system's picker. A pick replaces what is typed
                // once; the ceremony's path never overwrites the field while the person types.
                ui.TextInput(
                    bind: path,
                    placeholder: "Choose an encrypted backup or legacy identity JSON",
                    verbatim: true,
                    fill: 1.0,
                ) {
                    on changed(typed): session.enter(s, field: "backup-file", value: typed)
                }
                if s.connect_picked != picked {
                    ui.Timer(after: 0.01, restart: "{s.connect_picked}") {
                        on tick: set(path, s.connect_path).then(set(picked, s.connect_picked))
                    }
                }
                ui.Button("Browse", icon: .folder_open) {
                    on press: session.browse(s)
                }
            }
        }
        stack(gap: .tight) {
            ui.Text("Backup passphrase", role: .label)
            ui.TextInput(
                bind: backup_passphrase,
                placeholder: "Passphrase (leave empty for legacy JSON)",
                secret: true,
            ) {
                on changed(typed): session.enter_secret(s, field: "backup-passphrase", value: typed)
            }
        }
    }
}

/// Restore the identity from the 24 words of its recovery key, checked as they are typed so a
/// mistake shows where it is.
template view recovery_key_page(s: session.Setup, words: State<Text>) {
    stack(gap: .section) {
        page_title("Enter your recovery key")
        paragraph(
            "Type or paste the 24 words in order. Capital letters, line breaks and numbering don't matter.",
        )
        if s.error != "" {
            ui.Banner("Couldn't restore from this recovery key", message: s.error, kind: .error)
        }
        stack(gap: .tight) {
            ui.Text("Recovery key", role: .label)
            ui.TextInput(
                bind: words,
                placeholder: "word1 word2 word3 …",
                verbatim: true,
                lines: 4,
                mistakes: s.words_mistakes,
                read_only: s.busy,
            ) {
                // The words stay put while they are being restored.
                on changed(typed): session.enter_secret(s, field: "recovery-key", value: typed)
            }
        }
        if s.words_check == "typing" {
            ui.Text("{s.words_typed} of {s.words_total} words", secondary: true, numeric: true)
        } else if s.words_check == "problem" {
            ui.Banner(s.words_problem, kind: .warning, icon: .pencil)
        } else if s.words_check == "ready" {
            ui.Banner(
                "Recovery key for identity {short_fingerprint(s.words_fingerprint)}",
                message: if s.expected_identity != "" {
                    "It matches this computer's identity. Choose Restore identity to bring it back."
                } else {
                    "Choose Restore identity to use it on this computer."
                },
                kind: .success,
            )
        } else if s.words_check == "different" {
            ui.Banner(
                "These words belong to a different identity",
                message: "They restore {short_fingerprint(s.words_fingerprint)}, but this computer belongs to {short_fingerprint(s.expected_identity)}, so check that you're using this computer's recovery key.",
                kind: .error,
            )
        }
        if s.busy {
            row(gap: .related, align: .center) {
                ui.Spinner(beside: .body)
                ui.Text("Restoring your identity…", secondary: true, live: .polite)
            }
        }
    }
}

/// The consequence of losing the device, stated as a fact before the identity is made.
view protect_page(s: session.Setup) {
    stack(gap: .section) {
        stack(gap: .sibling) {
            page_title("Protect your Tilo identity")
            ui.Banner(
                "If you lose this device without a backup, this identity is gone",
                message: "There is no reset button. Write down a recovery key or save an encrypted backup after this step.",
                kind: .warning,
                icon: .shield,
            )
        }
        if s.error != "" {
            ui.Banner("Your identity was not created", message: s.error, kind: .error)
        }
        stack(gap: .sibling) {
            row(gap: .sibling, align: .start) {
                ui.Icon(.ban, secondary: true)
                ui.Text(
                    "There is no recovery service. Nobody, including Tilo, can reset or recreate this identity for you.",
                )
            }
            row(gap: .sibling, align: .start) {
                ui.Icon(.archive, secondary: true)
                ui.Text(
                    "Next, write down a recovery key or save an encrypted backup. Either one brings this identity back. Without one, a lost or wiped device needs a new identity; workspace data remains, but the old identity is no longer yours.",
                )
            }
        }
    }
}

/// The identity exists: keep a way to bring it back, a recovery key on paper or an encrypted file.
template view backup_page(
    s: session.Setup,
    new_passphrase: State<Text>,
    confirmation: State<Text>,
) {
    if s.error != "" {
        stack(gap: .group) {
            ui.Banner("Your identity could not be created", message: s.error, kind: .error)
            paragraph(
                "Nothing was written and the desktop is still protected. Go back and try again after checking storage and permissions.",
            )
            actions {
                back_button(s)
            }
        }
    } else if not s.backup_available {
        done_page(s)
    } else {
        stack(gap: .section) {
            page_title("Back up your Tilo identity")
            // A computer set up for someone who already uses Tilo joins the person they have, with
            // a code from one of their devices, instead of keeping the identity made here.
            if not s.recovery_only and not s.portable_backup_only {
                ui.Section {
                    ui.ListRow(
                        "I already use Tilo on another device",
                        icon: .monitor,
                        secondary: "Join this computer to your existing person with a code from one of your devices",
                        description: "Join this computer to your existing person with a code from one of your devices",
                        disclosure: true,
                    ) {
                        on activate: session.choose(s, option: "nearby")
                    }
                }
            }
            if s.backup_error != "" {
                ui.Banner("The backup was not saved", message: s.backup_error, kind: .error)
            } else if s.backup_saved != "" or s.backup_verified {
                if s.key_written and s.backup_saved == "" {
                    ui.Banner(
                        "Recovery key written down",
                        message: "With those 24 words you can bring this identity back on any computer.",
                        kind: .success,
                        icon: .check,
                    )
                } else if s.backup_saved != "" {
                    ui.Banner(
                        "Encrypted backup verified",
                        message: s.backup_saved,
                        kind: .success,
                        icon: .check,
                    )
                } else {
                    ui.Banner(
                        "Encrypted backup verified",
                        message: "The verified backup was saved before setup restarted.",
                        kind: .success,
                        icon: .check,
                    )
                }
            }
            // The quicker way to keep the identity: 24 words on paper. Offered first and beside
            // the file, never instead of it, because the two suit different people.
            if not s.key_written and (not s.backup_verified or s.backup_form_open) {
                ui.Section {
                    ui.ListRow(
                        "Write down a recovery key",
                        icon: .list_ordered,
                        secondary: "24 words on paper bring this identity back. No file or passphrase needed.",
                        description: "24 words on paper bring this identity back. No file or passphrase needed.",
                        disclosure: true,
                    ) {
                        on activate: session.choose(s, option: "write-down-recovery-key")
                    }
                }
            }
            if not s.backup_verified or s.backup_form_open {
                stack(gap: .sibling) {
                    paragraph(
                        "Or save an encrypted backup file and keep it apart from its passphrase.",
                    )
                    stack(gap: .tight) {
                        ui.Text("New backup passphrase", role: .label)
                        ui.TextInput(
                            bind: new_passphrase,
                            placeholder: "At least 12 characters, long and unique",
                            secret: true,
                        ) {
                            on changed(typed): session.enter_secret(
                                s,
                                field: "new-passphrase",
                                value: typed,
                            )
                        }
                    }
                    stack(gap: .tight) {
                        ui.Text("Confirm new backup passphrase", role: .label)
                        ui.TextInput(
                            bind: confirmation,
                            placeholder: "Confirm passphrase",
                            secret: true,
                        ) {
                            on changed(typed): session.enter_secret(
                                s,
                                field: "new-passphrase-confirmation",
                                value: typed,
                            )
                        }
                    }
                    actions {
                        if s.backup_verified {
                            // Continue stays the clear way on after a verified backup; another
                            // copy is an optional follow-up.
                            ui.Button("Save another copy", role: .secondary) {
                                on press: session.browse(s)
                            }
                        } else {
                            ui.Button("Choose location and save backup…", role: .primary) {
                                on press: session.browse(s)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The 24 words, numbered down each column as people write a list on paper, and three of them to
/// type back once they are written down.
template view key_copy_page(
    s: session.Setup,
    first: State<Text>,
    second: State<Text>,
    third: State<Text>,
) {
    protect capture
    let rows = (s.key_words.len() + 3) / 4
    stack(gap: .section) {
        page_title("Write down your recovery key")
        paragraph(
            "Write them on paper, in order. Anyone with these words can become you, so don't photograph them.",
        )
        ui.Section {
            stack(gap: .related) {
                for r in s.key_words.map((w, i) -> i).filter(i -> i < rows) {
                    row(gap: .sibling) {
                        for c in [0, 1, 2, 3] {
                            key_word(s, c * rows + r)
                        }
                    }
                }
            }
        }
        stack(gap: .related) {
            paragraph(
                "To check your copy, type words {check_at(s, 0)}, {check_at(s, 1)} and {check_at(s, 2)}.",
            )
            row(gap: .sibling) {
                key_answer(s, 0, first)
                key_answer(s, 1, second)
                key_answer(s, 2, third)
            }
        }
    }
}

/// One word of the key under its number; a fixed column keeps the words aligned past the
/// single-digit numbers.
view key_word(s: session.Setup, at: Int) {
    row(gap: .related, align: .center, fill: 1.0) {
        if at < s.key_words.len() {
            stack(min: 2rem, max: 2rem) {
                ui.Text("{at + 1}", role: .label, secondary: true, numeric: true)
            }
            if let word = s.key_words.at(at) {
                ui.Text(word)
            }
        }
    }
}

/// A word typed back, under its number: marked whole once it can no longer be the word asked for.
template view key_answer(s: session.Setup, i: Int, typed: State<Text>) {
    stack(gap: .tight, fill: 1.0) {
        ui.Text("Word {check_at(s, i)}", role: .label)
        ui.TextInput(
            bind: typed,
            verbatim: true,
            mistakes: if s.key_wrong.at(i) == .some(true) { [0, typed.len()] } else { [] },
        ) {
            on changed(answer): session.enter_secret(
                s,
                field: "recovery-key-word-{i + 1}",
                value: answer,
            )
        }
    }
}

/// The offer to play more formats: a centred title and one short line over a single large Install
/// button, with a quiet way past it. Nothing is chosen until the person presses Install; once they
/// did, the live result takes the button's place.
view codecs_page(s: session.Setup) {
    stack(gap: .section) {
        row(distribute: .center) {
            ui.Icon(.film, size: .large, accent: true)
        }
        ui.Text("Play more video and audio formats", role: .display, align: .center)
        ui.Text(
            "Downloaded to this device from the formats' publishers. You can turn it off in Settings.",
            secondary: true,
            align: .center,
        )
        if codecs_outcome_title(s) != "" {
            ui.Banner(
                codecs_outcome_title(s),
                message: codecs_outcome_message(s),
                kind: if codecs_failed(s) {
                    .error
                } else if s.codecs_state == "installed" {
                    .success
                } else if s.codecs_state == "waiting-for-network" { .warning } else { .info },
                icon: if s.codecs_state == "installed" and not codecs_failed(s) { .check },
            )
        }
        if codecs_progress_shows(s) {
            ui.ProgressBar(codecs_progress(s), accessible_name: "Installing more formats")
        }
        if codecs_choice(s) {
            // The one way in is this button. It holds the keyboard, so Enter is the same
            // deliberate press; it starts the download and never moves past the page by itself.
            row(distribute: .center) {
                ui.Button(
                    if codecs_failed(s) { "Try again" } else { "Install" },
                    icon: .download,
                    role: .primary,
                    size: .large,
                    default_focus: true,
                ) {
                    on press: session.choose(s, option: "install-codecs")
                }
            }
            row(distribute: .center) {
                ui.Button(if codecs_failed(s) { "Skip" } else { "Not now" }, role: .plain) {
                    on press: session.skip(s)
                }
            }
        }
    }
}

/// The hand-off: the identity is ready, or this device joined someone's.
view done_page(s: session.Setup) {
    stack(gap: .section) {
        if s.joined {
            page_title(
                if s.joined_label != "" and s.joined_person != "" {
                    "{s.joined_label} is now one of {s.joined_person}’s devices."
                } else { "Device added" },
            )
        } else {
            page_title("Welcome to Tilo")
            paragraph(
                "The desktop is ready. Start with anything: open an app, search for what you need, or invite someone to a workspace.",
            )
        }
        // The ways to make Tilo the person's own: what the system they ran before kept, the
        // accounts they already use, and the apps they want. Each leaves setup and opens there.
        ui.Section(title: "Make Tilo yours") {
            if s.old_system != "" {
                ui.ListRow(
                    "Bring your things from {s.old_system}",
                    icon: .hard_drive,
                    secondary: "Folders, browsers and mail come over. Nothing on that disk changes.",
                    description: "Folders, browsers and mail come over. Nothing on that disk changes.",
                    disclosure: true,
                ) {
                    on activate: session.choose(s, option: "migrate")
                }
            }
            ui.ListRow(
                "Connect your accounts",
                icon: .user,
                secondary: "Mail, calendars and cloud storage you already use",
                description: "Mail, calendars and cloud storage you already use",
                disclosure: true,
            ) {
                on activate: session.choose(s, option: "accounts")
            }
            ui.ListRow(
                "Choose apps in the Store",
                icon: .store,
                secondary: "Add what you need beside the apps Tilo comes with",
                description: "Add what you need beside the apps Tilo comes with",
                disclosure: true,
            ) {
                on activate: session.choose(s, option: "store")
            }
        }
    }
}

// ---------------------------------------------------------------- the tour
/// A page of the tour: a title, its idea, and the cards that teach it. The last page is a numbered
/// checklist for the first minutes on the desktop.
view tour_page(s: session.Setup) {
    if s.tour == 1 {
        stack(gap: .group) {
            page_title("Everything is close at hand")
            paragraph(
                "The desktop stays calm until you ask for something. Every gesture here has a keyboard route too.",
            )
            tour_card(
                "Search",
                if s.mac { "Command+K or Option+Space" } else { "Super+Space" },
                "Open it without typing to see your tabs, apps and actions; type to find any app, file, setting, or action.",
            )
            tour_card(
                "Overview",
                "Super+Tab or pinch",
                "See every workspace and window group, then use Up, Down, and Enter to choose one.",
            )
            tour_card(
                "Move",
                "Super+arrow keys or three-finger swipe",
                "Left and right step between tiled windows along the current strip; up maximizes the window and down restores it. Ctrl+Super+Up and Down, or a three-finger swipe up or down, go to the strip above or below, and Super+1 to 9 straight to a strip; Super+Page Up and Page Down, or a four-finger swipe, switch workspaces.",
            )
        }
    } else if s.tour == 2 {
        stack(gap: .group) {
            page_title("A place for each kind of work")
            paragraph(
                "The layout is simple once its four levels have names. Tilo remembers the layout for you.",
            )
            tour_card(
                "Workspace",
                "",
                "A durable place for a project, its people, permissions, apps, and history.",
            )
            tour_card(
                "Strip",
                "",
                "A horizontal run of window groups inside a workspace. Add another strip when work needs a second row.",
            )
            tour_card(
                "Window group",
                "",
                "A tiled frame for tabs that belong together. Move between groups with Super+Left or Super+Right.",
            )
            tour_card(
                "Tab",
                "",
                "An app or document inside a window group. Drag tabs to arrange them without overlapping windows.",
            )
        }
    } else if s.tour == 3 {
        stack(gap: .group) {
            page_title("Sharing stays deliberate")
            paragraph(
                "Collaboration is built into the workspace, not bolted onto individual apps. Nothing becomes shared merely because it is open.",
            )
            tour_card(
                "Invite from Workspace settings",
                "",
                "Choose a role and review exactly which workspace the invitation opens.",
            )
            tour_card(
                "Follow only when you choose",
                "",
                "People keep their own focus and view. Follow is an explicit, reversible action.",
            )
            tour_card(
                "Undo and permissions are system features",
                "",
                "History follows authorship, and apps ask Tilo for capabilities instead of receiving ambient access.",
            )
        }
    } else {
        // The step numbers already group and order the first tasks; a second card around them
        // would push the last one below the footer on a laptop's display.
        stack(gap: .section) {
            page_title("Your first five minutes")
            paragraph(
                "Start with one outcome, then let the workspace grow with the project. Find this again from Search as Desktop tour.",
            )
            stack(gap: .related) {
                first_task(
                    1,
                    "Name the workspace",
                    "Open Workspace settings from the title and choose a recognisable project name.",
                )
                first_task(
                    2,
                    "Open a shared starting point",
                    "Choose Notes, Calendar, or Chat, then add other apps beside it.",
                )
                first_task(
                    3,
                    "Invite the right people",
                    "Choose Invite people in Workspace settings and review each role before sharing.",
                )
                first_task(
                    4,
                    "Check work and recover mistakes",
                    "Open Activity to catch up, compare versions, or undo only the change that went wrong.",
                )
            }
        }
    }
}

/// One idea of the tour under its title, with the keys and gesture that reach it where it has
/// them.
view tour_card(title: Text, keys: Text, body: Text) {
    ui.Section(title) {
        stack(gap: .tight) {
            if keys != "" {
                ui.Text(keys, role: .label)
            }
            paragraph(body)
        }
    }
}

/// One task of the first minutes, under its number.
view first_task(number: Int, title: Text, body: Text) {
    row(gap: .sibling, align: .start) {
        ui.Text("{number}", role: .label, secondary: true, numeric: true)
        stack(gap: .tight, fill: 1.0) {
            ui.Text(title, role: .label)
            paragraph(body)
        }
    }
}

// ---------------------------------------------------------------- the footer
/// The step's ways forward, pinned under the page so they stay in view at every size.
view footer(s: session.Setup) {
    if s.step == "tour" {
        actions {
            ui.Button("Skip tour", key: "Escape") {
                on press: session.skip(s)
            }
            ui.Button("Back") {
                on press: session.back(s)
            }
            if s.tour == 4 {
                ui.Button("Enter Tilo", role: .primary) {
                    on press: session.finish(s)
                }
            } else {
                ui.Button("Continue", role: .primary) {
                    on press: session.continue(s)
                }
            }
        }
    } else if s.step == "intro" {
        // The two ways to begin, one row each; nothing to go back to.
        ui.Section {
            ui.ListRow(
                "Use my existing Tilo identity",
                icon: .user,
                secondary: "Bring it over from another device or a backup",
                description: "Bring it over from another device or a backup",
                disclosure: true,
            ) {
                on activate: session.choose(s, option: "existing")
            }
            ui.ListRow(
                "Create a new identity",
                icon: .plus,
                secondary: "Start fresh on this computer",
                description: "Start fresh on this computer",
                disclosure: true,
            ) {
                on activate: session.choose(s, option: "create")
            }
        }
    } else if s.step == "existing" {
        stack(gap: .related) {
            ui.Section {
                ui.ListRow(
                    "Use a code from another device",
                    icon: .monitor,
                    secondary: "Run tilo device add on a device that already has your identity",
                    description: "Run tilo device add on a device that already has your identity",
                    disclosure: true,
                ) {
                    on activate: session.choose(s, option: "nearby")
                }
                ui.ListRow(
                    "Enter your recovery key",
                    icon: .list_ordered,
                    secondary: "Type the 24 words you wrote down",
                    description: "Type the 24 words you wrote down",
                    disclosure: true,
                ) {
                    on activate: session.choose(s, option: "recovery-key")
                }
                ui.ListRow(
                    "Restore from a backup",
                    icon: .archive,
                    secondary: "Open an encrypted identity backup file",
                    description: "Open an encrypted identity backup file",
                    disclosure: true,
                ) {
                    on activate: session.choose(s, option: "backup-file")
                }
            }
            actions {
                if s.recovery_only {
                    ui.Button("Try again", icon: .refresh) {
                        on press: session.retry(s)
                    }
                } else {
                    back_button(s)
                }
            }
        }
    } else if s.step == "nearby" {
        actions {
            back_button(s)
            if s.nearby_active {
                ui.Button("Cancel") {
                    on press: session.cancel(s)
                }
            } else {
                ui.Button("Join", role: .primary) {
                    on press: session.retry(s)
                }
            }
        }
    } else if s.step == "recovery-key" {
        actions {
            back_button(s)
            ui.Button(
                "Restore identity",
                icon: .arrow_right,
                role: .primary,
                enabled: s.words_check == "ready" and not s.busy,
                description: "Type all 24 words of a recovery key first",
            ) {
                on press: session.continue(s)
            }
        }
    } else if s.step == "restore" {
        actions {
            back_button(s)
            ui.Button(
                "Restore identity",
                icon: .arrow_right,
                role: .primary,
                enabled: s.connect_path.trim() != "",
                description: "Choose a backup file first",
            ) {
                on press: session.continue(s)
            }
        }
    } else if s.step == "protect" {
        // The acknowledgement stays above the actions, never inside the scrolling explanation.
        stack(gap: .sibling) {
            ui.Checkbox(
                "I understand that losing this device without a backup means losing this identity.",
                bind: s.acknowledged,
            ) {
                on check: session.acknowledge(s, checked: not s.acknowledged)
            }
            actions {
                back_button(s)
                ui.Button(
                    "Create local identity",
                    icon: .shield,
                    role: .primary,
                    enabled: s.can_create,
                    description: "Tick the acknowledgement first",
                ) {
                    on press: session.continue(s)
                }
            }
        }
    } else if s.step == "codecs" {
        if not codecs_choice(s) {
            actions {
                ui.Button(
                    "Continue",
                    icon: .arrow_right,
                    role: .primary,
                    size: .large,
                    default_focus: true,
                ) {
                    on press: session.continue(s)
                }
            }
        }
    } else if s.step == "ready" {
        // Entering the desktop is the hand-off and the only way out of setup.
        actions {
            ui.Button("Continue to desktop", icon: .arrow_right, role: .primary) {
                on press: session.finish(s)
            }
        }
    } else if s.step == "recovery-key-copy" {
        actions {
            ui.Button("Back") {
                on press: session.back(s)
            }
            ui.Button(
                "I wrote it down",
                role: .primary,
                enabled: s.key_confirmed,
                description: "Type the three words back first",
            ) {
                on press: session.continue(s)
            }
        }
    } else {
        backup_footer(s)
    }
}

/// The backup step's way on: past a verified backup, or past none once the person acknowledged
/// what that means.
view backup_footer(s: session.Setup) {
    if s.portable_backup_only {
        actions {
            ui.Button("Cancel") {
                on press: session.finish(s)
            }
            ui.Button(
                "Done",
                role: .primary,
                enabled: s.backup_verified,
                description: "Save a verified backup first",
            ) {
                on press: session.continue(s)
            }
        }
    } else if s.backup_verified and s.backup_form_open {
        actions {
            ui.Button("Continue", role: .primary) {
                on press: session.continue(s)
            }
        }
    } else if s.backup_verified {
        actions {
            ui.Button("Save another copy") {
                on press: session.choose(s, option: "another-copy")
            }
            ui.Button("Continue", role: .primary) {
                on press: session.continue(s)
            }
        }
    } else {
        // The backup is offered, never forced: Devices keeps offering it once setup is done.
        stack(gap: .sibling) {
            paragraph(
                "Until you back it up, this identity lives only on this computer. Devices offers the backup whenever you are ready.",
            )
            actions {
                ui.Button("Later", description: "Back up from Devices whenever you like") {
                    on press: session.skip(s)
                }
            }
        }
    }
}
