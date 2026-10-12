//! Every screenshot scenario reaches the state it is named for: the frame
//! `--shots --scenario NAME` writes, composed by the framework from the
//! scenario's actions, staging and input, checked against the notebook it
//! wrote and the window's accessibility tree. Each capture is isolated (its
//! own profile, record store and harness), so they run on several threads;
//! the test reserves the runner in `.config/nextest.toml`.

use std::sync::atomic::{AtomicUsize, Ordering};

use notes::NotesShell;
use notes::scenario::{CONTINUATION, FIRST, SECOND};
use ui::framework::ResourceApp;
use ui::framework::testing::TestApp;
use ui::headless::{FrameHarness, InspectionNode, SemanticExpectation};
use ui::widget::{Block, BlockKind};
use vello::kurbo::Rect;

/// Bounds peak memory (each capture peaks near 260 MB in a debug build).
const MAX_WORKERS: usize = 8;

/// A phone-sized window, where the notes tree is a drawer.
const NARROW: (u32, u32) = (390, 844);

/// Scenarios also checked in a phone-sized window.
const NARROW_SCENARIOS: &[&str] = &[
    "code_keyboard_active",
    "rich_outline",
    "menu",
    "heading",
    "slash_applied",
    "shortcuts",
    "shortcuts_bottom",
    "note_menu",
    "table_large",
    "table_large_scrolled",
    "fenced_code",
    "sidebar_scroll",
    "scrolled",
];

#[test]
fn semantic_markdown_import_reports_original_success_notification() {
    ui::clock::freeze_for_capture();
    check(Some("import_success"), (1280, 720)).unwrap();
    let mut app = TestApp::<NotesShell>::capture_at(Some("import_success"), (1280, 720)).unwrap();
    // A capture retains its original notification even when frame preparation
    // exceeds the live toast lifetime. Draw the real app after that delay.
    std::thread::sleep(ui::widget::toast::DEFAULT_TTL + ui::time::Duration::from_millis(100));
    app.harness().relayout();
    assert!(observed(app.harness(), |node| {
        node.name.as_deref() == Some("Notifications")
            && node.value.as_deref() == Some("Imported Project brief")
    }));
}

#[test]
fn every_scenario_reaches_its_state() {
    // Match --shots before starting capture workers: transient notifications
    // belong to the captured scene and must not expire while it is prepared.
    ui::clock::freeze_for_capture();
    let mut captures = NotesShell::scenarios()
        .iter()
        .map(|scenario| (Some(*scenario), NotesShell::shot_viewport(Some(scenario))))
        .collect::<Vec<_>>();
    captures.push((None, NotesShell::shot_viewport(None)));
    captures.extend(
        NARROW_SCENARIOS
            .iter()
            .map(|scenario| (Some(*scenario), NARROW)),
    );
    let failures = std::sync::Mutex::new(Vec::new());
    let next = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .clamp(1, MAX_WORKERS)
        .min(captures.len());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while let Some(&(scenario, viewport)) =
                    captures.get(next.fetch_add(1, Ordering::Relaxed))
                {
                    let outcome = std::panic::catch_unwind(|| check(scenario, viewport))
                        .unwrap_or_else(|_| Err("panicked".into()));
                    if let Err(error) = outcome {
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("{scenario:?} at {viewport:?}: {error}"));
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "scenarios failed:\n{}",
        failures.join("\n")
    );
}

#[test]
fn imported_note_keeps_its_success_notification_in_capture() {
    ui::clock::freeze_for_capture();
    check(Some("import_success"), (1280, 720))
        .expect("the imported document and its notification are both captured");
}

#[test]
fn rename_scenario_keeps_its_real_inline_editor_focused() {
    check(Some("rename"), (1280, 720)).expect("the real double-click rename remains focused");
}

fn check(scenario: Option<&str>, viewport: (u32, u32)) -> Result<(), String> {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::capture_at(scenario, viewport)
        .map_err(|error| format!("capture failed: {error:?}"))?;
    let Some(title) = shown_title(scenario) else {
        let pages = app.read_document(notes::read::list_pages);
        return if pages.is_empty() {
            Ok(())
        } else {
            Err("a first note starts from an empty notebook".into())
        };
    };
    let panel = app.read_document(|doc| {
        let workspace = notes::read::project(doc)?;
        let page = workspace
            .pages()
            .iter()
            .find(|page| page.title == title)
            .ok_or_else(|| format!("note {title:?} missing"))?;
        let mut panel = notes::MainPanel::new();
        panel.bind_canonical_page(doc, page.id, title)?;
        Ok::<_, String>(panel)
    })?;
    let blocks = panel.editor().blocks();
    let driver = app.harness();
    let modal_hides_blocks = scenario == Some("code_group_drag");
    let rects = blocks
        .iter()
        .map(|block| {
            block_rect(driver, &block.id)
                .or_else(|| modal_hides_blocks.then_some(Rect::ZERO))
                .ok_or_else(|| format!("block {} has no rendered bounds", block.id))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if reached(driver, scenario, blocks, &rects) {
        Ok(())
    } else {
        Err(format!(
            "did not reach its state: {:?}; accessibility: {:?}",
            blocks
                .iter()
                .zip(&rects)
                .map(|(block, rect)| (&block.kind, block.text(), block.indent(), rect))
                .collect::<Vec<_>>(),
            driver.a11y_nodes()
        ))
    }
}

/// The note the scenario's checks read: the one it shows, after its input.
fn shown_title(scenario: Option<&str>) -> Option<&'static str> {
    match scenario {
        Some("rename_commit") => Some("Launch checklist v2"),
        Some("title_edit") => Some("Launch checklist"),
        _ => notes::scenario::shown_note(scenario),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one table of every scenario's checks keeps them comparable"
)]
fn reached(
    driver: &mut FrameHarness,
    scenario: Option<&str>,
    blocks: &[Block],
    rects: &[Rect],
) -> bool {
    let code = |block: &Block| matches!(&block.kind, BlockKind::CodeBlock { lang: Some(lang) } if lang == "python");
    let back_to_writing = |block: &Block| {
        matches!(block.kind, BlockKind::Paragraph)
            && block.text() == "Back to writing, without touching the mouse."
    };
    match scenario {
        Some("typed_lists") => {
            matches!(blocks, [first, second, third, fourth, fifth]
                if matches!(first.kind, BlockKind::Bullet { .. })
                && matches!(second.kind, BlockKind::Bullet { .. })
                && matches!(third.kind, BlockKind::Numbered { .. })
                && matches!(fourth.kind, BlockKind::Numbered { .. })
                && matches!(fifth.kind, BlockKind::Checklist { done: false, .. })
                && fifth.text() == "Follow up tomorrow")
        }
        Some("fenced_code_opening") => matches!(blocks, [a] if code(a) && a.text().is_empty()),
        Some("fenced_code") => {
            blocks.first().is_some_and(|a| {
                code(a) && a.text() == "message = \"Hello from a Python code block\""
            }) && blocks.last().is_some_and(|b| {
                matches!(b.kind, BlockKind::Paragraph) && b.text() == "A note below the code."
            })
        }
        Some("code_group_drop") => {
            let group = blocks.iter().filter(|block| code(block)).collect::<Vec<_>>();
            blocks.len() == 4
                && group.len() == 3
                && group.windows(2).all(|pair| {
                    pair.first()
                        .zip(pair.get(1))
                        .is_some_and(|(left, right)| left.same_code_group(right))
                })
                && blocks.iter().any(back_to_writing)
        }
        Some("code_keyboard" | "code_keyboard_active" | "code_group_drag") => {
            let expected = if scenario == Some("code_keyboard_active") {
                3
            } else {
                4
            };
            blocks.len() == expected
                && blocks.iter().take(3).map(Block::text).eq([
                    "if ready:",
                    "    print('Ready')",
                    "    print('Still code')",
                ])
                && blocks.iter().take(3).all(code)
                && (expected == 3 || blocks.last().is_some_and(back_to_writing))
        }
        Some("rich_outline") => {
            blocks
                .iter()
                .map(Block::indent)
                .eq([0, 1, 1, 1, 1, 2, 2, 0])
                && matches!(blocks, [_, _, a, b, ..] if code(a) && a.same_code_group(b))
                && matches!(rects, [a, b, _, _, c, d, ..] if (b.x0 - a.x0).abs() < 1.0 && d.x0 > c.x0)
        }
        Some("adjacent_code_fences") => {
            let fences = blocks
                .iter()
                .zip(rects)
                .filter(|(block, _)| code(block))
                .collect::<Vec<_>>();
            matches!(fences.as_slice(), [(a, _), (b, br), (c, cr), (d, _)]
                if a.same_code_group(b) && !b.same_code_group(c) && c.same_code_group(d)
                && cr.y0 >= br.y1 + 44.0)
        }
        Some("multiline_typing" | "paragraph_click") => {
            let texts = matches!(blocks, [a, b]
                if a.text() == FIRST && b.text() == format!("{SECOND}\n{CONTINUATION}"));
            let separate = rects
                .windows(2)
                .all(|pair| matches!(pair, [a, b] if b.y0 + 0.5 >= a.y1));
            let mut renderer = ui::TextRenderer::new();
            let heights = blocks.iter().zip(rects).all(|(block, rect)| {
                let (_, text_height) = renderer.measure(
                    block.text(),
                    ui::Theme::dark().tokens.typography.size(block.kind.text_role()),
                    Some(ui::num::f32_of_f64(rect.width())),
                );
                rect.height() + 0.5 >= f64::from(text_height)
            });
            let click_landed = scenario != Some("paragraph_click")
                || blocks.first().is_some_and(|first| {
                    let keys = [
                        format!("notes.block:{}", first.id),
                        format!("notes.block-text:{}", first.id),
                    ];
                    observed(driver, |node| {
                        (node.semantic_key.as_ref().is_some_and(|key| keys.contains(key))
                            || node
                                .action_target
                                .as_ref()
                                .is_some_and(|key| keys.contains(key)))
                            && node.state.focused
                            && node.text_selection.is_some()
                    })
                });
            texts && separate && heights && click_landed
        }
        Some("slash" | "block_type_menu") => observed(driver, |node| {
            node.semantic_key.as_deref() == Some("notes.slash-menu")
                && node.state.expanded == Some(true)
                && !node.state.hidden
        }),
        Some("slash_applied") => {
            matches!(blocks, [block] if matches!(block.kind, BlockKind::Heading(1)) && block.text() == "Overview")
        }
        Some("heading") => {
            driver
                .expect(&SemanticExpectation::named("note_title").value("Writing"))
                .is_ok()
                && matches!(blocks, [heading, body]
                    if matches!(heading.kind, BlockKind::Heading(1)) && heading.text() == "Roadmap"
                    && matches!(body.kind, BlockKind::Paragraph) && body.text() == "Ship by Q3")
        }
        Some("quote") => {
            matches!(blocks, [heading, quote, body]
                if matches!(heading.kind, BlockKind::Heading(2))
                && matches!(quote.kind, BlockKind::Quote)
                && body.text().starts_with("Quotes remain editable"))
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
            blocks
                .iter()
                .filter(|block| matches!(block.kind, BlockKind::Numbered { .. }))
                .count()
                == 4
                && blocks.iter().any(|block| block.text() == "Weeknight pasta")
        }
        Some("formula") => {
            blocks.iter().any(|block| matches!(&block.kind, BlockKind::CodeBlock { lang: Some(lang) } if lang == "tex"))
                && blocks.iter().any(|block| matches!(&block.kind, BlockKind::CodeBlock { lang: Some(lang) } if lang.starts_with("typst")))
        }
        Some("transclusion") => blocks.iter().any(|block| {
            block
                .line
                .marks()
                .iter()
                .any(|span| matches!(span.mark, ui::widget::Mark::Transclusion(_)))
        }),
        Some("external_link") => blocks.iter().any(|block| {
            block.line.marks().iter().any(|span| {
                matches!(&span.mark, ui::widget::Mark::ExternalLink(uri) if uri == "https://example.test/writing")
            })
        }),
        Some("table" | "table_large" | "table_large_scrolled") => blocks
            .iter()
            .any(|block| matches!(block.kind, BlockKind::Table { visual: true })),
        Some("table_cell_edit") => blocks.iter().any(|block| {
            matches!(block.kind, BlockKind::Table { visual: true })
                && block.text().contains("Mina Chen")
        }),
        Some("image_edit") => blocks.iter().any(|block| {
            block
                .text()
                .starts_with("![Settings external accounts](asset:")
        }),
        Some("image") => blocks.iter().any(|block| {
            block
                .text()
                .starts_with("![Settings external accounts](asset:")
        }) && observed(driver, |node| {
            node.action_target
                .as_deref()
                .is_some_and(|target| target.starts_with("notes.image-edit:"))
        }),
        Some("scrolled") => blocks.len() > 20,
        Some("drawer") => {
            // The retained shell publishes its real toggle rather than the
            // former manually painted notes.drawer synthetic checkbox.
            observed(driver, |node| {
                node.name.as_deref() == Some("Hide sidebar") && !node.state.hidden
            }) && observed(driver, |node| {
                node.role == ui::a11y::Role::Tree && !node.state.hidden
            }) && observed(driver, |node| {
                // TreeView publishes geometry on the visible rows, not on
                // its semantic Tree container. Require a genuinely laid-out
                // row as well as the opened shell's dismiss control.
                node.role == ui::a11y::Role::TreeItem && !node.state.hidden
                    && node.bounds.is_some_and(|bounds| {
                        let rect = bounds.to_rect();
                        rect.width() > 0.0 && rect.height() > 0.0
                    })
            })
        },
        Some("menu" | "note_menu") => observed(driver, |node| {
            node.role == ui::a11y::Role::Menu && !node.state.hidden
        }),
        Some("sidebar_hover") => observed(driver, |node| {
            node.action_target
                .as_deref()
                .is_some_and(|target| target.starts_with("row-action:1:"))
        }),
        Some("rename") => observed(driver, |node| {
            node.role == ui::a11y::Role::TextInput
                && node.state.focused
                && node.value.as_deref() == Some("Launch checklist")
        }),
        Some("rename_commit") => driver
            .expect(&SemanticExpectation::named("note_title").value("Launch checklist v2"))
            .is_ok(),
        Some("shortcuts" | "shortcuts_bottom") => observed(driver, |node| {
            node.role == ui::a11y::Role::Dialog
                && node.name.as_deref() == Some("Keyboard shortcuts")
                && !node.state.hidden
        }),
        Some("find") => observed(driver, |node| {
            node.role == ui::a11y::Role::TextInput
                && node.name.as_deref() == Some("Find in note")
                && node.value.as_deref() == Some("migration")
                && node.state.focused
        }),
        Some("sidebar_scroll") => observed(driver, |node| {
            node.name.as_deref() == Some("Reference 28") && node.bounds.is_some()
        }),
        Some("status") => observed(driver, |node| node.name.as_deref() == Some("Saved")),
        Some("saving") => observed(driver, |node| node.name.as_deref() == Some("Saving…")),
        Some("save_error") => observed(driver, |node| node.name.as_deref() == Some("Save failed")),
        Some("import_success") => {
            !blocks.iter().any(|block| {
                matches!(block.kind, BlockKind::Heading(1)) && block.text() == "Project brief"
            }) && blocks.iter().any(|block| {
                matches!(block.kind, BlockKind::Paragraph)
                    && block.text() == "Imported through Notes' semantic Markdown action."
            }) && observed(driver, |node| {
                node.name.as_deref() == Some("Notifications")
                    && node.value.as_deref() == Some("Imported Project brief")
            })
        }
        _ => driver
            .expect(&SemanticExpectation::named("note_title").value("Launch checklist"))
            .is_ok(),
    }
}

/// Whether any accessibility node of the window matches `predicate`.
fn observed(driver: &FrameHarness, predicate: impl Fn(&ui::a11y::AccessNode) -> bool) -> bool {
    fn access(
        node: &ui::a11y::AccessNode,
        predicate: &impl Fn(&ui::a11y::AccessNode) -> bool,
    ) -> bool {
        predicate(node) || node.children.iter().any(|child| access(child, predicate))
    }
    fn retained(node: &InspectionNode, predicate: &impl Fn(&ui::a11y::AccessNode) -> bool) -> bool {
        node.semantics
            .as_ref()
            .is_some_and(|node| access(node, predicate))
            || node.children.iter().any(|child| retained(child, predicate))
    }
    retained(&driver.inspection().root, &predicate)
}

fn block_rect(driver: &FrameHarness, id: &str) -> Option<Rect> {
    fn access(node: &ui::a11y::AccessNode, key: &str) -> Option<Rect> {
        if node.semantic_key.as_deref() == Some(key) {
            return node.bounds.map(ui::a11y::AccessBounds::to_rect);
        }
        node.children.iter().find_map(|node| access(node, key))
    }
    fn retained(node: &InspectionNode, key: &str) -> Option<Rect> {
        node.semantics
            .as_ref()
            .and_then(|node| access(node, key))
            .or_else(|| node.children.iter().find_map(|node| retained(node, key)))
    }
    retained(&driver.inspection().root, &format!("notes.block:{id}"))
}
