# Slides: the Rust app's features and where they live in tilolang

The Rust app (`apps/slides`) is not deleted yet: the port below is a draft. Each row names the tl
home it has or needs, and its test.

## Deck model and actions

| Rust feature | tl home | Test |
|---|---|---|
| `slides.deck` resource: title, palette, look, slides | `apps/tl/slides/app.tl`: `Deck`, `Slide`, `Element` | `scenarios.tl` deck; engine round-trip test |
| `add-slide`, `add-slides` | `add_slide`, `add_slides` | scenario + action test |
| `insert-slide`, `duplicate-slide`, `delete-slide`, `move-slide` | `duplicate_slide`, `delete_slide`, `move_slide` (insertion is `add_slide` + `place_slide`) | action tests |
| `set-text` (first two text boxes) | `set_text` | action test |
| `add-element`, `insert-element`, `delete-element` | `add_element`, `insert_element`, `delete_element` | action tests |
| `bring-to-front`, `send-to-back` | `bring_to_front`, `send_to_back` | action test |
| `replace-deck` | `adopt` (imported file) and the one undoable drop of the old starter slide | import test |
| Deck title, palette, look, slide layout and background edits | `set_title`, `set_palette`, `set_look`, `set_layout`, `set_background` | action test |
| Text styling (fill, stroke, size, align, bold, italic, radius) | `set_style`, `bind:` on `ui.BlockEditor` for words | action test |
| Rich-text editing of a box (bold, italic, lists) | `ui.BlockEditor(bind: e.body)` | editor walk |
| Speaker notes | `Slide.notes` (`RichText`), `set_notes`, `ui.BlockEditor(bind: s.notes)` | walk |
| Element move, resize, nudge, snapping | `move_element`, `resize_element`, `nudge_element`; drag on `ui.Map` (`drop`, `resize`) | action tests |
| Copy, cut, paste elements | `copy_element`, `paste_element` (`uses clipboard`) | action test |
| Aggregate limits (1000 slides, 10k elements, 2M chars, 2048 pictures) | engine `within_limits`, checked on import | engine test |
| Read deck as text | `read_deck` | action test |

## Files and pictures

| Rust feature | tl home | Test |
|---|---|---|
| `.odp` read/write | `crates/engines/tilo-deck` (`odp.rs`), `file.presentation()` / `File.of_presentation` | engine round-trip test |
| `.pptx` read/write | `crates/engines/tilo-deck` (`pptx.rs`), same bindings | engine round-trip test |
| Pictures bundled in the file, content-addressed in the deck | `Pictures` in the engine over the content store | engine round-trip test |
| Open a presentation (its kind opens the app) | `open_presentation` / `import_file` with `opens:` | scenario |
| Save as `.odp` / `.pptx`, save to the person's files | `export_odp`, `export_pptx`, `save_odp`, `save_pptx` | scenario |
| Insert a picture from the person's files | `insert_picture`, `add_picture` (`uses files, image`) | scenario |

## Views and interaction

| Rust feature | tl home | Test |
|---|---|---|
| Slide rail with thumbnails, reorder, duplicate, delete, drop-to-delete | rail rows in `slide_rail` (title + number); **live thumbnails missing — needs a canvas component** | walk |
| Editor stage: elements at their places, selection, drag, resize | `stage_of` over `ui.Map`/`ui.MapPin`/`ui.DragSource`; **styled text (per-box size/align), shape fills and custom colours missing — needs a stage component** | walk |
| Inspector: fill, stroke, font, bold, italic, lists, align, arrange, layout, background, look, palette, notes | `inspector`; **custom colour picker and role swatches at slide colours missing** | walk |
| Window bar: title, editor/overview, insert tools, present, properties | `ui.NavShell` header (title, Insert, Present, rail, properties) | walk |
| Overview grid | not yet ported; needs `ui.MediaGrid` over slide pictures, which needs the stage component to draw a slide to a picture | walk |
| Present mode: fullscreen on the largest/primary-external display | `ui.Fullscreen` (new described component over `FullscreenProvider`) | walk |
| Presenter view: current, next, notes | `presenter_view` | walk |
| Speaker clock, pause, reset | **missing: a view cannot read the clock; needs a `ui.Time` elapsed form or a live duration** | walk |
| Blank to black/white, cross-fade between slides, reduced motion | blanking is view state; **cross-fade needs a motion primitive on the stage** | walk |
| Keyboard shortcuts (N, P, G, T/R/O/L, arrows, Tab, Enter, Delete, Ctrl+Z/Y/D/C/V, present keys) | action `key:` metadata and present buttons; **single-letter editor keys conflict with typing and need the stage component's key routing** | walk |
| Context menus (element, slide) | inspector arrange buttons; **right-press menus on a pin need a `ui.ContextMenu` route** | walk |
| Help overlay (`?`) | **missing: needs a shared keyboard-reference sheet** | walk |
| Search over slides, elements, notes | the workspace's index over `Slide` and `Element` text | search walk |
| Accessibility (semantic tree, actor labels, slide window) | described components carry names and roles | a11y walk |

## What blocks the port

A slide is a canvas: views placed at slide coordinates, styled with a deck palette's roles, drawn
as text and shapes with arbitrary fills, and selected, dragged and resized. The toolkit today
places views only in rows, columns and a nine-way layers overlay; `ui.Map` gives positions but
draws nothing itself and `ui.Drawing` draws only theme inks with no rich text. Until one shared
component does that (and can draw a slide into a picture, for the rail and the overview grid), the
Rust app's canvas, rail thumbnails and overview cannot be replaced without losing what a person
sees.

The fullscreen component and a live elapsed time are smaller shared pieces the app also needs.
