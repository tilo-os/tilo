//! Hermetic captures mounted through the same Notes definition and canonical
//! owner as the live component. Scenarios use real semantic actions and keys.
use std::{cell::RefCell, fmt::Write as _, path::Path, rc::Rc};
use ui::application::{ApplicationHost, ComponentRunnerInputs, PreparedComponentInputs};
use ui::headless::{FrameHarness, SemanticExpectation};
use ui::launch::{LaunchError, LaunchRecovery};
use ui::permissions::{Actor, PermissionContext, Role, UserId};

thread_local! {
    static SAVE_FAULT_RELEASE: RefCell<Option<std::sync::mpsc::Sender<()>>> = const { RefCell::new(None) };
}

struct CaptureLifetime {
    directory: tempfile::TempDir,
    failed_save_release: RefCell<Option<std::thread::JoinHandle<()>>>,
    failed_save_signal: RefCell<Option<std::sync::mpsc::Sender<()>>>,
}

impl CaptureLifetime {
    fn path(&self) -> &Path {
        self.directory.path()
    }
}

impl Drop for CaptureLifetime {
    fn drop(&mut self) {
        if let Some(signal) = self.failed_save_signal.get_mut().take() {
            let _ = signal.send(());
        }
        if let Some(release) = self.failed_save_release.get_mut().take() {
            let _ = release.join();
        }
    }
}

const INSTANCE: u64 = 1;
const FIRST: &str = "Let's go. Hi this is ada speaking. Why does pressing Enter not work?";
const SECOND: &str = "This second paragraph should wrap naturally while I keep typing, and every line should remain visible below the first paragraph.";
const CONTINUATION: &str = "This continues the same paragraph after Shift+Enter, with enough text to wrap onto another visible line.";
const OUTLINE: &str = "1. Prepare the release\n\n  Keep the explanation with its list item. Paragraphs can wrap here without losing their place in the outline.\n\n  ```python\n  if ready:\n      publish()\n  ```\n\n  - [x] Check the writing experience\n    1. Review keyboard navigation\n    2. Read the screenshots critically\n\n2. Share the finished note\n";

fn fixture_body(scenario: Option<&str>) -> Option<String> {
    match scenario {
        Some("rich_outline") => Some(OUTLINE.into()),
        Some("quote") => Some("## A thought worth keeping\n\n> Good notes preserve the reason behind a decision, not just the decision itself.\n\nQuotes remain editable, linkable, draggable blocks—not pasted decoration.\n".into()),
        Some("mixed_outline") => Some("# A release, step by step\n\n1. Prepare the release\n  - Check the writing experience\n    - [x] Long paragraphs wrap without clipping\n    - [ ] Review keyboard navigation\n      Keep this explanation inside the same checklist item.\n  1. Ask for a second review\n    - Read the screenshots critically\n  2. Address the findings\n2. Share the finished note\n".into()),
        Some("external_link") => Some("# Links that behave like links\n\nExternal destinations stay editable Markdown, sync with the note, and open through the desktop portal.\n\nRead the [complete writing guide](https://example.test/writing) before publishing.\n\n> Unsafe schemes remain plain text instead of becoming executable actions.\n".into()),
        Some("table" | "table_cell_edit") => Some("# Tables without leaving the note\n\nTrack owners and progress together.\n\n| Project | Owner | Status |\n| :--- | :--- | ---: |\n| Search | Mina | Shipped |\n| Import | Jules | In review |\n| Mobile polish | Sam | Next |\n".into()),
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
            Some(source)
        }
        Some("image" | "image_edit") => Some("# Images stay part of the note\n\nWorkspace-relative images load asynchronously without blocking typing or escaping the collaboration sandbox.\n\n![Settings external accounts](image.png)\n".into()),
        Some("find") => Some("# Migration plan\n\nThe workspace migration keeps every linked decision close to the work.\n\n- [x] Inventory the current workspace\n- [ ] Review the migration guide\n\n> A careful migration should feel boring, reversible, and easy to verify.\n".into()),
        _ => None,
    }
}

pub fn supports(scenario: Option<&str>) -> bool {
    matches!(
        scenario,
        None | Some(
            "first_note"
                | "drawer"
                | "drag"
                | "block_drag"
                | "block_type_menu"
                | "menu"
                | "sidebar_hover"
                | "note_menu"
                | "rename"
                | "rename_commit"
                | "rich_outline"
                | "adjacent_code_fences"
                | "fenced_code"
                | "fenced_code_opening"
                | "code_keyboard"
                | "code_keyboard_active"
                | "code_group_drag"
                | "code_group_drop"
                | "multiline_typing"
                | "paragraph_click"
                | "typed_lists"
                | "mixed_outline"
                | "slash"
                | "slash_applied"
                | "heading"
                | "quote"
                | "title_edit"
                | "recipes"
                | "formula"
                | "transclusion"
                | "external_link"
                | "table"
                | "table_large"
                | "table_large_scrolled"
                | "table_cell_edit"
                | "image"
                | "image_edit"
                | "scrolled"
                | "shortcuts"
                | "shortcuts_bottom"
                | "find"
                | "sidebar_scroll"
                | "presence"
                | "status"
                | "saving"
                | "save_error"
                | "import_success"
        )
    )
}

/// Construct isolated, host-owned fixture storage. The directory remains alive
/// through the component, its writers, and any failed save recovery owners.
/// # Errors
/// Rejects unknown scenarios and propagates migration/construction failures.
pub fn prepare_fixture(
    scenario: Option<&str>,
    inputs: ComponentRunnerInputs,
) -> Result<PreparedComponentInputs, LaunchError> {
    prepare_fixture_owned(scenario, inputs).map(|(prepared, _)| prepared)
}

#[allow(
    clippy::too_many_lines,
    reason = "constructs one hermetic fixture while retaining all lifetime-owned host resources"
)]
fn prepare_fixture_owned(
    scenario: Option<&str>,
    inputs: ComponentRunnerInputs,
) -> Result<(PreparedComponentInputs, ui::record::RecordStore), LaunchError> {
    if !supports(scenario) {
        return Err(LaunchError::Scenario(format!(
            "canonical capture scenario not implemented: {scenario:?}"
        )));
    }
    let directory = Rc::new(CaptureLifetime {
        directory: tempfile::Builder::new()
            .prefix("notes-capture-")
            .tempdir()
            .map_err(|error| LaunchError::Operation(error.to_string()))?,
        failed_save_release: RefCell::new(None),
        failed_save_signal: RefCell::new(None),
    });
    let source = directory.path().join("source");
    if scenario != Some("first_note") {
        crate::shots::seed_sample_workspace(&source, scenario)
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
        if scenario == Some("sidebar_scroll")
            && let Some(workspace) =
                crate::storage::load_workspace(&source).map_err(LaunchError::Operation)?
        {
            for page in workspace
                .pages()
                .iter()
                .filter(|page| page.title.starts_with("Reference "))
            {
                crate::storage::write_page_body(&source, page.id, "")
                    .map_err(|error| LaunchError::Operation(error.to_string()))?;
            }
        }
        if let Some(body) = fixture_body(scenario) {
            let workspace =
                crate::storage::load_workspace(&source).map_err(LaunchError::Operation)?;
            let selected = workspace
                .and_then(|workspace| workspace.selected())
                .ok_or_else(|| LaunchError::Operation("fixture selection missing".into()))?;
            crate::storage::write_page_body(&source, selected, &body)
                .map_err(|error| LaunchError::Operation(error.to_string()))?;
        }
    }
    let profile_root = directory.path().join("state");
    let mut host = ApplicationHost::new_with_profile_lifetime(
        profile_root.clone(),
        "capture-device".into(),
        "capture-workspace",
        UserId("capture-author".into()),
        Rc::new(RefCell::new(PermissionContext::from_capabilities(
            Actor::person(Some(Role::Owner), "capture-author"),
            vec![],
        ))),
        tilo_actions::ActionContext::default(),
        directory.clone(),
    )?;
    let records = host.records_for(crate::definition())?;
    let prepared = crate::hosting::prepare_default(&mut host, INSTANCE, Some(&source), inputs)?;
    if matches!(scenario, Some("image" | "image_edit")) {
        let bytes = crate::files::read(&source.join(crate::storage::PAGES_DIR).join("image.png"))
            .map_err(|error| LaunchError::Operation(error.to_string()))?;
        records
            .with(&crate::hosting::default_notebook().id, |doc| {
                let workspace = crate::notebook::project(doc, None)?;
                let page = workspace
                    .pages()
                    .iter()
                    .find(|page| page.title == "Launch checklist")
                    .map(|page| page.id)
                    .ok_or("image fixture selection missing")?;
                let (block_id, text) = crate::doc::NotesDoc::page_block_snapshots(doc, page)
                    .into_iter()
                    .find(|(_, text)| text == "![Settings external accounts](image.png)")
                    .ok_or("image fixture block missing")?;
                let digest = blake3::hash(&bytes).to_hex().to_string();
                let replacement = format!("![Settings external accounts](asset:{digest})");
                let action = crate::actions::add_image_asset_action(
                    page,
                    bytes,
                    tilo_document::RichTextAction::SpliceText {
                        region: tilo_document::action::RegionKind::Body,
                        block: block_id,
                        at: 0,
                        del: text.chars().count(),
                        ins: replacement,
                    },
                );
                tilo_actions::apply_local(doc, &action, crate::actions::encode(&action))
                    .map_err(|error| error.to_string())?;
                Ok::<_, String>(())
            })
            .ok_or_else(|| LaunchError::Operation("image fixture record missing".into()))?
            .map_err(LaunchError::Operation)?;
        records
            .try_flush()
            .map_err(|error| LaunchError::Operation(format!("flush image fixture: {error}")))?;
    }
    if scenario == Some("save_error") {
        records
            .try_flush()
            .map_err(|error| LaunchError::Operation(format!("flush capture store: {error}")))?;
        let record_dir = app_state::DocStore::room_record_dir_at(
            &profile_root,
            crate::definition().app_id,
            "capture-workspace",
            &crate::hosting::default_notebook().id.storage_key(),
        )
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
        let document = record_dir.join(app_state::DOCUMENT_FILE);
        let (signal, wait) = std::sync::mpsc::channel();
        let release = tilo_files::native::atomic::block_replacement(
            &document,
            wait,
            std::time::Duration::from_secs(30),
        )
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
        SAVE_FAULT_RELEASE.with_borrow_mut(|slot| *slot = Some(signal.clone()));
        *directory.failed_save_signal.borrow_mut() = Some(signal);
        *directory.failed_save_release.borrow_mut() = Some(release);
    }
    Ok((prepared, records))
}

fn new_note(driver: &mut FrameHarness) -> Result<(), LaunchError> {
    fn created(resources: &[tilo_actions::Resource]) -> Option<String> {
        resources.iter().find_map(|resource| {
            if resource.kind == "notes.page" && resource.label == "Writing" {
                Some(resource.id.as_str().to_owned())
            } else {
                created(&resource.children)
            }
        })
    }

    let arguments = serde_json::json!({"value":"Writing"})
        .as_object()
        .cloned()
        .ok_or_else(|| LaunchError::Operation("invalid fixture action".into()))?;
    driver
        .invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: INSTANCE,
                address: tilo_actions::ActionAddress::new("notes.notebook", "new-page"),
            },
            &arguments,
        )
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    driver.relayout();
    // Creating a resource is a document action; opening it is a separate local
    // view operation. Other windows keep their current selection.
    let resource = created(&driver.resources()).ok_or_else(|| {
        LaunchError::Operation("created capture note missing from resources".into())
    })?;
    driver
        .invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: INSTANCE,
                address: tilo_actions::ActionAddress::new(resource, "open"),
            },
            &serde_json::Map::new(),
        )
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    driver.relayout();
    driver
        .expect(&SemanticExpectation::named("note_title").value("Writing"))
        .map_err(LaunchError::Operation)
}

fn open_note(driver: &mut FrameHarness, label: &str) -> Result<(), LaunchError> {
    fn find(resources: &[tilo_actions::Resource], label: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            (resource.kind == "notes.page" && resource.label == label)
                .then(|| resource.id.as_str().to_owned())
                .or_else(|| find(&resource.children, label))
        })
    }
    let resource = find(&driver.resources(), label)
        .ok_or_else(|| LaunchError::Operation(format!("capture note {label:?} missing")))?;
    driver
        .invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id: INSTANCE,
                address: tilo_actions::ActionAddress::new(resource, "open"),
            },
            &serde_json::Map::new(),
        )
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    driver.relayout();
    driver
        .expect(&SemanticExpectation::named("note_title").value(label))
        .map_err(LaunchError::Operation)
}

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

/// Stable block ids of the rendered blocks, top to bottom (presence keys
/// name blocks by id).
fn visible_block_ids(driver: &FrameHarness) -> Vec<String> {
    fn access(node: &ui::a11y::AccessNode, out: &mut Vec<(f64, String)>) {
        if let Some(id) = node
            .semantic_key
            .as_deref()
            .and_then(|key| key.strip_prefix("notes.block:"))
            .filter(|suffix| !suffix.contains(':'))
            && let Some(bounds) = node.bounds
        {
            out.push((bounds.to_rect().y0, id.to_owned()));
        }
        for child in &node.children {
            access(child, out);
        }
    }
    fn retained(node: &ui::headless::InspectionNode, out: &mut Vec<(f64, String)>) {
        if let Some(semantics) = &node.semantics {
            access(semantics, out);
        }
        for child in &node.children {
            retained(child, out);
        }
    }
    let mut rows = Vec::new();
    retained(&driver.inspection().root, &mut rows);
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows.dedup_by(|a, b| a.1 == b.1);
    rows.into_iter().map(|(_, id)| id).collect()
}

fn visible_block_rects(driver: &FrameHarness) -> Vec<vello::kurbo::Rect> {
    fn access(node: &ui::a11y::AccessNode, rects: &mut Vec<vello::kurbo::Rect>) {
        if node.semantic_key.as_deref().is_some_and(|key| {
            key.strip_prefix("notes.block:")
                .is_some_and(|suffix| !suffix.contains(':'))
        }) && let Some(bounds) = node.bounds
        {
            rects.push(bounds.to_rect());
        }
        for child in &node.children {
            access(child, rects);
        }
    }
    fn retained(node: &ui::headless::InspectionNode, rects: &mut Vec<vello::kurbo::Rect>) {
        if let Some(semantics) = &node.semantics {
            access(semantics, rects);
        }
        for child in &node.children {
            retained(child, rects);
        }
    }
    let mut rects = Vec::new();
    retained(&driver.inspection().root, &mut rects);
    rects.sort_by(|a, b| a.y0.total_cmp(&b.y0));
    rects.dedup();
    rects
}

fn semantic_target(driver: &FrameHarness, predicate: impl Fn(&str) -> bool) -> Option<String> {
    fn access(node: &ui::a11y::AccessNode, predicate: &impl Fn(&str) -> bool) -> Option<String> {
        node.action_target
            .as_deref()
            .filter(|target| predicate(target))
            .map(str::to_owned)
            .or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| access(child, predicate))
            })
    }
    fn retained(
        node: &ui::headless::InspectionNode,
        predicate: &impl Fn(&str) -> bool,
    ) -> Option<String> {
        node.semantics
            .as_ref()
            .and_then(|node| access(node, predicate))
            .or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| retained(child, predicate))
            })
    }
    retained(&driver.inspection().root, &predicate)
}

fn semantic_target_rect(
    driver: &FrameHarness,
    predicate: impl Fn(&str) -> bool,
) -> Option<vello::kurbo::Rect> {
    fn access(
        node: &ui::a11y::AccessNode,
        predicate: &impl Fn(&str) -> bool,
    ) -> Option<vello::kurbo::Rect> {
        node.action_target
            .as_deref()
            .filter(|target| predicate(target))
            .and(node.bounds)
            .map(ui::a11y::AccessBounds::to_rect)
            .or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| access(child, predicate))
            })
    }
    fn retained(
        node: &ui::headless::InspectionNode,
        predicate: &impl Fn(&str) -> bool,
    ) -> Option<vello::kurbo::Rect> {
        node.semantics
            .as_ref()
            .and_then(|node| access(node, predicate))
            .or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| retained(child, predicate))
            })
    }
    retained(&driver.inspection().root, &predicate)
}

fn semantic_name_rect(driver: &FrameHarness, name: &str) -> Option<vello::kurbo::Rect> {
    fn access(node: &ui::a11y::AccessNode, name: &str) -> Option<vello::kurbo::Rect> {
        (node.name.as_deref() == Some(name))
            .then_some(node.bounds)
            .flatten()
            .map(ui::a11y::AccessBounds::to_rect)
            .or_else(|| node.children.iter().find_map(|child| access(child, name)))
    }
    fn retained(node: &ui::headless::InspectionNode, name: &str) -> Option<vello::kurbo::Rect> {
        node.semantics
            .as_ref()
            .and_then(|node| access(node, name))
            .or_else(|| node.children.iter().find_map(|child| retained(child, name)))
    }
    retained(&driver.inspection().root, name)
}

fn pointer_event(
    driver: &mut FrameHarness,
    point: vello::kurbo::Point,
    state: winit::event::ElementState,
    button: winit::event::MouseButton,
) {
    driver.send(&winit::event::WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(point.x, point.y),
    });
    driver.send(&winit::event::WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state,
        button,
    });
}

fn move_pointer(driver: &mut FrameHarness, point: vello::kurbo::Point) {
    driver.send(&winit::event::WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(point.x, point.y),
    });
}

/// Drive the actual mounted component. No fixture-only widget setters.
/// # Errors
/// Refuses an unsupported or unreachable scenario.
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive capture scenarios intentionally share one canonical mounted-driver dispatch"
)]
pub fn drive_fixture(driver: &mut FrameHarness, scenario: Option<&str>) -> Result<(), LaunchError> {
    match scenario {
        None
        | Some(
            "first_note"
            | "rich_outline"
            | "adjacent_code_fences"
            | "quote"
            | "mixed_outline"
            | "transclusion"
            | "external_link"
            | "table"
            | "table_large"
            | "status",
        ) => {}
        Some("drawer") => {
            let rect = semantic_target_rect(driver, |target| target == "notes.drawer")
                .ok_or_else(|| LaunchError::Operation("narrow drawer toggle missing".into()))?;
            driver.click_at(rect.center().x, rect.center().y);
        }
        Some("drag") => {
            let from = semantic_name_rect(driver, "Launch checklist")
                .ok_or_else(|| LaunchError::Operation("sidebar drag source missing".into()))?;
            let to = semantic_name_rect(driver, "Projects")
                .ok_or_else(|| LaunchError::Operation("sidebar drag destination missing".into()))?;
            pointer_event(
                driver,
                from.center(),
                winit::event::ElementState::Pressed,
                winit::event::MouseButton::Left,
            );
            move_pointer(
                driver,
                vello::kurbo::Point::new(from.center().x, to.y0 + 2.0),
            );
        }
        Some("block_drag") => {
            let from = semantic_target_rect(driver, |target| {
                target.starts_with("notes.block-move:") && target.ends_with(":down")
            })
            .ok_or_else(|| LaunchError::Operation("block drag handle missing".into()))?;
            let blocks = visible_block_rects(driver);
            let destination = blocks
                .get(2)
                .ok_or_else(|| LaunchError::Operation("block drag destination missing".into()))?;
            pointer_event(
                driver,
                from.center(),
                winit::event::ElementState::Pressed,
                winit::event::MouseButton::Left,
            );
            move_pointer(
                driver,
                vello::kurbo::Point::new(from.center().x, destination.center().y),
            );
        }
        Some("block_type_menu") => {
            driver.relayout();
            let target = semantic_target(driver, |name| name.starts_with("notes.block-type:"))
                .ok_or_else(|| LaunchError::Operation("block type target missing".into()))?;
            driver
                .perform_semantic_named("block_editor", Some(&target), &ui::a11y::Action::Activate)
                .map_err(LaunchError::Operation)?;
        }
        Some("menu") => {
            if semantic_name_rect(driver, "Launch checklist").is_none()
                && let Some(toggle) =
                    semantic_target_rect(driver, |target| target == "notes.drawer")
            {
                driver.click_at(toggle.center().x, toggle.center().y);
                driver.relayout();
            }
            let rect = semantic_name_rect(driver, "Launch checklist")
                .ok_or_else(|| LaunchError::Operation("note row missing".into()))?;
            pointer_event(
                driver,
                rect.center(),
                winit::event::ElementState::Pressed,
                winit::event::MouseButton::Right,
            );
        }
        Some("sidebar_hover") => {
            let rect =
                semantic_target_rect(driver, |target| target.starts_with("row-action:1:"))
                    .ok_or_else(|| LaunchError::Operation("sidebar hover action missing".into()))?;
            move_pointer(driver, rect.center());
        }
        Some("note_menu") => {
            let rect = semantic_target_rect(driver, |target| target == "notes.page-title")
                .ok_or_else(|| LaunchError::Operation("note title target missing".into()))?;
            pointer_event(
                driver,
                rect.center(),
                winit::event::ElementState::Pressed,
                winit::event::MouseButton::Right,
            );
        }
        Some("rename" | "rename_commit") => {
            if semantic_name_rect(driver, "Launch checklist").is_none()
                && let Some(toggle) =
                    semantic_target_rect(driver, |target| target == "notes.drawer")
            {
                driver.click_at(toggle.center().x, toggle.center().y);
                driver.relayout();
            }
            let rect = semantic_name_rect(driver, "Launch checklist")
                .ok_or_else(|| LaunchError::Operation("rename row missing".into()))?;
            driver.double_click_at(rect.center().x, rect.center().y);
            if scenario == Some("rename_commit") {
                driver.type_text(" v2");
                driver.click_at(900.0, 400.0);
            }
        }
        Some("shortcuts" | "shortcuts_bottom") => {
            if semantic_name_rect(driver, "Launch checklist").is_none()
                && let Some(toggle) =
                    semantic_target_rect(driver, |target| target == "notes.drawer")
            {
                driver.click_at(toggle.center().x, toggle.center().y);
                driver.relayout();
            }
            let rect = semantic_name_rect(driver, "Launch checklist")
                .ok_or_else(|| LaunchError::Operation("sidebar focus target missing".into()))?;
            driver.click_at(rect.center().x, rect.center().y);
            driver.key("?");
            if scenario == Some("shortcuts_bottom") {
                driver.key("End");
            }
        }
        Some("find") => {
            chord(driver, &["ctrl"], "f");
            driver.type_text("migration");
        }
        Some("multiline_typing" | "paragraph_click") => {
            new_note(driver)?;
            let enter_text = |driver: &mut FrameHarness, text: &str| {
                if scenario == Some("paragraph_click") {
                    // This capture proves caret placement after pointer input;
                    // a real IME commit builds the same document without
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
            if scenario == Some("paragraph_click") {
                driver.relayout();
                let rects = visible_block_rects(driver);
                let [first, second] = rects.as_slice() else {
                    return Err(LaunchError::Operation(format!(
                        "paragraph click capture expected two block targets, found {}",
                        rects.len()
                    )));
                };
                driver.click_at(first.x0 + first.width() * 0.18, first.center().y);
                driver.click_at(second.x0 + second.width() * 0.28, second.center().y);
                driver.click_at(first.x0 + first.width() * 0.72, first.center().y);
            }
        }
        Some("typed_lists") => {
            new_note(driver)?;
            for (index, line) in [
                "- Gather ideas",
                "Write a first draft",
                "",
                "1. Review the draft",
                "Share with the team",
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
        Some("slash") => {
            new_note(driver)?;
            driver.type_text("/");
        }
        Some("slash_applied") => {
            new_note(driver)?;
            driver.type_text("/h1");
            driver.key("Enter");
            driver.type_text("Overview");
        }
        Some("heading") => {
            new_note(driver)?;
            driver.type_text("# Roadmap");
            driver.key("Enter");
            driver.type_text("Ship by Q3");
        }
        Some("title_edit") => {
            driver
                .click_named("note_title")
                .map_err(LaunchError::Operation)?;
            driver.type_text("Launch checklist notes");
            driver
                .expect(&SemanticExpectation::named("note_title").value("Launch checklist notes"))
                .map_err(LaunchError::Operation)?;
        }
        Some("recipes") => open_note(driver, "Recipes")?,
        Some("formula") => open_note(driver, "Formula notebook")?,
        Some("table_cell_edit") => {
            driver.relayout();
            let target = semantic_target(driver, |name| {
                name.starts_with("notes.table-cell:") && name.ends_with(":1:1")
            })
            .ok_or_else(|| LaunchError::Operation("editable table cell target missing".into()))?;
            driver
                .perform_semantic_named("block_editor", Some(&target), &ui::a11y::Action::Activate)
                .map_err(LaunchError::Operation)?;
            driver.key("End");
            driver.type_text(" Chen");
        }
        Some("image" | "image_edit") => {
            let deadline = ui::time::now() + std::time::Duration::from_secs(2);
            let target = loop {
                driver.relayout();
                if let Some(target) =
                    semantic_target(driver, |name| name.starts_with("notes.image-edit:"))
                {
                    break target;
                }
                if ui::time::now() >= deadline {
                    return Err(LaunchError::Operation(
                        "image preview did not become ready within two seconds".into(),
                    ));
                }
                let _ = ui::rt::sleep_thread(std::time::Duration::from_millis(5));
            };
            if scenario == Some("image_edit") {
                driver
                    .perform_semantic_named(
                        "block_editor",
                        Some(&target),
                        &ui::a11y::Action::Activate,
                    )
                    .map_err(LaunchError::Operation)?;
            }
        }
        Some("table_large_scrolled" | "scrolled") => {
            driver.relayout();
            let target = visible_block_rects(driver)
                .into_iter()
                .next()
                .ok_or_else(|| {
                    LaunchError::Operation("scroll capture has no visible block".into())
                })?;
            driver.click_at(target.center().x, target.center().y);
            driver.scroll(0.0, -1000.0);
        }
        Some("fenced_code" | "fenced_code_opening") => {
            new_note(driver)?;
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
            new_note(driver)?;
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
                .ok_or_else(|| LaunchError::Operation("code group drag handle missing".into()))?;
                let blocks = visible_block_rects(driver);
                let destination = blocks.get(3).ok_or_else(|| {
                    LaunchError::Operation("code group drag destination missing".into())
                })?;
                pointer_event(
                    driver,
                    from.center(),
                    winit::event::ElementState::Pressed,
                    winit::event::MouseButton::Left,
                );
                move_pointer(
                    driver,
                    vello::kurbo::Point::new(from.center().x, destination.center().y),
                );
                if scenario == Some("code_group_drop") {
                    pointer_event(
                        driver,
                        vello::kurbo::Point::new(from.center().x, destination.center().y),
                        winit::event::ElementState::Released,
                        winit::event::MouseButton::Left,
                    );
                }
            }
        }
        Some("sidebar_scroll") => {
            if let Some(toggle) = semantic_target_rect(driver, |target| target == "notes.drawer") {
                driver.click_at(toggle.center().x, toggle.center().y);
                driver.relayout();
            }
            let sidebar = semantic_name_rect(driver, "Reference 01")
                .ok_or_else(|| LaunchError::Operation("scrollable notes tree missing".into()))?;
            move_pointer(driver, sidebar.center());
            for _ in 0..10 {
                driver.scroll(0.0, -3.0);
            }
        }
        Some("presence") => {
            use ui::permissions::UserId;
            use ui::presence::{PresenceCtx, PresenceKey, SelectionRange};
            let mut presence = PresenceCtx::empty();
            let alice = UserId("alice".into());
            let bob = UserId("bob".into());
            presence.set_display_name(alice.clone(), "Alice");
            presence.set_display_name(bob, "Bob");
            let fifth = visible_block_ids(driver)
                .into_iter()
                .nth(4)
                .ok_or_else(|| {
                    LaunchError::Operation("presence capture needs five blocks".into())
                })?;
            let block = ui::widget::block_editor::block_presence_key(&fifth);
            presence.set_caret(&block, alice.clone(), 18);
            presence.record_peer_typing(
                alice,
                block,
                SelectionRange { start: 0, end: 18 },
                ui::time::now(),
            );
            presence.upsert_remote_cursor("alice".into(), [560.0, 300.0], Some("Alice"));
            if let Some(other) = driver
                .resources()
                .into_iter()
                .flat_map(|resource| resource.children)
                .find(|resource| {
                    resource.kind == "notes.page" && resource.label != "Launch checklist"
                })
            {
                presence.register_peer_at(
                    "bob".into(),
                    PresenceKey::Tab {
                        id: other.id.as_str().to_owned(),
                    },
                );
            }
            presence.upsert_remote_cursor("bob".into(), [1500.0, 380.0], Some("Bob"));
            driver.set_presence_context(presence);
        }
        Some("saving" | "save_error") => {
            let first = visible_block_rects(driver)
                .into_iter()
                .next()
                .ok_or_else(|| LaunchError::Operation("status edit target missing".into()))?;
            driver.click_at(first.x0 + first.width() * 0.8, first.center().y);
            driver.type_text("!");
            if scenario == Some("save_error") {
                let flush = driver.flush_component_records();
                SAVE_FAULT_RELEASE.with_borrow_mut(|slot| {
                    if let Some(signal) = slot.take() {
                        let _ = signal.send(());
                    }
                });
                if flush.is_ok() {
                    return Err(LaunchError::Operation(
                        "capture persistence fault did not fail the writer".into(),
                    ));
                }
                // Let the temporary filesystem fault clear. The writer keeps
                // its real sticky error visible until close retries it.
                let _ = ui::rt::sleep_thread(std::time::Duration::from_millis(50));
                for _ in 0..100 {
                    driver.relayout();
                    if semantic_observed(driver, |node| node.name.as_deref() == Some("Save failed"))
                    {
                        break;
                    }
                    let _ = ui::rt::sleep_thread(std::time::Duration::from_millis(10));
                }
            }
        }
        Some("import_success") => {
            let arguments = serde_json::json!({
                "title": "Project brief",
                "body": "# Project brief\n\nImported through Notes' semantic Markdown action.\n\n- [x] Preserve headings\n- [x] Preserve lists\n"
            })
            .as_object()
            .cloned()
            .ok_or_else(|| LaunchError::Operation("invalid Markdown import fixture".into()))?;
            driver
                .invoke_bound_action(
                    &ui::bound_dispatch::BoundActionAddress {
                        instance_id: INSTANCE,
                        address: tilo_actions::ActionAddress::new(
                            "notes.notebook",
                            "import-markdown-content",
                        ),
                    },
                    &arguments,
                )
                .map_err(|error| LaunchError::Operation(error.to_string()))?;
        }
        _ => {
            return Err(LaunchError::Scenario(format!(
                "canonical capture scenario not implemented: {scenario:?}"
            )));
        }
    }
    for _ in 0..3 {
        driver.relayout();
    }
    Ok(())
}

fn captured_rect(driver: &FrameHarness, id: &str) -> Option<vello::kurbo::Rect> {
    fn access(node: &ui::a11y::AccessNode, id: &str) -> Option<vello::kurbo::Rect> {
        if node.semantic_key.as_deref() == Some(id) {
            return node.bounds.map(ui::a11y::AccessBounds::to_rect);
        }
        node.children.iter().find_map(|node| access(node, id))
    }
    fn retained(node: &ui::headless::InspectionNode, id: &str) -> Option<vello::kurbo::Rect> {
        node.semantics
            .as_ref()
            .and_then(|node| access(node, id))
            .or_else(|| node.children.iter().find_map(|node| retained(node, id)))
    }
    retained(&driver.inspection().root, &format!("notes.block:{id}"))
}

fn slash_menu_is_expanded(driver: &FrameHarness) -> bool {
    fn access(node: &ui::a11y::AccessNode) -> bool {
        (node.semantic_key.as_deref() == Some("notes.slash-menu")
            && node.state.expanded == Some(true)
            && !node.state.hidden)
            || node.children.iter().any(access)
    }
    fn retained(node: &ui::headless::InspectionNode) -> bool {
        node.semantics.as_ref().is_some_and(access) || node.children.iter().any(retained)
    }
    retained(&driver.inspection().root)
}

fn semantic_observed(
    driver: &FrameHarness,
    predicate: impl Fn(&ui::a11y::AccessNode) -> bool,
) -> bool {
    fn access(
        node: &ui::a11y::AccessNode,
        predicate: &impl Fn(&ui::a11y::AccessNode) -> bool,
    ) -> bool {
        predicate(node) || node.children.iter().any(|child| access(child, predicate))
    }
    fn retained(
        node: &ui::headless::InspectionNode,
        predicate: &impl Fn(&ui::a11y::AccessNode) -> bool,
    ) -> bool {
        node.semantics
            .as_ref()
            .is_some_and(|node| access(node, predicate))
            || node.children.iter().any(|child| retained(child, predicate))
    }
    retained(&driver.inspection().root, &predicate)
}

// Preserve the assertions from the original scenario driver, now observing
// canonical authored blocks and the mounted runner's actual semantic bounds.
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive capture assertions stay adjacent to their canonical authored scenarios"
)]
fn validate_capture(
    driver: &mut FrameHarness,
    records: &ui::record::RecordStore,
    scenario: Option<&str>,
    theme: &ui::Theme,
) -> Result<(), LaunchError> {
    use ui::widget::{Block, BlockKind};
    let mut panel = records
        .read(&crate::hosting::default_notebook().id, |doc| {
            let workspace = crate::notebook::project(doc, None)?;
            if scenario == Some("first_note") {
                return if workspace.pages().is_empty() {
                    Ok(None)
                } else {
                    Err("fresh fixture is not empty".to_owned())
                };
            }
            let title = match scenario {
                None
                | Some(
                    "rich_outline"
                    | "adjacent_code_fences"
                    | "quote"
                    | "title_edit"
                    | "mixed_outline"
                    | "transclusion"
                    | "external_link"
                    | "table"
                    | "table_large"
                    | "table_large_scrolled"
                    | "table_cell_edit"
                    | "image"
                    | "image_edit"
                    | "scrolled"
                    | "drawer"
                    | "drag"
                    | "block_drag"
                    | "block_type_menu"
                    | "menu"
                    | "sidebar_hover"
                    | "note_menu"
                    | "rename"
                    | "shortcuts"
                    | "shortcuts_bottom"
                    | "find"
                    | "sidebar_scroll"
                    | "presence"
                    | "status"
                    | "saving"
                    | "save_error",
                ) => "Launch checklist",
                Some("recipes") => "Recipes",
                Some("formula") => "Formula notebook",
                Some("rename_commit") => "Launch checklist v2",
                Some("import_success") => "Project brief",
                _ => "Writing",
            };
            let page = workspace
                .pages()
                .iter()
                .find(|page| page.title == title)
                .ok_or("capture note missing")?;
            let mut panel = crate::MainPanel::new();
            panel.bind_canonical_page(doc, page.id, title)?;
            Ok(Some(panel))
        })
        .ok_or_else(|| LaunchError::Operation("capture record missing".into()))?
        .map_err(LaunchError::Operation)?;
    let Some(panel) = panel.as_mut() else {
        return Ok(());
    };
    let blocks = panel.editor().blocks();
    let blocks_may_be_modal_hidden = matches!(scenario, Some("code_group_drag"));
    let rects = blocks
        .iter()
        .map(|block| {
            captured_rect(driver, &block.id)
                .or_else(|| blocks_may_be_modal_hidden.then_some(vello::kurbo::Rect::ZERO))
                .ok_or_else(|| {
                    LaunchError::Operation(format!(
                        "capture block {} has no rendered bounds",
                        block.id
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let code = |block: &Block| matches!(&block.kind, BlockKind::CodeBlock { lang: Some(lang) } if lang == "python");
    let observed = match scenario {
        Some("typed_lists") => {
            matches!(blocks, [first,second,third,fourth,fifth] if matches!(first.kind, BlockKind::Bullet{..}) && matches!(second.kind, BlockKind::Bullet{..}) && matches!(third.kind, BlockKind::Numbered{..}) && matches!(fourth.kind, BlockKind::Numbered{..}) && matches!(fifth.kind, BlockKind::Checklist{done:false,..}) && fifth.text() == "Follow up tomorrow")
        }
        Some("fenced_code_opening") => matches!(blocks, [a] if code(a) && a.text().is_empty()),
        Some("fenced_code") => {
            blocks.first().is_some_and(|a| {
                code(a) && a.text() == "message = \"Hello from a Python code block\""
            }) && blocks.last().is_some_and(|b| {
                matches!(b.kind, BlockKind::Paragraph) && b.text() == "A note below the code."
            })
        }
        Some(
            "code_keyboard" | "code_keyboard_active" | "code_group_drag" | "code_group_drop",
        ) => {
            let expected = if scenario == Some("code_keyboard_active") {
                3
            } else {
                4
            };
            if scenario == Some("code_group_drop") {
                let group = blocks.iter().filter(|block| code(block)).collect::<Vec<_>>();
                blocks.len() == 4
                    && group.len() == 3
                    && group.windows(2).all(|pair| {
                        pair.first().zip(pair.get(1)).is_some_and(|(left, right)| left.same_code_group(right))
                    })
                    && blocks.iter().any(|block| {
                        matches!(block.kind, BlockKind::Paragraph)
                            && block.text() == "Back to writing, without touching the mouse."
                    })
            } else {
                blocks.len() == expected
                    && blocks.iter().take(3).map(Block::text).eq([
                        "if ready:",
                        "    print('Ready')",
                        "    print('Still code')",
                    ])
                    && blocks.iter().take(3).all(code)
                    && (expected == 3
                        || blocks.last().is_some_and(|b| {
                            matches!(b.kind, BlockKind::Paragraph)
                                && b.text() == "Back to writing, without touching the mouse."
                        }))
            }
        }
        Some("rich_outline") => {
            blocks
                .iter()
                .map(Block::indent)
                .eq([0, 1, 1, 1, 1, 2, 2, 0])
                && matches!(blocks, [_,_,a,b,..] if code(a) && a.same_code_group(b))
                && matches!(rects.as_slice(), [a,b,_,_,c,d,..] if (b.x0-a.x0).abs()<1.0 && d.x0>c.x0)
        }
        Some("adjacent_code_fences") => {
            let fences = blocks
                .iter()
                .zip(&rects)
                .filter(|(block, _)| code(block))
                .collect::<Vec<_>>();
            matches!(fences.as_slice(), [(a,_),(b,br),(c,cr),(d,_)] if a.same_code_group(b) && !b.same_code_group(c) && c.same_code_group(d) && cr.y0 >= br.y1+44.0)
        }
        Some("multiline_typing" | "paragraph_click") => {
            let texts = matches!(blocks, [a,b] if a.text() == FIRST && b.text() == format!("{SECOND}\n{CONTINUATION}"));
            let separate = rects
                .windows(2)
                .all(|pair| matches!(pair, [a,b] if b.y0+0.5>=a.y1));
            let mut renderer = ui::TextRenderer::new();
            renderer.set_text_scale(theme.text_scale);
            let heights = blocks.iter().zip(&rects).all(|(block, rect)| {
                let (_, text_height) = renderer.measure(
                    block.text(),
                    block.kind.font_size(),
                    Some(ui::num::f32_of_f64(rect.width())),
                );
                rect.height() + 0.5 >= f64::from(text_height)
            });
            let click_landed = scenario != Some("paragraph_click")
                || blocks.first().is_some_and(|first| {
                    fn selected(node: &ui::a11y::AccessNode, key: &str) -> bool {
                        ((node.semantic_key.as_deref() == Some(key)
                            || node.action_target.as_deref() == Some(key))
                            && node.state.focused
                            && node.text_selection.is_some())
                            || node.children.iter().any(|child| selected(child, key))
                    }
                    fn retained(node: &ui::headless::InspectionNode, key: &str) -> bool {
                        node.semantics.as_ref().is_some_and(|node| selected(node, key))
                            || node.children.iter().any(|child| retained(child, key))
                    }
                    let snapshot = driver.inspection();
                    retained(&snapshot.root, &format!("notes.block:{}", first.id))
                        || retained(&snapshot.root, &format!("notes.block-text:{}", first.id))
                });
            texts && separate && heights && click_landed
        }
        Some("slash" | "block_type_menu") => slash_menu_is_expanded(driver),
        Some("slash_applied") => {
            matches!(blocks, [block] if matches!(block.kind, BlockKind::Heading(1)) && block.text() == "Overview")
        }
        Some("heading") => {
            driver
                .expect(&SemanticExpectation::named("note_title").value("Writing"))
                .is_ok()
                && matches!(blocks, [heading, body] if matches!(heading.kind, BlockKind::Heading(1)) && heading.text() == "Roadmap" && matches!(body.kind, BlockKind::Paragraph) && body.text() == "Ship by Q3")
        }
        Some("quote") => {
            matches!(blocks, [heading, quote, body] if matches!(heading.kind, BlockKind::Heading(2)) && matches!(quote.kind, BlockKind::Quote) && body.text().starts_with("Quotes remain editable"))
        }
        Some("title_edit") => driver
            .expect(&SemanticExpectation::named("note_title").value("Launch checklist notes"))
            .is_ok(),
        Some("mixed_outline") => {
            blocks.len() == 10
                && matches!(blocks.first().map(|block| &block.kind), Some(BlockKind::Heading(1)))
                && blocks
                    .iter()
                    .map(Block::indent)
                    .eq([0, 0, 1, 2, 2, 3, 1, 2, 1, 0])
                && matches!(blocks.get(3).map(|block| &block.kind), Some(BlockKind::Checklist { done: true, .. }))
                && matches!(blocks.get(4).map(|block| &block.kind), Some(BlockKind::Checklist { done: false, .. }))
                && matches!(blocks.get(5).map(|block| &block.kind), Some(BlockKind::Paragraph))
        }
        Some("recipes") => {
            blocks.iter().filter(|block| matches!(block.kind, BlockKind::Numbered { .. })).count() == 4
                && blocks.iter().any(|block| block.text() == "Weeknight pasta")
        }
        Some("formula") => {
            blocks.iter().any(|block| matches!(&block.kind, BlockKind::CodeBlock { lang: Some(lang) } if lang == "tex"))
                && blocks.iter().any(|block| matches!(&block.kind, BlockKind::CodeBlock { lang: Some(lang) } if lang.starts_with("typst")))
        }
        Some("transclusion") => blocks.iter().any(|block| {
            block.line.marks().iter().any(|span| matches!(span.mark, ui::widget::Mark::Transclusion(_)))
        }),
        Some("external_link") => blocks.iter().any(|block| {
            block.line.marks().iter().any(|span| matches!(&span.mark, ui::widget::Mark::ExternalLink(uri) if uri == "https://example.test/writing"))
        }),
        Some("table" | "table_large" | "table_large_scrolled") => {
            blocks.iter().any(|block| matches!(block.kind, BlockKind::Table { visual: true }))
        }
        Some("table_cell_edit") => blocks.iter().any(|block| {
            matches!(block.kind, BlockKind::Table { visual: true }) && block.text().contains("Mina Chen")
        }),
        Some("image" | "image_edit") => blocks.iter().any(|block| {
            block
                .text()
                .starts_with("![Settings external accounts](asset:")
        }),
        Some("scrolled") => blocks.len() > 20,
        Some("drawer") => semantic_observed(driver, |node| {
            node.action_target.as_deref() == Some("notes.drawer")
                && node.state.checked == Some(true)
        }),
        Some("menu" | "note_menu") => semantic_observed(driver, |node| {
            node.role == ui::a11y::Role::Menu && !node.state.hidden
        }),
        Some("sidebar_hover") => semantic_observed(driver, |node| {
            node.action_target
                .as_deref()
                .is_some_and(|target| target.starts_with("row-action:1:"))
        }),
        Some("rename") => semantic_observed(driver, |node| {
            node.role == ui::a11y::Role::TextInput
                && node.state.focused
                && node.value.as_deref() == Some("Launch checklist")
        }),
        Some("rename_commit") => driver
            .expect(
                &SemanticExpectation::named("note_title").value("Launch checklist v2"),
            )
            .is_ok(),
        Some("shortcuts" | "shortcuts_bottom") => semantic_observed(driver, |node| {
            node.role == ui::a11y::Role::Dialog
                && node.name.as_deref() == Some("Keyboard shortcuts")
                && !node.state.hidden
        }),
        Some("find") => semantic_observed(driver, |node| {
            node.role == ui::a11y::Role::TextInput
                && node.name.as_deref() == Some("Find in note")
                && node.value.as_deref() == Some("migration")
                && node.state.focused
        }),
        Some("sidebar_scroll") => semantic_observed(driver, |node| {
            node.name.as_deref() == Some("Reference 28") && node.bounds.is_some()
        }),
        Some("status") => {
            semantic_observed(driver, |node| node.name.as_deref() == Some("Saved"))
        }
        Some("saving") => {
            semantic_observed(driver, |node| node.name.as_deref() == Some("Saving…"))
        }
        Some("save_error") => {
            semantic_observed(driver, |node| node.name.as_deref() == Some("Save failed"))
        }
        Some("import_success") => {
            !blocks.iter().any(|block| {
                matches!(block.kind, BlockKind::Heading(1)) && block.text() == "Project brief"
            }) && blocks.iter().any(|block| {
                matches!(block.kind, BlockKind::Paragraph)
                    && block.text() == "Imported through Notes' semantic Markdown action."
            }) && semantic_observed(driver, |node| {
                node.name.as_deref() == Some("Notifications")
                    && node.value.as_deref() == Some("Imported Project brief")
            })
        }
        _ => true,
    };
    if !observed {
        let multiline_heights = matches!(scenario, Some("multiline_typing" | "paragraph_click"))
            .then(|| {
                let mut renderer = ui::TextRenderer::new();
                renderer.set_text_scale(theme.text_scale);
                blocks
                    .iter()
                    .zip(&rects)
                    .map(|(block, rect)| {
                        let (_, text_height) = renderer.measure(
                            block.text(),
                            block.kind.font_size(),
                            Some(ui::num::f32_of_f64(rect.width())),
                        );
                        (*rect, text_height)
                    })
                    .collect::<Vec<_>>()
            });
        return Err(LaunchError::Operation(format!(
            "capture scenario {scenario:?} did not reach its required state: {:?}; heights={multiline_heights:?}",
            blocks
                .iter()
                .zip(&rects)
                .map(|(b, r)| (&b.kind, b.text(), b.indent(), r))
                .collect::<Vec<_>>()
        )));
    }
    Ok(())
}

/// Render supported captures while legacy scenario ports are completed.
/// # Errors
/// Keeps close recovery owners attached to failures instead of discarding them.
pub fn render_to_path(
    viewport: (u32, u32),
    out: &Path,
    scenario: Option<&str>,
    scale: Option<f32>,
) -> Result<(), LaunchError> {
    let theme = ui::launch::capture_theme(scale);
    let mut text_renderer = ui::TextRenderer::new();
    text_renderer.set_text_scale(scale.unwrap_or(1.0));
    let (prepared, records) = prepare_fixture_owned(
        scenario,
        ComponentRunnerInputs {
            theme: theme.clone(),
            text_renderer,
        },
    )?;
    let component =
        crate::definition().instantiate(prepared.context, prepared.target, prepared.restore)?;
    let mut driver = FrameHarness::from_component(component, viewport, prepared.runner_inputs)?;
    let result = drive_fixture(&mut driver, scenario)
        .and_then(|()| validate_capture(&mut driver, &records, scenario, &theme));
    if result.is_ok() {
        driver.render_window_png(out);
    }
    driver.close_components();
    let reports = driver.take_component_close_reports();
    if let Err(source) = result {
        return Err(LaunchError::ComponentCapture {
            source: Box::new(source),
            recovery: LaunchRecovery::reports(reports),
        });
    }
    if reports.iter().any(|report| {
        report.record_error.is_some()
            || report.view_error.is_some()
            || report.teardown_error.is_some()
    }) {
        return Err(LaunchError::ComponentCapture {
            source: Box::new(LaunchError::Operation("capture close failed".into())),
            recovery: LaunchRecovery::reports(reports),
        });
    }
    Ok(())
}
