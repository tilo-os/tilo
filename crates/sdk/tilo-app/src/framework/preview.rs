//! History mode for every framework app: past versions rendered by the app's
//! own view, read-only.
//!
//! A view is a function of the resource state, so a historical version needs
//! no separate renderer: the framework reads the resource from the frozen
//! document, builds the view again with editing disabled, and lays it out in
//! History mode's preview pane. Restoring a version is equally generic: the
//! store's `restore` verb, checked against what the resource reads.

use std::any::TypeId;

use tilo_resource::{Reader, Resource};
use ui_core::automerge::AutoCommit;
use ui_core::history_mode::{
    FrozenRootPreview, HistoricalGap, HistoricalSupport, HistoryDefinition, HistoryPresentation,
    HistoryPreview, HistoryScopeDefinition, PartVersion, PreviewInput, PreviewView,
};
use ui_core::vello::kurbo::Rect;

use super::app::{Editing, ResourceApp, build_root, leaked};
use super::live::Live;
use super::root::ResourceRoot;
use ui_core::storage::AppStorage;

struct HistoryKey<A>(std::marker::PhantomData<A>);

struct FolderObservationKey<A>(std::marker::PhantomData<A>);

/// The folder scope of `A`'s History alone: what its root declares it owns, so
/// the runner can tell the folder source it was handed is the root's.
pub(crate) fn folder_history_observation<A: ResourceApp>() -> &'static HistoryDefinition {
    // The definition table is not reentrant: read the whole definition first.
    let definition = history_definition::<A>();
    leaked(TypeId::of::<FolderObservationKey<A>>(), || {
        folder_observation(definition)
    })
}

/// [`folder_history_observation`] of the app whose History is `definition`. Not
/// generic, so it is compiled once rather than for every app.
#[inline(never)]
fn folder_observation(definition: &HistoryDefinition) -> HistoryDefinition {
    let scopes: &'static [HistoryScopeDefinition] = Box::leak(
        definition
            .scopes
            .iter()
            .filter(|scope| scope.key == FOLDER_SCOPE)
            .map(|scope| HistoryScopeDefinition {
                key: scope.key,
                title: scope.title,
                presentation: HistoryPresentation::Folder,
            })
            .collect::<Box<[_]>>(),
    );
    HistoryDefinition {
        app_id: definition.app_id,
        scopes,
    }
}

/// The History scope of the folder an app of
/// [`ResourceApp::FOLDER_HISTORY`](super::ResourceApp::FOLDER_HISTORY) shows.
pub(crate) const FOLDER_SCOPE: &str = "folder";

/// The History mode registration of `A`: one scope, previewed by its view.
pub(crate) fn history_definition<A: ResourceApp>() -> &'static HistoryDefinition {
    leaked(TypeId::of::<HistoryKey<A>>(), || {
        history_definition_of(
            super::app::hooks::<A>().id(),
            A::FOLDER_HISTORY,
            (<A::Resource as Resource>::LABEL, preview::<A>),
        )
    })
}

/// [`history_definition`] of the app `app_id`: its resource's scope, called
/// `label` and previewed by `create`, and the scope of `folder` when it shows
/// the workspace's files. Not generic, so it is compiled once rather than for every app.
#[inline(never)]
fn history_definition_of(
    app_id: &'static str,
    folder: Option<super::app::FolderHistory>,
    (label, create): (&'static str, ui_core::history_mode::PreviewFactory),
) -> HistoryDefinition {
    // A window that shows the workspace's folder: its History lists the folder's changes,
    // or the shown file's; History moves to it when the app opens a file's.
    let folder_scope = folder.map(|folder| HistoryScopeDefinition {
        key: FOLDER_SCOPE,
        title: folder.title,
        presentation: HistoryPresentation::Folder,
    });
    let resource = HistoryScopeDefinition {
        key: "resource",
        title: label,
        presentation: HistoryPresentation::Domain { create },
    };
    // A window whose record keeps only which folder is open lists the folder alone.
    let resource = folder
        .is_none_or(|folder| folder.beside_resource)
        .then_some(resource);
    let scopes: &'static [HistoryScopeDefinition] = Box::leak(
        resource
            .into_iter()
            .chain(folder_scope)
            .collect::<Box<[_]>>(),
    );
    HistoryDefinition { app_id, scopes }
}

/// What History can do with `R`'s versions, from what the type declared:
/// a read adapter, or a stored record its `read` reads field by field.
#[must_use]
pub(crate) fn historical_support<R: Resource>() -> HistoricalSupport {
    HistoricalSupport::of(
        R::READABLE
            || matches!(R::shape(), Some(tilo_resource::Shape::Record(fields)) if !fields.is_empty()),
    )
}

/// One version of `R` as the read API answers it: the type's own `read`
/// adapter over the frozen document, exactly as it answers the live record —
/// its semantic content, never the raw record.
///
/// # Errors
/// A [`HistoricalGap`] to show: the version does not read as the type, or the
/// type has no read adapter.
#[inline(never)]
pub(crate) fn historical_content<R: Resource>(
    frozen: &AutoCommit,
    item: Option<&str>,
) -> Result<serde_json::Value, HistoricalGap> {
    if historical_support::<R>() == HistoricalSupport::Unsupported {
        return Err(HistoricalGap::unsupported(format!(
            "a {} shows no history of its content",
            R::LABEL
        )));
    }
    // The version must read as the app's type at all: one written by a
    // newer app is incompatible, which is not the same as empty.
    let state = R::read(frozen).map_err(|error| HistoricalGap::unavailable(error.to_string()))?;
    // The read API's answer states the type's content as its `content`
    // beside the version's heads, so the adapter's own JSON goes there
    // unwrapped.
    R::read_content(&state, frozen, item).map_err(HistoricalGap::unavailable)
}

fn preview<A: ResourceApp>(input: PreviewInput) -> Result<Box<dyn HistoryPreview>, String> {
    preview_of(input, <A::Resource as Resource>::LABEL, &past_root::<A>)
}

/// [`preview`] of the app whose root over a past version `root` builds;
/// `label` is its resource's [`Resource::LABEL`]. Not generic, so loading
/// the version is compiled once rather than for every app.
#[inline(never)]
fn preview_of(
    input: PreviewInput,
    label: &'static str,
    root: &dyn Fn(&AutoCommit) -> Result<ResourceRoot, String>,
) -> Result<Box<dyn HistoryPreview>, String> {
    let doc = AutoCommit::load(input.snapshot.bytes()).map_err(|error| error.to_string())?;
    let root = root(&doc)?;
    Ok(open_preview(root, doc, label, input.initial_view))
}

/// [`open_preview`] of a copy of `doc`. Not generic, so the generic code
/// that calls it never holds a document, and a document's teardown is
/// compiled once rather than in every app.
#[inline(never)]
fn open_copy(
    root: ResourceRoot,
    doc: &AutoCommit,
    label: &'static str,
    view: PreviewView,
) -> Box<dyn HistoryPreview> {
    open_preview(root, doc.clone(), label, view)
}

/// History's preview of `root` drawing `doc`, opened at `view`. Not generic
/// over the app, so it and the preview's table are compiled once.
fn open_preview(
    root: ResourceRoot,
    doc: AutoCommit,
    label: &'static str,
    view: PreviewView,
) -> Box<dyn HistoryPreview> {
    Box::new(past_preview(root, doc, label).with_view(view))
}

/// `root` drawing `doc`, the past version it was built over; `label` is
/// the shown resource's [`Resource::LABEL`].
fn past_preview(root: ResourceRoot, doc: AutoCommit, label: &'static str) -> FrozenRootPreview {
    FrozenRootPreview::new(root.into_root_widget(), doc, label)
}

/// `A`'s root over `doc`, a frozen past version: read-only, keeping
/// nothing of the person's.
fn past_root<A: ResourceApp>(doc: &AutoCommit) -> Result<ResourceRoot, String> {
    let reader = Reader::<A::Resource>::read(doc).map_err(|error| error.to_string())?;
    Ok(build_root::<A>(
        reader,
        Editing::ReadOnly(Box::new(super::scenario::Run::past())),
        AppStorage::for_app(super::app::hooks::<A>().id()),
        super::settings::Hosted::default(),
    ))
}

/// What draws a part's version in a window of its app ([`part_preview`]).
pub(crate) type PartPreview = fn(
    super::chain::PastPart,
    Option<ui_core::resource::DocumentId>,
    PartVersion<'_>,
) -> Result<Box<dyn HistoryPreview>, String>;

/// `A`'s window over its document as it is now, its chain's parts read from `past`, the item the
/// selected changes changed shown: what History shows of a version of a part. `record` is the
/// window's record, beside which the chain may be kept.
pub(crate) fn part_preview<A: ResourceApp>(
    past: super::chain::PastPart,
    record: Option<ui_core::resource::DocumentId>,
    version: PartVersion<'_>,
) -> Result<Box<dyn HistoryPreview>, String> {
    let reader = Reader::<A::Resource>::read(version.own).map_err(|error| error.to_string())?;
    let run = super::scenario::Run::past().launched(super::scenario::Launched {
        record,
        reveal: std::cell::RefCell::new(past.changed),
        ..super::scenario::Launched::default()
    });
    let root = build_root::<A>(
        reader,
        Editing::ReadOnly(Box::new(run)),
        AppStorage::for_app(super::app::hooks::<A>().id()),
        super::settings::Hosted {
            records: Some((past.store, past.me, None)),
            ..super::settings::Hosted::default()
        },
    );
    Ok(open_copy(
        root,
        version.own,
        <A::Resource as Resource>::LABEL,
        version.view,
    ))
}

/// A past version drawn by the app's own view, exactly as History mode's
/// preview draws it: read-only, over the frozen document. Built by
/// [`HeadlessApp::past_version`](super::testing::HeadlessApp::past_version).
pub struct PastVersion<R: Resource> {
    preview: FrozenRootPreview,
    /// The root's live state, which the root widget holds erased.
    live: Live<R>,
}

impl<R: Resource> PastVersion<R> {
    /// Every name and value the drawn view exposes to assistive technology,
    /// depth first: what a reader of the past version would hear.
    pub fn texts(&self) -> Vec<String> {
        ui_core::a11y::texts(self.preview.root())
    }

    /// The state the view was presented.
    pub fn state(&self) -> std::rc::Rc<R::State> {
        self.live.get()
    }

    /// The drawn size of the view.
    pub fn size(&self) -> Rect {
        self.preview.root().get_rect()
    }
}

/// Lay out and draw `A`'s view over `doc`, a frozen past version, in a
/// `size` pane.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn render_past<A: ResourceApp>(
    doc: AutoCommit,
    size: (u32, u32),
) -> Result<PastVersion<A::Resource>, String> {
    let root = past_root::<A>(&doc)?;
    let live = root
        .typed::<A::Resource>()
        .map(|typed| typed.live.clone())
        .ok_or_else(|| format!("{} drew another resource", super::app::hooks::<A>().title()))?;
    let mut preview = past_preview(root, doc, <A::Resource as Resource>::LABEL);
    let theme = ui_core::Theme::dark();
    let mut text = ui_core::text_render::TextRenderer::new();
    let bounds = Rect::new(0.0, 0.0, f64::from(size.0), f64::from(size.1));
    preview.layout(bounds, &mut text, &theme)?;
    let mut scene = ui_core::vello::Scene::new();
    preview.draw(
        &mut scene,
        ui_core::vello::kurbo::Affine::IDENTITY,
        &mut text,
        &theme,
    );
    Ok(PastVersion { preview, live })
}

/// What builds a past version's root over its document, or says why it cannot.
#[cfg(not(target_family = "wasm"))]
type RootBuilder<'a> = dyn Fn(&AutoCommit) -> Result<ResourceRoot, String> + 'a;

/// Lay out `A`'s view over `doc`, a frozen past version, in `bounds` and
/// draw it: what `render` answers for a version.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn draw_past<A: ResourceApp>(
    doc: AutoCommit,
    bounds: Rect,
    scene: &mut ui_core::vello::Scene,
    transform: ui_core::vello::kurbo::Affine,
    theme: &ui_core::Theme,
) -> Result<(), String> {
    draw_over(
        doc,
        (<A::Resource as Resource>::LABEL, &past_root::<A>),
        bounds,
        scene,
        transform,
        theme,
    )
}

/// [`draw_past`] of the app whose root over `doc` the second of `app`
/// builds, its resource labelled the first. Not generic, so the generic code
/// that calls it never holds a document, and the drawing and a document's
/// teardown are compiled once rather than in every app.
#[cfg(not(target_family = "wasm"))]
#[inline(never)]
fn draw_over(
    doc: AutoCommit,
    (label, root): (&'static str, &RootBuilder<'_>),
    bounds: Rect,
    scene: &mut ui_core::vello::Scene,
    transform: ui_core::vello::kurbo::Affine,
    theme: &ui_core::Theme,
) -> Result<(), String> {
    let root = root(&doc)?;
    let mut preview = past_preview(root, doc, label);
    let mut text = ui_core::text_render::TextRenderer::new();
    preview.layout(bounds, &mut text, theme)?;
    preview.draw(scene, transform, &mut text, theme);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use tilo_resource::AppNamed;
    use ui_core::Theme;
    use ui_core::text_render::TextRenderer;
    use ui_core::vello::Scene;
    use ui_core::vello::kurbo::Affine;

    #[derive(Model, Default)]
    pub struct Page {
        pub heading: String,
    }

    #[resource(kind = "test.page", catalogue = "tests/locales")]
    impl Page {
        /// Set the heading.
        #[action]
        pub fn set_heading(&mut self, heading: String) {
            self.heading = heading;
        }
    }

    struct PageApp;

    impl ResourceApp for PageApp {
        type Resource = Page;
        const ID: &'static str = "so.tilo.test-page";
        const TITLE: &'static str = "Page";

        fn view(cx: &mut ViewCx<Page>) -> Element {
            Box::new(Column::new().push(Box::new(
                cx.text(|page| page.heading.clone()).with_id("heading"),
            )))
        }
    }

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Shelf {
        pub title: String,
        pub books: Collection<Book>,
        pub note: Option<String>,
    }

    #[derive(Model, Default, Debug, Clone, PartialEq)]
    pub struct Book {
        pub name: String,
        pub read: bool,
    }

    #[resource(kind = "test.shelf", catalogue = "tests/locales")]
    impl Shelf {
        /// Rename the shelf.
        #[action]
        pub fn rename(&mut self, title: String) {
            self.title = title;
        }

        /// Add a book.
        #[action]
        pub fn add(&mut self, cx: &mut ActionCx, name: String) {
            self.books.push(cx.id(), Book { name, read: false });
        }

        /// Mark a book read.
        #[action]
        pub fn finish(&mut self, book: Id) {
            if let Some(book) = self.books.get_mut(&book) {
                book.read = true;
            }
        }

        /// Remove a book.
        #[action]
        pub fn remove(&mut self, book: Id) {
            self.books.remove(&book);
        }

        /// Annotate the shelf.
        #[action]
        pub fn annotate(&mut self, note: Option<String>) {
            self.note = note;
        }
    }

    /// Bring `replica` back to the version `target` through the store's `restore` verb, as
    /// `Shelf`'s history decodes it.
    fn restore(
        replica: &mut Replica<Shelf, AppNamed>,
        target: &[ui_core::automerge::ChangeHash],
    ) -> Result<(), tilo_actions::ActionError> {
        use super::super::history::{HistoryEdit, ResourceHistoryEdit, version_name};
        let edit = HistoryEdit::Restore {
            version: version_name(target),
            ids: None,
            step: None,
        };
        let encoded = tilo_actions::EncodedAction {
            type_id: super::super::history::EDIT_TYPE.to_owned(),
            payload: super::super::history::edit_payload(&edit).unwrap(),
        };
        let mut doc = replica.doc().clone();
        tilo_actions::apply_local(&mut doc, &ResourceHistoryEdit::<Shelf>::new(edit), encoded)?;
        *replica = Replica::from_doc(doc).in_app("so.tilo.model", "Model");
        Ok(())
    }

    tilo_test_env::scenario! {
        fn past_versions_restore_by_record_shape_and_render_read_only;

        phase a_record_restores_a_past_version_field_by_field_and_item_by_item;
        {
            let mut shelf = Replica::<Shelf>::new().in_app("so.tilo.model", "Model");
            shelf.apply(Shelf::RENAME.with("Reading")).unwrap();
            shelf.apply(Shelf::ADD.with("Dune")).unwrap();
            shelf.apply(Shelf::ADD.with("Emma")).unwrap();
            let ids: Vec<Id> = shelf.state().unwrap().books.ids().cloned().collect();
            let past_heads = shelf.heads();
            let past = shelf.state().unwrap();

            shelf.apply(Shelf::RENAME.with("Done")).unwrap();
            shelf.apply(Shelf::FINISH.with(ids[0].clone())).unwrap();
            shelf.apply(Shelf::REMOVE.with(ids[1].clone())).unwrap();
            shelf.apply(Shelf::ADD.with("Ulysses")).unwrap();
            shelf
                .apply(Shelf::ANNOTATE.with(Some("tidy up".into())))
                .unwrap();
            assert_ne!(shelf.state().unwrap(), past);

            restore(&mut shelf, &past_heads).unwrap();
            assert_eq!(shelf.state().unwrap(), past);
            // Item identity survives: the restored books are the same items.
            assert_eq!(
                shelf
                    .state()
                    .unwrap()
                    .books
                    .ids()
                    .cloned()
                    .collect::<Vec<_>>(),
                ids
            );
        }

        phase a_version_comes_back_only_as_the_app_reads_it;
        {
            use ui_core::automerge::transaction::Transactable as _;
            // A record stored at a version this app does not read is not restored.
            let mut shelf = Replica::<Shelf>::new().in_app("so.tilo.model", "Model");
            shelf.apply(Shelf::RENAME.with("Reading")).unwrap();
            let past = shelf.heads();
            shelf.apply(Shelf::RENAME.with("Done")).unwrap();
            let mut doc = shelf.doc().clone();
            tilo_resource::evolution::write_version(&mut doc, Shelf::VERSION + 1).unwrap();
            doc.commit();
            let mut newer = Replica::<Shelf>::from_doc(doc).in_app("so.tilo.model", "Model");
            let refused = restore(&mut newer, &past).unwrap_err().to_string();
            assert!(refused.contains("stored at version"), "{refused}");

            // A version that no longer opens as the app's type is not restored.
            let mut shelf = Replica::<Shelf>::new().in_app("so.tilo.model", "Model");
            shelf.apply(Shelf::RENAME.with("Reading")).unwrap();
            let mut doc = shelf.doc().clone();
            doc.put_object(ui_core::automerge::ROOT, "title", ui_core::automerge::ObjType::List)
                .unwrap();
            doc.commit();
            let unreadable = doc.get_heads();
            doc.put(ui_core::automerge::ROOT, "title", "Fixed").unwrap();
            doc.commit();
            let mut shelf = Replica::<Shelf>::from_doc(doc).in_app("so.tilo.model", "Model");
            let refused = restore(&mut shelf, &unreadable).unwrap_err().to_string();
            assert!(refused.contains("no longer opens"), "{refused}");
            assert_eq!(shelf.state().unwrap().title, "Fixed");
            assert_eq!(historical_support::<Shelf>(), HistoricalSupport::Content);
        }

        phase a_past_version_renders_through_the_apps_own_view_read_only;
        {
            let mut page = Replica::<Page>::new().in_app("so.tilo.model", "Model");
            page.apply(Page::SET_HEADING.with("First draft")).unwrap();
            let past = page.heads();
            page.apply(Page::SET_HEADING.with("Final")).unwrap();
            let mut doc = page.doc().clone();
            let bytes = doc.fork_at(&past).unwrap().save();

            let frozen = AutoCommit::load(&bytes).unwrap();
            let root = build_root::<PageApp>(
                Reader::read(&frozen).unwrap(),
                Editing::ReadOnly(Box::new(crate::framework::scenario::Run::past())),
                AppStorage::for_app(PageApp::ID),
                crate::framework::settings::Hosted::default(),
            );
            let mut preview = past_preview(root, frozen, Page::LABEL);
            let theme = Theme::dark();
            let mut text = TextRenderer::new();
            preview
                .layout(Rect::new(10.0, 20.0, 410.0, 320.0), &mut text, &theme)
                .unwrap();
            let mut scene = Scene::new();
            preview.draw(&mut scene, Affine::IDENTITY, &mut text, &theme);
            assert_eq!(shown(preview.root()), ["First draft"]);
            assert!(preview.root().get_rect().width() > 0.0);
        }
    }

    fn shown(root: &dyn Widget) -> Vec<String> {
        fn visit(widget: &dyn Widget, out: &mut Vec<String>) {
            if let Some(node) = widget.accessibility()
                && let Some(label) = node.name
            {
                out.push(label);
            }
            widget.for_each_child(&mut |child| visit(child, out));
        }
        let mut out = Vec::new();
        visit(root, &mut out);
        out
    }
}
