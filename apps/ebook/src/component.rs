//! Books' checked application entry point, shared by native and browser hosts.
use automerge::AutoCommit;
use ui::application::{AppComponent, ApplicationDefinition, HostContext, LaunchTarget};
use ui::{launch::LaunchError, record::RecordKind, widget::IconDescriptor};

/// Whether the host has neither files nor a picker, so the one book there is
/// to read is the one compiled into the app: the browser.
const BUNDLED_LIBRARY: bool = cfg!(target_arch = "wasm32");

fn books_definition(create: ui::application::ComponentFactory) -> ApplicationDefinition {
    ApplicationDefinition {
        app_id: "so.tilo.ebook",
        title: "Books",
        icon: IconDescriptor::Named("ebook".into()),
        record_kind: RecordKind::new("ebook.library"),
        required: vec![],
        optional: vec![],
        create,
    }
}

static DEFINITION: std::sync::LazyLock<ApplicationDefinition> = std::sync::LazyLock::new(|| {
    books_definition(|context, target, _| create(context, &target, BUNDLED_LIBRARY))
});
pub fn definition() -> &'static ApplicationDefinition {
    &DEFINITION
}

/// The browser's definition, for exercising its bundled library natively.
#[cfg(test)]
pub(crate) fn bundled_library_definition() -> &'static ApplicationDefinition {
    static DEFINITION: std::sync::LazyLock<ApplicationDefinition> =
        std::sync::LazyLock::new(|| {
            books_definition(|context, target, _| create(context, &target, true))
        });
    &DEFINITION
}

fn create(
    mut context: HostContext,
    target: &LaunchTarget,
    bundled: bool,
) -> Result<AppComponent, LaunchError> {
    if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_record(|doc| initialize_library(doc, bundled))
            .map_err(LaunchError::Record)?;
    }
    let mut root = crate::BooksApp::new();
    if bundled {
        let bundled_id = crate::content::bundled_book_id();
        let shelved = context
            .read_record(|doc| {
                crate::doc::BooksDoc::read_library_entries(doc)
                    .iter()
                    .any(|entry| entry.id == bundled_id)
            })
            .map_err(LaunchError::Record)?;
        if shelved {
            root.open_book_on_load(&bundled_id);
        }
    }
    let mut registry = tilo_actions::ActionRegistry::new();
    crate::actions::register(&mut registry);
    Ok(AppComponent::new(context, Box::new(root), registry))
}

/// A new library is empty; with a bundled library it holds the bundled book.
fn initialize_library(doc: &mut AutoCommit, bundled: bool) -> Result<(), String> {
    if bundled {
        crate::content::record_bundled_book(doc).ok_or("the bundled book does not parse")?;
    }
    Ok(())
}

impl ui::widget::AppWidget for crate::BooksApp {
    fn app_kind(&self) -> ui::widget::AppKind {
        // The kind vocabulary has no reader; a book is a document the
        // reader annotates, which `Editor` names.
        ui::widget::AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Books"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Named("ebook".into())
    }
    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        None
    }
}

#[cfg(test)]
#[path = "component_tests.rs"]
mod tests;
