// One test binary instead of one per file; #[path] keeps the files in place.
#[path = "support/mod.rs"]
mod support;

#[path = "agent_surface.rs"]
mod agent_surface;
#[path = "answers.rs"]
mod answers;
#[path = "app.rs"]
mod app;
#[path = "brushes.rs"]
mod brushes;
#[path = "crdt_fields.rs"]
mod crdt_fields;
#[path = "document.rs"]
mod document;
#[path = "drawer.rs"]
mod drawer;
#[path = "finish.rs"]
mod finish;
#[path = "history.rs"]
mod history;
#[path = "interact.rs"]
mod interact;
#[path = "layers_tree.rs"]
mod layers_tree;
#[path = "look.rs"]
mod look;
#[path = "menus.rs"]
mod menus;
#[path = "picture.rs"]
mod picture;
#[path = "portable.rs"]
mod portable;
#[path = "responsive.rs"]
mod responsive;
#[path = "responsiveness.rs"]
mod responsiveness;
#[path = "selection.rs"]
mod selection;
#[path = "subject.rs"]
mod subject;
#[path = "subject_off_thread.rs"]
mod subject_off_thread;
#[path = "svg.rs"]
mod svg;
#[path = "vocabulary.rs"]
mod vocabulary;
