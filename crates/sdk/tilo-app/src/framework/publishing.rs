//! What a part of a view publishes beside its resource that no binding
//! covers: the live entities it shows ([`LiveRow`]s) and the resource's
//! edits bound to them, which of the resource's actions its live state
//! makes unavailable, search entries for what the resource does not hold,
//! whether the secrets behind them are unlocked, what it looks up for the
//! platform search's queries, and what it asks of the host (a document to
//! open, the search to show). A view says it with
//! [`ViewCx::publishing`](super::ViewCx::publishing); the root collects it as
//! it collects a hosted surface's, so a view never needs a widget of its own
//! to publish what it shows.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;

use tilo_resource::{ActionDecl, Resource, SearchEntry};
use ui_core::search::SearchQuery;
use ui_core::widget::{AppHostIntent, AppModel, Decorator, Element, ModelPath, Widget};

use super::effect::EffectStatus;
use super::rows::LiveRow;
use super::surface::SurfacePublication;

type Rows = Rc<dyn Fn() -> Vec<LiveRow>>;
type Row = Rc<dyn Fn(&str) -> Option<LiveRow>>;
type Pending = Rc<dyn Fn(&str) -> bool>;
/// The edits bound to a row, put into the `Vec<ActionDecl<R>>` it is
/// handed (and nothing else); whether the row has any.
type RowEdits = Rc<dyn Fn(&str, &mut dyn std::any::Any) -> bool>;
type Statuses = Rc<dyn Fn() -> Vec<EffectStatus>>;
type Intents = Rc<RefCell<dyn FnMut() -> Option<AppHostIntent>>>;
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
    pending: Option<Pending>,
    row_edits: Option<RowEdits>,
    availability: Option<Statuses>,
    entries: Option<Entries>,
    /// Whether the system's search index forgets the entries once the app
    /// closes.
    transient: bool,
    unlocked: Option<Unlocked>,
    query: Option<OnQuery>,
    answering: Option<Answering>,
    intents: Option<Intents>,
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

    /// Whether the live row `id`, which [`Self::live_row`] does not know
    /// now, may be known once what the view is reading lands: an agent
    /// addressing it is answered then. Start that read here if it has not
    /// started; the window must be woken when it lands.
    #[must_use]
    pub fn live_row_pending(mut self, pending: impl Fn(&str) -> bool + 'static) -> Self {
        self.sources.pending = Some(Rc::new(pending));
        self
    }

    /// Edits of the resource bound to the live row `row` (rename *this*
    /// file, invite *this* person): typed builders of the resource's
    /// actions, published on that row and applied through the same
    /// permission check, confirmation and undo as the resource's own
    /// declarations. A row itself carries only view and effect operations;
    /// this is how it offers a document change.
    #[must_use]
    pub fn row_edits(mut self, edits: impl Fn(&str) -> Vec<ActionDecl<R>> + 'static) -> Self {
        self.sources.row_edits = Some(Rc::new(move |row, into| {
            let offered = edits(row);
            if offered.is_empty() {
                return false;
            }
            if let Some(into) = into.downcast_mut::<Vec<ActionDecl<R>>>() {
                into.extend(offered);
            }
            true
        }));
        self
    }

    /// The resource's own actions that differ from their declaration right
    /// now, as the view's live state says (a mode switch unavailable while
    /// recording, with why). Read whenever the resource is published, so
    /// the palette, agents and the door follow the view: an action
    /// unavailable here is refused with its reason.
    #[must_use]
    pub fn availability(mut self, statuses: impl Fn() -> Vec<EffectStatus> + 'static) -> Self {
        self.sources.availability = Some(Rc::new(statuses));
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

    /// Say that the system's search index forgets [`Self::search_entries`]
    /// once the app closes: entries that are the machine's state of the
    /// moment (the processes running) or a catalogue the system searches by
    /// itself. They are found only while the app runs, and a closed app
    /// leaves none behind to open into something that is gone.
    #[must_use]
    pub fn search_entries_while_running(mut self) -> Self {
        self.sources.transient = true;
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

    /// What the view asks of the host, one at a time, taken after every
    /// frame: a document chosen here to open in its own app, the platform
    /// search to show.
    #[must_use]
    pub fn host_intents(
        mut self,
        intents: impl FnMut() -> Option<AppHostIntent> + 'static,
    ) -> Self {
        self.sources.intents = Some(Rc::new(RefCell::new(intents)));
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

    // The latest query reaches the view before it syncs; what it says of
    // its answer is read after.
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
    fn availability(&self) -> Vec<EffectStatus> {
        self.sources
            .availability
            .as_ref()
            .map_or_else(Vec::new, |statuses| statuses())
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
    fn live_row_pending(&self, id: &str) -> bool {
        self.sources
            .pending
            .as_ref()
            .is_some_and(|pending| pending(id))
    }
    fn binds_rows(&self) -> bool {
        self.sources.row_edits.is_some()
    }
    fn offers_on_row(&self, row: &str) -> bool {
        // Asked with nothing to fill: whether the row has any edit at all.
        self.sources
            .row_edits
            .as_ref()
            .is_some_and(|edits| edits(row, &mut ()))
    }
    fn row_declarations(&self, row: &str, into: &mut dyn std::any::Any) -> bool {
        self.sources
            .row_edits
            .as_ref()
            .is_some_and(|edits| edits(row, into))
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
        !self.sources.transient
    }
    fn search_queries(&self) -> Option<SearchQueries> {
        Some(self.queries.clone())
    }
    fn take_host_intent(&mut self) -> Option<AppHostIntent> {
        self.sources
            .intents
            .as_ref()
            .and_then(|intents| (intents.borrow_mut())())
    }
}

/// Coalesced delivery of the platform search's queries to a view that
/// looks them up.
#[derive(Clone)]
pub(crate) struct SearchQueries(Rc<RefCell<QueryDelivery>>);

#[derive(Default)]
struct QueryDelivery {
    last: Option<SearchQuery>,
    pending: Option<SearchQuery>,
    /// The view said its results for `last` are still coming.
    awaiting: bool,
    /// Wakes the window, so a query asked between frames reaches the
    /// view without waiting for input. `None` where no query is ever
    /// delivered (History).
    wake: Option<ui_core::WakeHandle>,
}

impl SearchQueries {
    pub(crate) fn waking(wake: Option<ui_core::WakeHandle>) -> Self {
        Self(Rc::new(RefCell::new(QueryDelivery {
            wake,
            ..QueryDelivery::default()
        })))
    }

    /// Whether the view has yet to answer the latest query: it has not
    /// been given it, or said its results are still coming.
    pub(crate) fn answering(&self) -> bool {
        let delivery = self.0.borrow();
        delivery.wake.is_some() && (delivery.pending.is_some() || delivery.awaiting)
    }

    /// The latest query asked.
    pub(crate) fn last(&self) -> Option<SearchQuery> {
        self.0.borrow().last.clone()
    }

    /// What the view says of its results for the latest query.
    pub(crate) fn settle(&self, awaiting: bool) {
        self.0.borrow_mut().awaiting = awaiting;
    }

    pub(crate) fn submit(&self, query: &SearchQuery) {
        if query
            .text()
            .chars()
            .nth(ui_core::search::MAX_SEARCH_QUERY_SCALARS)
            .is_some()
        {
            return;
        }
        let wake = {
            let mut delivery = self.0.borrow_mut();
            if delivery.last.as_ref() == Some(query) {
                return;
            }
            delivery.last = Some(query.clone());
            delivery.pending = Some(query.clone());
            delivery.awaiting = false;
            delivery.wake.clone()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }

    pub(crate) fn take(&self) -> Option<SearchQuery> {
        self.0.borrow_mut().pending.take()
    }
}
