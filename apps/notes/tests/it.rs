// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "click_link_navigates.rs"]
mod click_link_navigates;
#[path = "cold_start.rs"]
mod cold_start;
#[path = "complete_capture.rs"]
mod complete_capture;
#[path = "e2e_smoke.rs"]
mod e2e_smoke;
#[path = "effects.rs"]
mod effects;
#[path = "framework_body.rs"]
mod framework_body;
#[path = "framework_new_note.rs"]
mod framework_new_note;
#[path = "framework_save_status.rs"]
mod framework_save_status;
#[path = "history_and_agents.rs"]
mod history_and_agents;
#[path = "interaction_event_path.rs"]
mod interaction_event_path;
#[path = "launch_cli.rs"]
mod launch_cli;
#[path = "live_block_anchor.rs"]
mod live_block_anchor;
#[path = "main_panel_scroll.rs"]
mod main_panel_scroll;
#[path = "markdown_marks_roundtrip.rs"]
mod markdown_marks_roundtrip;
#[path = "markdown_phase3_roundtrip.rs"]
mod markdown_phase3_roundtrip;
#[path = "offscreen_smoke.rs"]
mod offscreen_smoke;
#[path = "outline_commands.rs"]
mod outline_commands;
#[path = "page_edit_persistence.rs"]
mod page_edit_persistence;
#[path = "paste_image.rs"]
mod paste_image;
#[path = "read_content.rs"]
mod read_content;
#[path = "responsive_drawer.rs"]
mod responsive_drawer;
#[path = "scroll_damage.rs"]
mod scroll_damage;
#[path = "sidebar_collab_growth.rs"]
mod sidebar_collab_growth;
#[path = "sidebar_dnd.rs"]
mod sidebar_dnd;
#[path = "slash_menu_focus.rs"]
mod slash_menu_focus;
#[path = "tab_nesting.rs"]
mod tab_nesting;
#[path = "tree_operations.rs"]
mod tree_operations;
#[path = "two_peer_sync.rs"]
mod two_peer_sync;
#[path = "typing_damage.rs"]
mod typing_damage;
#[path = "typing_order.rs"]
mod typing_order;
#[path = "welcome_page_renders_all_block_kinds.rs"]
mod welcome_page_renders_all_block_kinds;
#[path = "workspace_persistence.rs"]
mod workspace_persistence;

#[path = "canonical_component.rs"]
mod canonical_component;
#[path = "support/component.rs"]
mod component_support;
#[path = "support/framework.rs"]
mod framework_support;

#[path = "canonical_actions.rs"]
mod canonical_actions;

#[path = "canonical_views.rs"]
mod canonical_views;

#[path = "note_context_menu.rs"]
mod note_context_menu;
#[path = "note_header.rs"]
mod note_header;
#[path = "semantic_input.rs"]
mod semantic_input;

#[path = "vertical_editing.rs"]
mod vertical_editing;

#[path = "table_preview.rs"]
mod table_preview;

#[path = "table_editing.rs"]
mod table_editing;

#[path = "soft_line_breaks.rs"]
mod soft_line_breaks;

#[path = "group_moves.rs"]
mod group_moves;

#[path = "code_groups.rs"]
mod code_groups;

#[path = "format_preservation.rs"]
mod format_preservation;

#[path = "capture_runtime.rs"]
mod capture_runtime;

#[path = "scenarios.rs"]
mod scenarios;

#[path = "block_commands.rs"]
mod block_commands;

#[path = "clipboard_safety.rs"]
mod clipboard_safety;

#[path = "find_replace.rs"]
mod find_replace;

mod slash_text_input;

#[path = "paragraph_damage.rs"]
mod paragraph_damage;
