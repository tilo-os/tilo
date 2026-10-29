//! What a part of a view publishes beside its resource that no binding
//! covers: the live entities it shows ([`LiveRow`]s), search entries for
//! what the resource does not hold, whether the secrets behind them are
//! unlocked, and what it looks up for the platform search's queries. A view says it with
//! [`ViewCx::publishing`](super::ViewCx::publishing); the root collects it as
//! it collects a hosted surface's, so a view never needs a widget of its own
//! to publish what it shows.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

use tilo_resource::{Resource, SearchEntry};
use ui_core::search::SearchQuery;
use ui_core::widget::{AppModel, Decorator, Element, ModelPath, Widget};

use super::effect::EffectStatus;
use super::rows::LiveRow;
use super::surface::{SearchQueries, SurfacePublication};

type Rows = Rc<dyn Fn() -> Vec<LiveRow>>;
type Row = Rc<dyn Fn(&str) -> Option<LiveRow>>;
type Entries = Rc<dyn Fn() -> Vec<SearchEntry>>;
type Unlocked = Rc<dyn Fn() -> Option<bool>>;
type OnQuery = Rc<RefCell<dyn FnMut(&SearchQuery)>>;
type Answering = Rc<dyn Fn() -> bool>;

/// What a part of a view publishes beside the resource. Each source is read
/// whenever the resource is published or searched, so it reads the view's
/// [`LocalState`](ui_core::LocalState)s and settings as they are then.
pub struct Publishes<R: Resource> {
    sources: Sources,
    resource: PhantomData<fn() -> R>,
}

/// The sources of a [`Publishes`], without its resource type, so the widget
/// that publishes them is compiled once for every app.
#[derive(Clone, Default)]
struct Sources {
    rows: Option<Rows>,
    row: Option<Row>,
    entries: Option<Entries>,
    unlocked: Option<Unlocked>,
    query: Option<OnQuery>,
    answering: Option<Answering>,
}

impl<R: Resource> Default for Publishes<R> {
    fn default() -> Self {
        Self {
            sources: Sources::default(),
            resource: PhantomData,
        }
    }
}

impl<R: Resource> Publishes<R> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The live external entities the view shows, published as children of
    /// the resource and merged by id with its document children. Keep it to
    /// what fits a published tree (the rows on screen, a bounded page) and
    /// resolve the rest with [`Self::live_row`].
    #[must_use]
    pub fn live_rows(mut self, rows: impl Fn() -> Vec<LiveRow> + 'static) -> Self {
        self.sources.rows = Some(Rc::new(rows));
        self
    }

    /// The live row with an id [`Self::live_rows`] leaves out, so an action
    /// addressed to any entity the view knows still reaches it.
    #[must_use]
    pub fn live_row(mut self, row: impl Fn(&str) -> Option<LiveRow> + 'static) -> Self {
        self.sources.row = Some(Rc::new(row));
        self
    }

    /// Search entries for what the view shows that the resource does not
    /// hold (a vault's items, a page's text), indexed with the resource's
    /// own as the app's one search source. Read whenever someone searches,
    /// so keep it to what the view already has at hand.
    #[must_use]
    pub fn search_entries(mut self, entries: impl Fn() -> Vec<SearchEntry> + 'static) -> Self {
        self.sources.entries = Some(Rc::new(entries));
        self
    }

    /// Whether the secrets behind the view's secret-bearing search entries
    /// ([`SearchEntry::secret_bearing`]) are unlocked now: a vault that
    /// opened and found its key. The system's search index finds those
    /// entries only while the app last said `Some(true)`; `None` says
    /// nothing.
    #[must_use]
    pub fn secrets_unlocked(mut self, unlocked: impl Fn() -> Option<bool> + 'static) -> Self {
        self.sources.unlocked = Some(Rc::new(unlocked));
        self
    }

    /// Look up what the platform search asks for beyond what the view holds
    /// (a server's catalogue, a mailbox not yet fetched), and offer what
    /// comes back through [`Self::search_entries`]. `query` gets the latest
    /// query on the UI thread before the view syncs; queries asked between
    /// frames are coalesced. Never asked of a past version.
    #[must_use]
    pub fn search_query(mut self, query: impl FnMut(&SearchQuery) + 'static) -> Self {
        self.sources.query = Some(Rc::new(RefCell::new(query)));
        self
    }

    /// Whether the results for the latest [`Self::search_query`] are still
    /// coming (a server is still looking). A search already showing waits
    /// for them and shows them once this turns false, without anyone typing
    /// again. Read after every sync.
    #[must_use]
    pub fn search_pending(mut self, pending: impl Fn() -> bool + 'static) -> Self {
        self.sources.answering = Some(Rc::new(pending));
        self
    }
}

/// `content`, publishing what `publishes` says on the resource `R`. `wake`
/// is the live window's, `None` in a past version, which is asked nothing.
pub(crate) fn publishing<R: Resource>(
    publishes: Publishes<R>,
    content: Element,
    wake: Option<ui_core::WakeHandle>,
) -> Element {
    element(
        std::any::TypeId::of::<R>(),
        publishes.sources,
        content,
        wake,
    )
}

#[inline(never)]
fn element(
    resource: std::any::TypeId,
    sources: Sources,
    content: Element,
    wake: Option<ui_core::WakeHandle>,
) -> Element {
    // Only a view that looks queries up is asked them.
    let queries = SearchQueries::waking(wake.filter(|_| sources.query.is_some()));
    Box::new(Publishing {
        resource,
        sources,
        content,
        queries,
    })
}

struct Publishing {
    resource: std::any::TypeId,
    sources: Sources,
    content: Element,
    queries: SearchQueries,
}

impl Decorator for Publishing {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn provide<'a>(&'a self, request: &mut ui_core::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn SurfacePublication>() {
            request.provide::<dyn SurfacePublication>(self);
            return;
        }
        self.content.provide(request);
    }

    fn provide_mut<'a>(
        &'a mut self,
        request: &mut ui_core::widget::capability::RequestMut<'a, '_>,
    ) {
        if request.wants::<dyn SurfacePublication>() {
            request.provide::<dyn SurfacePublication>(self);
            return;
        }
        self.content.provide_mut(request);
    }

    // Whoever reaches the view's widget by its type (a test, a host that
    // embeds it) finds it through the part that publishes beside it.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        self.content.as_any()
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        self.content.as_any_mut()
    }

    fn name(&self) -> &'static str {
        "Publishing"
    }

    // The latest query reaches the view before it syncs, as a surface's
    // does; what it says of its answer is read after.
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if let Some(query) = self.queries.take()
            && let Some(on_query) = &self.sources.query
        {
            (on_query.borrow_mut())(&query);
        }
        self.content.sync(state, path);
        self.queries.settle(
            self.sources
                .answering
                .as_ref()
                .is_some_and(|answering| answering()),
        );
    }
}

impl SurfacePublication for Publishing {
    fn resource(&self) -> std::any::TypeId {
        self.resource
    }
    fn bound_declarations(&self, _into: &mut dyn std::any::Any) {}
    fn availability(&self) -> Vec<EffectStatus> {
        Vec::new()
    }
    fn live_rows(&self) -> Vec<LiveRow> {
        self.sources
            .rows
            .as_ref()
            .map_or_else(Vec::new, |rows| rows())
    }
    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.sources.row.as_ref().and_then(|row| row(id))
    }
    fn live_row_pending(&self, _id: &str) -> bool {
        false
    }
    fn binds_rows(&self) -> bool {
        false
    }
    fn offers_on_row(&self, _row: &str) -> bool {
        false
    }
    fn row_declarations(&self, _row: &str, _into: &mut dyn std::any::Any) -> bool {
        false
    }
    fn search_entries(&self) -> Vec<SearchEntry> {
        self.sources
            .entries
            .as_ref()
            .map_or_else(Vec::new, |entries| entries())
    }
    fn secrets_unlocked(&self) -> Option<bool> {
        self.sources
            .unlocked
            .as_ref()
            .and_then(|unlocked| unlocked())
    }
    fn kept_in_search_index(&self) -> bool {
        true
    }
    fn supports_device_shell(&self) -> bool {
        false
    }
    fn device_shell_operation(
        &mut self,
        _operation: &ui_core::floating_surface::DeviceShellOperation,
    ) -> Result<serde_json::Value, String> {
        Err("this view has no device shell".into())
    }
    fn request_device_shell(&mut self) -> bool {
        false
    }
    fn search_queries(&self) -> SearchQueries {
        self.queries.clone()
    }
    fn take_host_intent(&mut self) -> Option<ui_core::widget::AppHostIntent> {
        None
    }
    fn take_save_retry(&mut self) -> bool {
        false
    }
}
