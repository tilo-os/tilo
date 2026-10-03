//! Books' checked application entry point, shared by native and browser hosts.
use automerge::AutoCommit;
use ui::application::{AppComponent, ApplicationDefinition, HostContext, LaunchTarget};
use ui::{launch::LaunchError, record::RecordKind, widget::IconDescriptor};

/// Whether the host has no native files, so the one book there is to read is
/// the one compiled into the app: the browser.
fn bundled_library() -> bool {
    !matches!(
        tilo_files::availability(tilo_files::Feature::NativePaths),
        tilo_files::Availability::Available
    )
}

/// What Books opens when a host hands it a file.
pub const EPUB_MEDIA_TYPE: &str = "application/epub+zip";

/// The resource a book is addressed by in Books' action surface.
pub fn book_resource(book_id: &str) -> String {
    format!("ebook.book:{book_id}")
}

fn books_definition(create: ui::application::ComponentFactory) -> ApplicationDefinition {
    ApplicationDefinition {
        app_id: "so.tilo.ebook",
        title: "Books",
        icon: IconDescriptor::Named("ebook".into()),
        record_kind: RecordKind::new("ebook.library"),
        required: vec![],
        optional: vec![],
        opens: &[EPUB_MEDIA_TYPE],
        create,
    }
}

static DEFINITION: std::sync::LazyLock<ApplicationDefinition> = std::sync::LazyLock::new(|| {
    books_definition(|context, target, _| create(context, &target, bundled_library()))
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
    let store = context.content().cloned();
    let handed = context
        .take_open_file()
        .map(|file| {
            let handed = crate::content::open_handed_book(file.bytes(), store.as_ref()).map_err(
                |error| LaunchError::Operation(format!("Couldn’t open {}: {error}", file.name())),
            )?;
            file.report_opened(book_resource(&handed.book.id));
            Ok::<_, LaunchError>(handed)
        })
        .transpose()?;
    if matches!(target, LaunchTarget::New) {
        // A library opened for a handed-over book holds that book, not the sample.
        let sample = bundled && handed.is_none();
        context
            .initialize_new_record(|doc| initialize_library(doc, sample))
            .map_err(LaunchError::Record)?;
    }
    let mut root = crate::BooksApp::new();
    if let Some(store) = store {
        root.set_content_store(store);
    }
    if let Some(handed) = handed {
        root.open_handed_on_load(handed);
    } else if bundled {
        let recent = context
            .read_record(most_recently_opened)
            .map_err(LaunchError::Record)?;
        if let Some(book_id) = recent {
            root.open_book_on_load(&book_id);
        }
    }
    let mut registry = tilo_actions::ActionRegistry::new();
    crate::actions::register(&mut registry);
    Ok(AppComponent::new(context, Box::new(root), registry))
}

/// The shelved book last opened (the latest added when none was), which a
/// host without files shows again, so another mount of the library reads
/// the same book.
fn most_recently_opened(doc: &AutoCommit) -> Option<String> {
    crate::doc::BooksDoc::read_library_entries(doc)
        .into_iter()
        .enumerate()
        .max_by_key(|(order, entry)| {
            (
                crate::doc::BooksDoc::read_progress(doc, &entry.id).opened,
                *order,
            )
        })
        .map(|(_, entry)| entry.id)
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
