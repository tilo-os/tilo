//! Desktop search whose code may load on first use: the browser desktop
//! starts without the palette, its sources and ranking, and fetches them
//! as its surface first lays out ([`DESKTOP_SEARCH`]).

use ui::PlatformConventions;
use ui::widget::Widget;

use crate::model::{Hit, Target};
use crate::palette::{DesktopSearch, SearchData};
use crate::usage::Usage;

/// What a desktop asks of its search, as the trait object
/// [`DESKTOP_SEARCH`] makes.
pub trait DesktopSearchApi {
    fn is_open(&self) -> bool;
    fn waiting(&self) -> bool;
    fn open(&mut self, data: SearchData);
    fn close(&mut self);
    fn dismissing(&mut self);
    fn set_query(&mut self, query: &str);
    fn refresh(&mut self) -> bool;
    fn observe(&mut self) -> bool;
    fn take_choice(&mut self) -> Option<Target>;
    fn results(&self) -> &[Hit];
    fn query(&self) -> &str;
    fn choose(&mut self, index: usize, argument: Option<String>) -> bool;
    fn usage(&self) -> &Usage;
    fn set_usage(&mut self, usage: Usage);
    fn set_max_width(&mut self, width: Option<f64>);
    fn desired_size(&self) -> (f64, f64);
    fn palette(&self) -> &dyn Widget;
    fn palette_mut(&mut self) -> &mut dyn Widget;
}

impl DesktopSearchApi for DesktopSearch {
    fn is_open(&self) -> bool {
        Self::is_open(self)
    }
    fn waiting(&self) -> bool {
        Self::waiting(self)
    }
    fn open(&mut self, data: SearchData) {
        Self::open(self, data);
    }
    fn close(&mut self) {
        Self::close(self);
    }
    fn dismissing(&mut self) {
        Self::dismissing(self);
    }
    fn set_query(&mut self, query: &str) {
        Self::set_query(self, query);
    }
    fn refresh(&mut self) -> bool {
        Self::refresh(self)
    }
    fn observe(&mut self) -> bool {
        Self::observe(self)
    }
    fn take_choice(&mut self) -> Option<Target> {
        Self::take_choice(self)
    }
    fn results(&self) -> &[Hit] {
        Self::results(self)
    }
    fn query(&self) -> &str {
        Self::query(self)
    }
    fn choose(&mut self, index: usize, argument: Option<String>) -> bool {
        Self::choose(self, index, argument)
    }
    fn usage(&self) -> &Usage {
        Self::usage(self)
    }
    fn set_usage(&mut self, usage: Usage) {
        Self::set_usage(self, usage);
    }
    fn set_max_width(&mut self, width: Option<f64>) {
        self.palette_mut().set_max_width(width);
    }
    fn desired_size(&self) -> (f64, f64) {
        self.palette().desired_size()
    }
    fn palette(&self) -> &dyn Widget {
        Self::palette(self)
    }
    fn palette_mut(&mut self) -> &mut dyn Widget {
        Self::palette_mut(self)
    }
}

/// Desktop search's code, as a program that fetches it on first use hands
/// it over.
#[derive(Clone, Copy)]
pub struct MakeDesktopSearch(fn() -> Box<dyn DesktopSearchApi>);

/// The desktop search this build links.
#[must_use]
pub fn builtin_desktop_search() -> MakeDesktopSearch {
    MakeDesktopSearch(|| Box::new(DesktopSearch::new()))
}

ui::lazy_code! {
    /// Desktop search's code: linked, or with `lazy-search` fetched as the
    /// desktop first lays out.
    pub static DESKTOP_SEARCH: MakeDesktopSearch = builtin_desktop_search, feature = "lazy-search";
}

/// Search opened before its code was in: what to open over, then.
struct Opening {
    data: SearchData,
    query: Option<String>,
    platform: Option<PlatformConventions>,
}

/// Desktop search whose code may still be on its way. Until it is in,
/// search is closed and what it was asked to open over waits; the first
/// [`Self::adopt`] after the code arrives opens it.
#[derive(Default)]
pub struct LazyDesktopSearch {
    search: Option<Box<dyn DesktopSearchApi>>,
    opening: Option<Opening>,
}

impl LazyDesktopSearch {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Make the search once its code is in (asking for the code meanwhile),
    /// and open what waited on it. Whether search opened just now.
    pub fn adopt(&mut self) -> bool {
        if self.search.is_none() {
            let Some(make) = DESKTOP_SEARCH.get() else {
                return false;
            };
            self.search = Some((make.0)());
        }
        let Some(opening) = self.opening.take() else {
            return false;
        };
        self.open(opening.data);
        if let Some(platform) = opening.platform {
            self.set_platform_conventions(platform);
        }
        if let Some(query) = opening.query {
            self.set_query(&query);
        }
        true
    }

    pub fn is_open(&self) -> bool {
        self.search.as_ref().is_some_and(|search| search.is_open())
    }

    /// Whether an application's answer to the query is still coming.
    pub fn waiting(&self) -> bool {
        self.search.as_ref().is_some_and(|search| search.waiting())
    }

    /// Open over `data` with an empty query, or once the code is in.
    pub fn open(&mut self, data: SearchData) {
        self.adopt();
        match &mut self.search {
            Some(search) => search.open(data),
            None => {
                self.opening = Some(Opening {
                    data,
                    query: None,
                    platform: None,
                });
            }
        }
    }

    pub fn close(&mut self) {
        self.opening = None;
        if let Some(search) = &mut self.search {
            search.close();
        }
    }

    pub fn dismissing(&mut self) {
        if let Some(search) = &mut self.search {
            search.dismissing();
        }
    }

    pub fn set_query(&mut self, query: &str) {
        match (&mut self.search, &mut self.opening) {
            (Some(search), _) => search.set_query(query),
            (None, Some(opening)) => opening.query = Some(query.to_owned()),
            (None, None) => {}
        }
    }

    pub fn set_platform_conventions(&mut self, platform: PlatformConventions) {
        match (&mut self.search, &mut self.opening) {
            (Some(search), _) => search.palette_mut().set_platform_conventions(platform),
            (None, Some(opening)) => opening.platform = Some(platform),
            (None, None) => {}
        }
    }

    pub fn refresh(&mut self) -> bool {
        self.search.as_mut().is_some_and(|search| search.refresh())
    }

    pub fn observe(&mut self) -> bool {
        self.search.as_mut().is_some_and(|search| search.observe())
    }

    pub fn take_choice(&mut self) -> Option<Target> {
        self.search.as_mut()?.take_choice()
    }

    /// The rows shown, in palette order; none while the code is on its way.
    pub fn results(&self) -> &[Hit] {
        self.search.as_ref().map_or(&[], |search| search.results())
    }

    /// What is typed; nothing while the code is on its way.
    pub fn query(&self) -> &str {
        self.search.as_ref().map_or("", |search| search.query())
    }

    /// Choose the shown row `index`, as a person choosing it does.
    pub fn choose(&mut self, index: usize, argument: Option<String>) -> bool {
        self.search
            .as_mut()
            .is_some_and(|search| search.choose(index, argument))
    }

    /// What the person chose before, once the code is in.
    pub fn usage(&self) -> Option<&Usage> {
        Some(self.search.as_ref()?.usage())
    }

    /// What the person chose before, as the host kept it: given once the
    /// code is in.
    pub fn set_usage(&mut self, usage: Usage) {
        self.adopt();
        if let Some(search) = &mut self.search {
            search.set_usage(usage);
        }
    }

    pub fn set_max_width(&mut self, width: Option<f64>) {
        if let Some(search) = &mut self.search {
            search.set_max_width(width);
        }
    }

    pub fn desired_size(&self) -> (f64, f64) {
        self.search
            .as_ref()
            .map_or((0.0, 0.0), |search| search.desired_size())
    }

    /// The palette, once the code is in.
    pub fn palette(&self) -> Option<&dyn Widget> {
        Some(self.search.as_ref()?.palette())
    }

    /// The palette, once the code is in.
    pub fn palette_mut(&mut self) -> Option<&mut dyn Widget> {
        Some(self.search.as_mut()?.palette_mut())
    }
}
