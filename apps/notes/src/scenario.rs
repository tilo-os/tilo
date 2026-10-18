//! Screenshot scenarios: a sample notebook written with the notebook's own
//! actions, the note each scenario shows, and what a person does in it,
//! driven through the window's event path.

use std::fmt::Write as _;

use ui::headless::{FrameHarness, SemanticExpectation};
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, MouseButton};

use crate::actions::{self, Action};
use crate::main_panel::MainPanel;
use crate::model::{PageId, TagId};

/// Every scenario `--shots --scenario NAME` renders.
pub const SCENARIOS: &[&str] = &[
    "first_note",
    "drawer",
    "drag",
    "block_drag",
    "block_type_menu",
    "menu",
    "sidebar_hover",
    "note_menu",
    "rename",
    "rename_commit",
    "slash",
    "slash_query",
    "slash_applied",
    "heading",
    "fenced_code",
    "fenced_code_opening",
    "code_keyboard",
    "code_keyboard_active",
    "typed_lists",
    "multiline_typing",
    "mixed_outline",
    "rich_outline",
    "paragraph_click",
    "code_group_drag",
    "code_group_drop",
    "adjacent_code_fences",
    "quote",
    "title_edit",
    "recipes",
    "formula",
    "transclusion",
    "external_link",
    "table",
    "table_large",
    "table_large_scrolled",
    "table_cell_edit",
    "image",
    "image_sized",
    "long_list",
    "image_edit",
    "status",
    "save_retrying",
    "save_not_saved",
    "save_error",
    "import_success",
    "presence",
    "shortcuts",
    "shortcuts_bottom",
    "find",
    "scrolled",
    "sidebar_scroll",
    "mention",
];

/// A 16:9-ish desktop window that shows the sidebar and a full page.
pub const VIEWPORT: (u32, u32) = (1280, 720);

/// The paragraphs `multiline_typing` and `paragraph_click` type.
pub const FIRST: &str = "Let's go. Hi this is Quinten speaking. Why does pressing Enter not work?";
pub const SECOND: &str = "This second paragraph should wrap naturally while I keep typing, and every line should remain visible below the first paragraph.";
pub const CONTINUATION: &str = "This continues the same paragraph after Shift+Enter, with enough text to wrap onto another visible line.";

/// The note a person types into in the typing scenarios.
pub const WRITING: &str = "Writing";
/// The note most scenarios show.
pub const LAUNCH: &str = "Launch checklist";

fn page(n: u128) -> PageId {
    uuid::Uuid::from_u128(0x6e6f_7465_0000_4000_8000_0000_0000_0000 | n)
}

fn tag(n: u128) -> TagId {
    uuid::Uuid::from_u128(0x7461_6767_0000_4000_8000_0000_0000_0000 | n)
}

/// Scenarios in which a person types into a new note.
fn types_into_new_note(scenario: Option<&str>) -> bool {
    matches!(
        scenario,
        Some(
            "multiline_typing"
                | "paragraph_click"
                | "typed_lists"
                | "slash"
                | "mention"
                | "slash_query"
                | "slash_applied"
                | "heading"
                | "fenced_code"
                | "fenced_code_opening"
                | "code_keyboard"
                | "code_keyboard_active"
                | "code_group_drag"
                | "code_group_drop"
        )
    )
}

/// The title of the note `scenario` shows; `None` for an empty notebook.
#[must_use]
pub fn shown_note(scenario: Option<&str>) -> Option<&'static str> {
    match scenario {
        Some("first_note") => None,
        Some("recipes") => Some("Recipes"),
        Some("formula") => Some("Formula notebook"),
        Some("import_success") => Some("Project brief"),
        _ if types_into_new_note(scenario) => Some(WRITING),
        _ => Some(LAUNCH),
    }
}

const LAUNCH_BODY: &str = "\
What has to land before we ship. Owner in **bold**.

## This week

- [x] Finalise the onboarding copy

- [x] Cut the release branch

- [ ] Write the migration guide

- [ ] Record the walkthrough video

## Notes

See [[Reading list]] for sources.

Read the [release writing guide](https://example.test/writing) before publishing.

![[Reading list]]

```sh
git checkout -b release/v1.0
git tag -a v1.0 -m \"release\"
git push origin v1.0 --tags
```
";

const INBOX_BODY: &str = "\
Quick captures to sort later.

- Call the dentist

- Reply to the venue about the date

- Look into a standing desk
";

const READING_BODY: &str = "\
Things worth coming back to.

- *The Timeless Way of Building* — Alexander

- `SICP`, chapter 2

- **Notes on the Synthesis of Form**
";

const PROJECTS_BODY: &str = "\
Active work, one note each.

- [[Launch checklist]]
";

const RECIPES_BODY: &str = "\
## Weeknight pasta

- 200g pasta

- garlic, chili, good olive oil

- a handful of parsley

## Method

1. Boil the pasta in well-salted water.

2. Warm the oil, garlic and chili gently.

3. Toss the drained pasta through the oil.

4. Finish with parsley off the heat.
";

const FORMULA_BODY: &str = r#"# Formula notebook

Edit the source; the mathematical preview stays live underneath.

## TeX

```tex
\sum_{i=1}^{n} \frac{x_i^2}{\sqrt{1+x_i}} + \begin{bmatrix}a&b\\c&d\end{bmatrix}
```

## Typst

```typst visual
integral_0^infinity frac(1, 1+x^2) + vec(alpha, beta, gamma)
```

## Typst cases

```typst
f(x) = cases(x^2 & "if" x >= 0, -x & "otherwise")
```
"#;

/// The image the image scenarios show: a checked-in PNG, so the real decoder
/// draws it without network access.
const IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/settings.png"));

/// What the scenario's launch checklist says, when it differs from the
/// sample's.
fn launch_body(scenario: Option<&str>) -> String {
    match scenario {
        Some("rich_outline") => "1. Prepare the release\n\n  Keep the explanation with its list item. Paragraphs can wrap here without losing their place in the outline.\n\n  ```python\n  if ready:\n      publish()\n  ```\n\n  - [x] Check the writing experience\n    1. Review keyboard navigation\n    2. Read the screenshots critically\n\n2. Share the finished note\n".into(),
        Some("quote") => "## A thought worth keeping\n\n> Good notes preserve the reason behind a decision, not just the decision itself.\n\nQuotes remain editable, linkable, draggable blocks—not pasted decoration.\n".into(),
        Some("mixed_outline") => "# A release, step by step\n\n1. Prepare the release\n  - Check the writing experience\n    - [x] Long paragraphs wrap without clipping\n    - [ ] Review keyboard navigation\n      Keep this explanation inside the same checklist item.\n  1. Ask for a second review\n    - Read the screenshots critically\n  2. Address the findings\n2. Share the finished note\n".into(),
        Some("external_link") => "# Links that behave like links\n\nExternal destinations stay editable Markdown, sync with the note, and open through the desktop portal.\n\nRead the [complete writing guide](https://example.test/writing) before publishing.\n\n> Unsafe schemes remain plain text instead of becoming executable actions.\n".into(),
        Some("table" | "table_cell_edit") => "# Tables without leaving the note\n\nTrack owners and progress together.\n\n| Project | Owner | Status |\n| :--- | :--- | ---: |\n| Search | Mina | Shipped |\n| Import | Jules | In review |\n| Mobile polish | Sam | Next |\n".into(),
        Some("table_large" | "table_large_scrolled") => {
            let header = (1..=10).map(|column| format!("Field {column}")).collect::<Vec<_>>().join(" | ");
            let separator = ["---"; 10].join(" | ");
            let mut source = format!("# Every detail belongs in the note\n\n| {header} |\n| {separator} |\n");
            for row in 1..=16 {
                let cells = (1..=10)
                    .map(|column| if row == 1 && column == 1 {
                        "A long cell with important details that must remain readable on a narrow display".to_owned()
                    } else {
                        format!("Row {row}, field {column}")
                    })
                    .collect::<Vec<_>>()
                    .join(" | ");
                let _ = writeln!(source, "| {cells} |");
            }
            source
        }
        Some("image_sized") => format!(
            "# Images stay part of the note\n\nA picture keeps the width it was sized to.\n\n![Settings external accounts|360](asset:{})\n",
            blake3::hash(IMAGE).to_hex()
        ),
        Some("long_list") => {
            let mut body = String::from("## Moving checklist\n\n");
            for (index, step) in [
                "Book the van",
                "Order boxes",
                "Pack the books",
                "Forward the mail",
                "Cancel the internet",
                "Defrost the freezer",
                "Photograph the meters",
                "Clean the oven",
                "Return the keys",
                "Update the address",
                "Unpack the kitchen",
                "Invite the neighbours",
            ]
            .iter()
            .enumerate()
            {
                let _ = writeln!(body, "{}. {step}", index + 1);
                if index == 2 {
                    body.push_str("   The heavy ones go in the small boxes.\n");
                }
            }
            body
        }
        Some("image" | "image_edit") => format!(
            "# Images stay part of the note\n\nWorkspace images load asynchronously without blocking typing or escaping the collaboration sandbox.\n\n![Settings external accounts](asset:{})\n",
            blake3::hash(IMAGE).to_hex()
        ),
        Some("find") => "# Migration plan\n\nThe workspace migration keeps every linked decision close to the work.\n\n- [x] Inventory the current workspace\n- [ ] Review the migration guide\n\n> A careful migration should feel boring, reversible, and easy to verify.\n".into(),
        Some("adjacent_code_fences") => "## Two separate examples\n\nEach Python example keeps its own code block.\n\n```python\nmessage = \"First example\"\nprint(message)\n```\n\n```python\nmessage = \"Second example\"\nprint(message.upper())\n```\n\nContinue writing below either block with Ctrl+Enter.\n".into(),
        Some("scrolled") => {
            let mut body = LAUNCH_BODY.to_owned();
            for index in 1..=20 {
                let _ = write!(
                    body,
                    "\nFollow-up item {index}: keep the document readable while scrolling.\n\n"
                );
            }
            body
        }
        _ => LAUNCH_BODY.into(),
    }
}

/// The notebook `scenario` shows, written with the notebook's own actions:
/// a handful of notes in a tree, two tags on the launch checklist, and the
/// note a typing scenario types into.
#[must_use]
pub fn actions(scenario: Option<&str>) -> Vec<Action> {
    if scenario == Some("first_note") {
        return Vec::new();
    }
    let (inbox, projects, launch, reading, recipes, formulas) =
        (page(1), page(2), page(3), page(4), page(5), page(6));
    let mut notes = vec![
        (inbox, None, "Inbox"),
        (projects, None, "Projects"),
        (launch, Some(projects), LAUNCH),
        (reading, None, "Reading list"),
        (recipes, None, "Recipes"),
        (formulas, None, "Formula notebook"),
    ];
    let references = (1..=28)
        .map(|index| format!("Reference {index:02}"))
        .collect::<Vec<_>>();
    if scenario == Some("sidebar_scroll") {
        for (index, title) in (0u32..).zip(&references) {
            notes.push((page(100 + u128::from(index)), None, title));
        }
    }
    if types_into_new_note(scenario) {
        notes.push((page(7), None, WRITING));
    }
    let mut out = notes
        .iter()
        .map(|(id, parent, title)| actions::create_page_action(*id, *parent, title))
        .collect::<Vec<_>>();
    let titles = notes
        .iter()
        .map(|(id, _, title)| ((*title).to_owned(), *id))
        .collect::<std::collections::HashMap<_, _>>();
    let launch_body = launch_body(scenario);
    for (id, body) in [
        (launch, launch_body.as_str()),
        (inbox, INBOX_BODY),
        (reading, READING_BODY),
        (projects, PROJECTS_BODY),
        (recipes, RECIPES_BODY),
        (formulas, FORMULA_BODY),
    ] {
        let blocks = MainPanel::seed_blocks(body, |title| titles.get(title).copied());
        out.push(actions::write_body_action(id, &blocks));
    }
    if matches!(scenario, Some("image" | "image_edit" | "image_sized")) {
        // The image itself: the body already names it by its digest.
        out.push(actions::keep_image_action(IMAGE.to_vec()));
    }
    if scenario == Some("import_success") {
        // The note a Markdown file became, the moment its import succeeded.
        out.push(actions::import_markdown_action(
            Some(page(8)),
            "Project brief",
            "Imported through Notes' semantic Markdown action.\n\n- [x] Preserve headings\n- [x] Preserve lists\n",
            Vec::new(),
        ));
    }
    // Tags are notebook metadata, not syntax hidden in the note body.
    for (id, name) in [(tag(1), "Release"), (tag(2), "Writing")] {
        out.push(actions::new_tag_action(id, name));
        out.push(actions::set_tag_assignment_action(launch, id, true));
    }
    out
}

/// What a person does in `scenario`, through the window's event path.
///
/// # Errors
/// Returns what the scenario could not find on screen.
#[allow(
    clippy::too_many_lines,
    reason = "one table of every scenario's input keeps them comparable"
)]
pub fn input(scenario: Option<&str>, driver: &mut FrameHarness) -> Result<(), String> {
    match scenario {
        Some("drawer") => {
            let rect = driver
                .targets()
                .get("nav-shell-sidebar")
                .copied()
                .ok_or("narrow drawer toggle missing")?;
            driver.click_at(rect.center().x, rect.center().y);
            if !driver.control_failures().is_empty() {
                return Err(format!(
                    "drawer control was refused: {:?}",
                    driver.control_failures()
                ));
            }
            let _ = let_drawer_open(driver);
        }
        Some("drag") => {
            let from = semantic_name_rect(driver, LAUNCH).ok_or("sidebar drag source missing")?;
            let to =
                semantic_name_rect(driver, "Projects").ok_or("sidebar drag destination missing")?;
            press(driver, from.center(), MouseButton::Left);
            move_pointer(driver, Point::new(from.center().x, to.y0 + 2.0));
        }
        Some("block_drag") => {
            let from = semantic_target_rect(driver, |target| {
                target.starts_with("notes.block-move:") && target.ends_with(":down")
            })
            .ok_or("block drag handle missing")?;
            let blocks = visible_block_rects(driver);
            let destination = blocks.get(2).ok_or("block drag destination missing")?;
            press(driver, from.center(), MouseButton::Left);
            move_pointer(driver, Point::new(from.center().x, destination.center().y));
        }
        Some("block_type_menu") => {
            driver.relayout();
            let target = semantic_target(driver, |name| name.starts_with("notes.block-type:"))
                .ok_or("block type target missing")?;
            driver.perform_semantic_named(
                "block_editor",
                Some(&target),
                &ui::a11y::Action::Activate,
            )?;
        }
        Some("menu") => {
            reveal_sidebar(driver);
            let rect = semantic_name_rect(driver, LAUNCH).ok_or("note row missing")?;
            press(driver, rect.center(), MouseButton::Right);
        }
        Some("sidebar_hover") => {
            let rect = semantic_target_rect(driver, |target| target.starts_with("row-action:1:"))
                .ok_or("sidebar hover action missing")?;
            move_pointer(driver, rect.center());
        }
        Some("note_menu") => {
            let rect = semantic_target_rect(driver, |target| target == "notes.page-title")
                .ok_or("note title target missing")?;
            press(driver, rect.center(), MouseButton::Right);
        }
        Some("rename" | "rename_commit") => {
            reveal_sidebar(driver);
            let rect = semantic_name_rect(driver, LAUNCH).ok_or("rename row missing")?;
            driver.double_click_at(rect.center().x, rect.center().y);
            if scenario == Some("rename_commit") {
                driver.type_text(" v2");
                driver.click_at(900.0, 400.0);
            }
        }
        Some("shortcuts" | "shortcuts_bottom") => {
            reveal_sidebar(driver);
            let rect = semantic_name_rect(driver, LAUNCH).ok_or("sidebar focus target missing")?;
            driver.click_at(rect.center().x, rect.center().y);
            chord(driver, &["ctrl"], "/");
            if scenario == Some("shortcuts_bottom") {
                driver.key("End");
            }
        }
        Some("find") => {
            chord(driver, &["ctrl"], "f");
            driver.type_text("migration");
        }
        Some("multiline_typing" | "paragraph_click") => {
            let commit = scenario == Some("paragraph_click");
            let enter_text = |driver: &mut FrameHarness, text: &str| {
                if commit {
                    // This scene shows caret placement after pointer input;
                    // a real IME commit builds the same note without
                    // rendering hundreds of unrelated key frames first.
                    driver.commit_text(text);
                } else {
                    driver.type_text(text);
                }
            };
            enter_text(driver, FIRST);
            driver.key("Enter");
            enter_text(driver, SECOND);
            chord(driver, &["shift"], "Enter");
            enter_text(driver, CONTINUATION);
            if commit {
                driver.relayout();
                let rects = visible_block_rects(driver);
                let [first, second] = rects.as_slice() else {
                    return Err(format!(
                        "paragraph click expected two blocks, found {}",
                        rects.len()
                    ));
                };
                driver.click_at(first.x0 + first.width() * 0.18, first.center().y);
                driver.click_at(second.x0 + second.width() * 0.28, second.center().y);
                driver.click_at(first.x0 + first.width() * 0.72, first.center().y);
            }
        }
        Some("typed_lists") => {
            for (index, line) in [
                "- Gather ideas",
                "Write a first draft",
                "",
                "",
                "1. Review the draft",
                "Share with the team",
                "",
                "",
                "- [ ] Follow up tomorrow",
            ]
            .iter()
            .enumerate()
            {
                if index > 0 {
                    driver.key("Enter");
                }
                driver.type_text(line);
            }
        }
        Some("slash") => driver.type_text("/"),
        Some("mention") => {
            show_room(driver);
            driver.type_text("Ask @");
        }
        Some("slash_query") => driver.type_text("Groceries for Monday /c"),
        Some("slash_applied") => {
            driver.type_text("/h1");
            driver.key("Enter");
            driver.type_text("Overview");
        }
        Some("heading") => {
            driver.type_text("# Roadmap");
            driver.key("Enter");
            driver.type_text("Ship by Q3");
        }
        Some("title_edit") => {
            driver.click_named("note_title")?;
            driver.type_text("Launch checklist notes");
            driver.expect(
                &SemanticExpectation::named("note_title").value("Launch checklist notes"),
            )?;
        }
        Some("table_cell_edit") => {
            driver.relayout();
            let target = semantic_target(driver, |name| {
                name.starts_with("notes.table-cell:") && name.ends_with(":1:1")
            })
            .ok_or("editable table cell target missing")?;
            driver.perform_semantic_named(
                "block_editor",
                Some(&target),
                &ui::a11y::Action::Activate,
            )?;
            driver.key("End");
            driver.type_text(" Chen");
        }
        Some("image" | "image_edit" | "image_sized") => {
            let deadline = ui::time::now() + std::time::Duration::from_secs(2);
            let target = loop {
                driver.relayout();
                if let Some(target) =
                    semantic_target(driver, |name| name.starts_with("notes.image-edit:"))
                {
                    break target;
                }
                if ui::time::now() >= deadline {
                    return Err("image preview did not become ready within two seconds".into());
                }
                let _ = ui::rt::sleep_thread(std::time::Duration::from_millis(5));
            };
            if scenario == Some("image_edit") {
                driver.perform_semantic_named(
                    "block_editor",
                    Some(&target),
                    &ui::a11y::Action::Activate,
                )?;
            }
        }
        Some("table_large_scrolled" | "scrolled") => {
            driver.relayout();
            let target = visible_block_rects(driver)
                .into_iter()
                .next()
                .ok_or("scroll capture has no visible block")?;
            driver.click_at(target.center().x, target.center().y);
            driver.scroll(0.0, -1000.0);
        }
        Some("fenced_code" | "fenced_code_opening") => {
            driver.type_text("```python");
            if scenario == Some("fenced_code") {
                for line in [
                    "message = \"Hello from a Python code block\"",
                    "print(message)",
                    "for value in range(3):",
                    "    print(value)",
                    "```",
                    "A note below the code.",
                ] {
                    driver.key("Enter");
                    driver.type_text(line);
                }
            }
        }
        Some("code_keyboard" | "code_keyboard_active" | "code_group_drag" | "code_group_drop") => {
            driver.type_text("```python");
            driver.key("Enter");
            driver.type_text("if ready:");
            driver.key("Enter");
            driver.key("Tab");
            driver.type_text("print('Ready')");
            driver.key("Enter");
            driver.type_text("print('Still code')");
            if scenario != Some("code_keyboard_active") {
                chord(driver, &["ctrl"], "Enter");
                driver.type_text("Back to writing, without touching the mouse.");
            }
            if matches!(scenario, Some("code_group_drag" | "code_group_drop")) {
                driver.relayout();
                let from = semantic_target_rect(driver, |target| {
                    target.starts_with("notes.block-move:") && target.ends_with(":down")
                })
                .ok_or("code group drag handle missing")?;
                let blocks = visible_block_rects(driver);
                let destination = blocks.get(3).ok_or("code group drag destination missing")?;
                let over = Point::new(from.center().x, destination.center().y);
                press(driver, from.center(), MouseButton::Left);
                move_pointer(driver, over);
                if scenario == Some("code_group_drop") {
                    pointer(driver, over, ElementState::Released, MouseButton::Left);
                }
            }
        }
        Some("sidebar_scroll") => {
            reveal_sidebar(driver);
            let sidebar = semantic_name_rect(driver, "Reference 01")
                .ok_or("scrollable notes tree missing")?;
            move_pointer(driver, sidebar.center());
            for _ in 0..10 {
                driver.scroll(0.0, -3.0);
            }
        }
        Some("presence") => show_collaborators(driver)?,
        Some("save_error") => {
            let first = visible_block_rects(driver)
                .into_iter()
                .next()
                .ok_or("status edit target missing")?;
            // A space after the line's last word: an edit the note saves
            // that reads as nothing in the capture.
            driver.click_at(first.x0 + first.width() * 0.8, first.center().y);
            driver.key("End");
            driver.type_text(" ");
        }
        _ => {}
    }
    for _ in 0..3 {
        driver.relayout();
    }
    Ok(())
}

/// Alice typing in the fifth block with her pointer over the note, and Bob
/// reading another note.
fn show_collaborators(driver: &mut FrameHarness) -> Result<(), String> {
    use ui::permissions::UserId;
    use ui::presence::{PresenceCtx, PresenceKey, SelectionRange};
    let mut presence = PresenceCtx::empty();
    let alice = UserId("alice".into());
    presence.set_display_name(alice.clone(), "Alice");
    presence.set_display_name(UserId("bob".into()), "Bob");
    let fifth = visible_block_ids(driver)
        .into_iter()
        .nth(4)
        .ok_or("the presence scene needs five blocks")?;
    let block = ui::widget::block_editor::block_presence_key(&fifth);
    presence.set_caret(&block, alice.clone(), 18);
    presence.record_peer_typing(
        alice,
        block,
        SelectionRange { start: 0, end: 18 },
        ui::time::now(),
    );
    presence.upsert_remote_cursor("alice".into(), [560.0, 300.0], Some("Alice"));
    presence.register_peer_at(
        "bob".into(),
        PresenceKey::Tab {
            id: actions::page_resource_id(page(4)),
        },
    );
    presence.upsert_remote_cursor("bob".into(), [1500.0, 380.0], Some("Bob"));
    driver.set_presence_context(presence);
    Ok(())
}

/// The room around the note: two people and an agent the `@` list offers.
fn show_room(driver: &mut FrameHarness) {
    use ui::permissions::UserId;
    use ui::presence::{ParticipantKind, PresenceCtx};
    let mut presence = PresenceCtx::empty();
    for (id, name, kind) in [
        ("alice", "Alice Moreau", ParticipantKind::Person),
        ("aaron", "Aaron Kim", ParticipantKind::Person),
        ("agent:scribe", "Scribe", ParticipantKind::Agent),
    ] {
        presence.set_display_name(UserId(id.into()), name);
        presence.set_participant_kind(UserId(id.into()), kind);
    }
    driver.set_presence_context(presence);
    driver.relayout();
}

/// Open the narrow drawer when the notes tree is hidden behind it.
fn reveal_sidebar(driver: &mut FrameHarness) {
    if semantic_name_rect(driver, LAUNCH).is_none()
        && let Some(toggle) = driver.targets().get("nav-shell-sidebar").copied()
    {
        driver.click_at(toggle.center().x, toggle.center().y);
        driver.relayout();
        let _ = let_drawer_open(driver);
    }
}

/// Run frames until the drawer has slid fully in. A capture waits for
/// visual work, not for motion, so a shot taken straight after the toggle
/// caught the drawer part-way in, its left edge and title cut off. Returns
/// whether every animation came to rest within the bound; a blinking caret
/// may keep asking for frames, and the bound outlasts the slide either way.
fn let_drawer_open(driver: &mut FrameHarness) -> bool {
    driver.wait_until(DRAWER_SETTLE, |driver| {
        !driver.frame_schedule().needs_animation_frame
    })
}

/// Longer than the drawer's slide at any motion setting.
const DRAWER_SETTLE: ui::time::Duration = ui::time::Duration::from_millis(600);

fn chord(driver: &mut FrameHarness, modifiers: &[&str], key: &str) {
    driver.set_modifiers(
        &modifiers
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>(),
    );
    driver.key(key);
    driver.set_modifiers(&[]);
}

fn pointer(driver: &mut FrameHarness, at: Point, state: ElementState, button: MouseButton) {
    move_pointer(driver, at);
    driver.send(&ui::synthetic_input::mouse_input(state, button));
}

fn press(driver: &mut FrameHarness, at: Point, button: MouseButton) {
    pointer(driver, at, ElementState::Pressed, button);
}

fn move_pointer(driver: &mut FrameHarness, at: Point) {
    driver.send(&ui::synthetic_input::cursor_moved(at.x, at.y));
}

/// Every accessibility node of the window, depth first.
fn nodes(driver: &FrameHarness) -> Vec<ui::a11y::AccessNode> {
    fn access(node: &ui::a11y::AccessNode, out: &mut Vec<ui::a11y::AccessNode>) {
        out.push(node.clone());
        for child in &node.children {
            access(child, out);
        }
    }
    fn retained(node: &ui::headless::InspectionNode, out: &mut Vec<ui::a11y::AccessNode>) {
        if let Some(semantics) = &node.semantics {
            access(semantics, out);
        }
        for child in &node.children {
            retained(child, out);
        }
    }
    let mut out = Vec::new();
    retained(&driver.inspection().root, &mut out);
    out
}

/// The rendered note blocks, top to bottom, with their stable ids.
fn visible_blocks(driver: &FrameHarness) -> Vec<(Rect, String)> {
    let mut rows = nodes(driver)
        .into_iter()
        .filter_map(|node| {
            let id = node
                .semantic_key
                .as_deref()?
                .strip_prefix("notes.block:")
                .filter(|suffix| !suffix.contains(':'))?
                .to_owned();
            Some((node.bounds?.to_rect(), id))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| a.0.y0.total_cmp(&b.0.y0));
    rows.dedup_by(|a, b| a.1 == b.1);
    rows
}

fn visible_block_ids(driver: &FrameHarness) -> Vec<String> {
    visible_blocks(driver)
        .into_iter()
        .map(|(_, id)| id)
        .collect()
}

fn visible_block_rects(driver: &FrameHarness) -> Vec<Rect> {
    visible_blocks(driver)
        .into_iter()
        .map(|(rect, _)| rect)
        .collect()
}

fn semantic_target(driver: &FrameHarness, predicate: impl Fn(&str) -> bool) -> Option<String> {
    nodes(driver)
        .into_iter()
        .find_map(|node| node.action_target.filter(|target| predicate(target)))
}

fn semantic_target_rect(driver: &FrameHarness, predicate: impl Fn(&str) -> bool) -> Option<Rect> {
    nodes(driver).into_iter().find_map(|node| {
        node.action_target
            .as_deref()
            .filter(|target| predicate(target))
            .and(node.bounds)
            .map(ui::a11y::AccessBounds::to_rect)
    })
}

fn semantic_name_rect(driver: &FrameHarness, name: &str) -> Option<Rect> {
    nodes(driver).into_iter().find_map(|node| {
        (node.name.as_deref() == Some(name))
            .then_some(node.bounds)
            .flatten()
            .map(ui::a11y::AccessBounds::to_rect)
    })
}
