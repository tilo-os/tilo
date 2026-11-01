//! The places list's controls, bound to what the Files window publishes: a
//! press, a drag, a screen reader, the palette and an agent reach the same
//! effect under the same contract. The favorites are this device's, so both
//! are effects; neither writes the document.

use std::sync::LazyLock;

use ui::tilo_actions::{ActionBinding, ResourceSpec};
use ui::tilo_resource::{AbsolutePath, Effect, Resource};
use ui::widget::NavDrop;

use crate::effects::FilesEffect;
use crate::places_sidebar::path_of_id;
use crate::resource::FileExplorer;

/// The window as the palette, an agent and a control see it: what the
/// resource publishes, with its effects beside it.
static WINDOW: LazyLock<ResourceSpec> = LazyLock::new(|| {
    let mut window = ui::tilo_resource::publish::<FileExplorer>(
        FileExplorer::KIND,
        FileExplorer::LABEL,
        &FileExplorer::default(),
    );
    window.actions.extend(FilesEffect::specs());
    window
});

/// The folder a places row names, as the absolute path an effect takes.
fn place(row: &str) -> Result<AbsolutePath, String> {
    let path = path_of_id(row).ok_or_else(|| format!("`{row}` names no folder"))?;
    AbsolutePath::new(path).map_err(|error| error.to_string())
}

/// A favorite's row action: unpins the folder the row names.
pub(crate) fn remove_favorite() -> Option<ActionBinding<String>> {
    let spec = WINDOW
        .actions
        .iter()
        .find(|spec| spec.id == FilesEffect::REMOVE_FAVORITE.id())?;
    ActionBinding::fallible(&WINDOW, spec, |row: String| {
        Ok(FilesEffect::RemoveFavorite { path: place(&row)? }.arguments())
    })
    .ok()
}

/// Dragging one favorite onto another: moves the dragged one to where it
/// landed, before the row it was dropped on or after it.
pub(crate) fn move_favorite() -> Option<ActionBinding<(String, NavDrop)>> {
    let spec = WINDOW
        .actions
        .iter()
        .find(|spec| spec.id == FilesEffect::MOVE_FAVORITE.id())?;
    ActionBinding::fallible(&WINDOW, spec, |(row, drop): (String, NavDrop)| {
        let (before, beside) = match drop {
            NavDrop::Before(beside) => (true, beside),
            NavDrop::After(beside) => (false, beside),
            NavDrop::Into(_) => {
                return Err("a favorite lands beside another, never inside it".to_owned());
            }
        };
        Ok(FilesEffect::MoveFavorite {
            source: place(&row)?,
            target: place(&beside)?,
            before,
        }
        .arguments())
    })
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_actions::OperationKind;

    /// The places list's controls are bound to effects the window publishes,
    /// and the move names the folder it was dropped beside and its side.
    #[test]
    fn place_controls_bind_published_effects() {
        let remove = remove_favorite().expect("the window publishes remove-favorite");
        assert_eq!(remove.address().action.as_str(), "remove-favorite");
        assert_eq!(remove.address().resource.as_str(), FileExplorer::KIND);
        let move_favorite = move_favorite().expect("the window publishes move-favorite");
        assert_eq!(move_favorite.address().action.as_str(), "move-favorite");
        for id in ["remove-favorite", "move-favorite"] {
            let spec = WINDOW
                .actions
                .iter()
                .find(|spec| spec.id == id)
                .unwrap_or_else(|| panic!("`{id}` is published"));
            assert_ne!(spec.kind, OperationKind::Document, "{id}");
        }
        assert!(place("fav:/home/u/Pictures").is_ok(), "a favorite's row");
        assert!(place("loc:/").is_ok(), "a location's row");
        assert!(place("files.entry:42").is_err(), "an entry names no folder");    }
}
