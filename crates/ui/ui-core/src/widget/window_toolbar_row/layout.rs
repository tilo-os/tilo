//! Where each part of a window bar's row stands: the fold order that
//! decides what stays in the row and what goes into the overflow menu,
//! the location's forms, and what the views and labels measure.

use vello::kurbo::Vec2;

use super::{
    CONTROL_GAP, CONTROL_PX, FIELD_LOCATION_PX, FieldFace, Icon, LOCATION_ICON_PX, MIN_LOCATION_PX,
    Point, Rect, SECTION_GAP, Theme, WindowToolbar, picker_width,
};

/// The gap a location takes back beside it, each side: the space the row keeps between parts.
const SHARED_GAP: f64 = 2.0 * (SECTION_GAP - CONTROL_GAP);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    Sidebar,
    Back,
    Forward,
    Leading(usize),
    Location,
    View(usize),
    ViewsMenu,
    Action(usize),
    Notice(usize),
    Overflow,
    Reaction(usize),
    ReactionAdd,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Parts {
    pub sidebar: Rect,
    pub back: Rect,
    pub forward: Rect,
    pub leading: Vec<Rect>,
    pub location: Rect,
    pub views: Vec<Rect>,
    pub views_menu: Rect,
    pub actions: Vec<Rect>,
    pub actions_labelled: bool,
    pub leading_words: Vec<f64>,
    pub action_words: Vec<f64>,
    pub spare: f64,
    pub overflow: Rect,
    pub form: usize,
    pub labelled: bool,
    pub notice: Vec<Rect>,
    pub notice_labelled: Vec<bool>,
    pub reactions: Vec<Rect>,
    pub reaction_add: Rect,
}

pub(crate) type ViewsSize = Option<ViewsMeasure>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ViewsMeasure {
    pub strip: (f64, f64),
    /// The views menu's width: as wide as its widest label needs, so
    /// choosing another view never moves the row.
    pub menu: f64,
    /// What the location as the views' picker stands beside its label:
    /// its face's padding and chevron (none: the location cannot pick the
    /// views, [`picks_views`]). Laid out, the least width of the picker:
    /// that beside the form the location shows.
    pub location: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LocationForms {
    pub text: Vec<f64>,
    /// A breadcrumb's own name: the place it stands in, its narrowest whole form.
    pub name: Option<f64>,
    pub chrome: f64,
    /// Each of the notice's buttons as a text button (none: the notice up
    /// has none, or it is not measured).
    pub actions: Vec<f64>,
    pub trailing: Option<(f64, f64)>,
}

impl LocationForms {
    /// The form the location shows in `width`: the widest that stands whole, else the narrowest.
    pub(super) fn fit(&self, width: f64) -> usize {
        let widest = width + SHARED_GAP;
        self.text
            .iter()
            .position(|words| words + self.chrome <= widest)
            .unwrap_or_else(|| self.text.len().saturating_sub(1))
    }

    /// The width the location's narrowest form needs whole: its chrome and the gaps beside it.
    pub(super) fn whole(&self) -> f64 {
        let words = self.text.last().copied().or(self.name);
        words.map_or(MIN_LOCATION_PX, |words| words + self.chrome)
    }

    /// The least width the location takes: [`Self::whole`], less the gap it takes back each side.
    pub(super) fn least(&self) -> f64 {
        (self.whole() - SHARED_GAP).max(MIN_LOCATION_PX)
    }

    pub(super) fn width(&self, form: usize) -> f64 {
        self.text.get(form).copied().unwrap_or(0.0)
    }

    pub(super) fn notice(&self, toolbar: &WindowToolbar, leftover: f64) -> Option<NoticeFit> {
        let notice = toolbar.notice.as_ref()?;
        let whole = self.width(0) + self.chrome;
        let count = notice.actions.len();
        let fit = |named: usize| -> (Vec<f64>, Vec<bool>, f64) {
            let labelled: Vec<bool> = (0..count)
                .map(|i| i < named && self.actions.get(i).is_some_and(|w| *w > 0.0))
                .collect();
            let buttons: Vec<f64> = (0..count)
                .map(|i| match (labelled[i], self.actions.get(i)) {
                    (true, Some(width)) => *width,
                    _ => CONTROL_PX,
                })
                .collect();
            let beside = buttons.iter().map(|button| CONTROL_GAP + button).sum();
            (buttons, labelled, beside)
        };
        let (buttons, labelled, beside) = (0..=count)
            .rev()
            .map(fit)
            .find(|(_, labelled, beside)| {
                labelled.iter().all(|named| !named) || whole + beside <= leftover
            })
            .unwrap_or_else(|| fit(0));
        Some(NoticeFit {
            buttons,
            labelled,
            least: (whole + beside).min(leftover),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct NoticeFit {
    pub(super) buttons: Vec<f64>,
    pub(super) labelled: Vec<bool>,
    pub(super) least: f64,
}

#[derive(Clone, Debug)]
pub(super) struct Measured {
    pub(super) key: MeasureKey,
    pub(super) views: ViewsSize,
    pub(super) forms: LocationForms,
    pub(super) tabs: Vec<f64>,
    pub(super) leading: Vec<f64>,
    pub(super) actions: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MeasureKey {
    pub(super) labels: Vec<String>,
    pub(super) leading: Vec<(String, bool)>,
    pub(super) actions: Vec<(String, bool)>,
    pub(super) forms: Vec<String>,
    pub(super) name: Option<String>,
    pub(super) editable: bool,
    pub(super) picker: bool,
    pub(super) lead: LocationLead,
    pub(super) notice: Vec<String>,
    pub(super) trailing: Option<String>,
    pub(super) text_scale: u32,
}

pub(super) fn location_chrome(
    (editable, picker, lead): (bool, bool, LocationLead),
    theme: &Theme,
) -> f64 {
    if picker {
        return picker_width(0.0, theme);
    }
    let mut chrome = 0.0;
    if editable {
        let rect = Rect::new(0.0, 0.0, FIELD_LOCATION_PX, CONTROL_PX);
        chrome += rect.width() - FieldFace::new(rect, "").text_rect(theme).width();
    }
    chrome += match lead {
        LocationLead::None => return chrome,
        LocationLead::Icon => LOCATION_ICON_PX,
        LocationLead::Avatar => crate::widget::AvatarSize::Small.side(theme),
    } + f64::from(theme.tokens.spacing.s2);
    chrome
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LocationLead {
    None,
    Icon,
    Avatar,
}

impl LocationLead {
    pub(super) fn of(location: &crate::window_toolbar::ToolbarLocation) -> Self {
        if location.avatar.is_some() {
            Self::Avatar
        } else if location.icon.as_deref().and_then(Icon::from_name).is_some() {
            Self::Icon
        } else {
            Self::None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum ViewsPlace {
    Strip(f64, f64),
    Menu(f64),
    /// The location as the views' picker, at least this wide.
    InLocation(f64),
    Overflow,
}

pub(super) fn folded_views(measured: ViewsMeasure) -> ViewsPlace {
    measured
        .location
        .map_or(ViewsPlace::Menu(measured.menu), ViewsPlace::InLocation)
}

/// The actions a row never gives up: those up to and including the window's main one
/// ([`crate::window_toolbar::ToolbarAction::primary`]), and none where it declares none.
fn main_action_floor(toolbar: &WindowToolbar, row_actions: usize) -> usize {
    toolbar
        .actions
        .iter()
        .take_while(|action| !action.menu)
        .position(|action| action.primary)
        .map_or(0, |index| index + 1)
        .min(row_actions)
}

pub(super) fn picks_views(toolbar: &WindowToolbar) -> bool {
    toolbar.notice.is_none()
        && toolbar.location.as_ref().is_some_and(|location| {
            !location.editable
                && location.choices.is_empty()
                && location.segments.is_empty()
                && !location.label.is_empty()
        })
}

/// What one layout keeps in the row: the first `plain` leading actions
/// that do not move through the content, the first `navigating` that do,
/// the first `inline` actions, and whether the forward button and the
/// sidebar's toggle stand in the row (where not, they are in the overflow
/// menu).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Kept {
    pub(super) plain: usize,
    pub(super) navigating: usize,
    pub(super) inline: usize,
    pub(super) forward: bool,
    pub(super) sidebar: bool,
}

impl Kept {
    pub(super) fn all(toolbar: &WindowToolbar, inline: usize) -> Self {
        let navigating = toolbar
            .leading
            .iter()
            .filter(|action| action.navigates)
            .count();
        Self {
            plain: toolbar.leading.len().saturating_sub(navigating),
            navigating,
            inline,
            forward: true,
            sidebar: true,
        }
    }

    pub(super) fn shows(self, toolbar: &WindowToolbar, index: usize) -> bool {
        let Some(action) = toolbar.leading.get(index) else {
            return false;
        };
        let rank = toolbar
            .leading
            .iter()
            .take(index)
            .filter(|other| other.navigates == action.navigates)
            .count();
        rank < if action.navigates {
            self.navigating
        } else {
            self.plain
        }
    }
}

impl Parts {
    /// Lay `toolbar`'s parts out across `row`, centred on its centre line:
    /// navigation and the leading actions leading, the views and the
    /// actions trailing, the location filling between. The location takes
    /// what the controls leave: it names itself in the widest of its forms
    /// that stands whole there, then the narrowest, and leaves the row where
    /// even that does not. The controls keep their places while they fit: at most [`crate::window_toolbar::MAX_ACTIONS`] actions stand
    /// in the row, and only where the buttons would overlap do the last of
    /// them move into the overflow menu, as do the declared actions past
    /// that many and those declared for the menu, and the view switcher
    /// folds into the views menu. Beyond that the rest goes into the
    /// overflow menu in [`Parts::yielding`]'s order. One action never goes
    /// into the menu alone: the menu's button would take its place and hide
    /// it. The window's main action
    /// ([`crate::window_toolbar::ToolbarAction::primary`]) stays in the row
    /// at every width, its icon alone where the words do not fit.
    ///
    /// The location is the views' picker only where the form it shows stands
    /// whole beside its chevron.
    ///
    /// A labelled leading action is a text button only where the row folds
    /// nothing at all, its label included, and an icon otherwise: its label
    /// is the first thing a narrowing row gives up.
    ///
    /// A notice up in the location ([`WindowToolbar::notice`]) stands
    /// whole, its button right after its text, before anything that yields
    /// before the location stands in the row; its text is cut short only
    /// where all of that is in the menu. Its button never is: it names
    /// itself where it does so beside the whole text, and is its icon
    /// where not.
    pub(crate) fn lay_out(
        toolbar: &WindowToolbar,
        row: Rect,
        views: ViewsSize,
        (forms, labels, action_labels): (&LocationForms, &[f64], &[f64]),
    ) -> Self {
        if row.width() <= 0.0 || row.height() <= 0.0 {
            return Self::default();
        }
        let navigation = Kept {
            plain: 0,
            inline: 0,
            ..Kept::all(toolbar, 0)
        };
        let leftover =
            Self::with_inline(toolbar, row, navigation, ViewsPlace::Overflow, (&[], &[]))
                .location
                .width();
        let form = forms.fit(leftover);
        let notice = forms.notice(toolbar, leftover);
        let least = match notice.as_ref() {
            Some(notice) => notice.least,
            None if toolbar.location.is_none() => 0.0,
            None => forms.least(),
        };
        let shown = (forms.width(form), least);
        let icons = Self::arranged(toolbar, row, views, shown, (&[], &[]));
        let names = labels
            .iter()
            .chain(action_labels)
            .any(|width| *width > CONTROL_PX);
        let mut parts = if form == 0 && names && icons.folds_nothing(toolbar) {
            let labelled = Self::arranged(toolbar, row, views, shown, (labels, action_labels));
            if labelled.folds_as(&icons) {
                Self {
                    labelled: labels.iter().any(|width| *width > 0.0),
                    ..labelled
                }
            } else {
                icons
            }
        } else {
            icons
        };
        let spare = (parts.location.width() - (forms.width(form) + forms.chrome)).max(0.0);
        if !parts.actions_labelled {
            let mut extra = 0.0;
            let mut kept = vec![0.0; action_labels.len()];
            for (index, action) in toolbar.actions.iter().enumerate() {
                if !action.labelled {
                    continue;
                }
                let Some(words) = action_labels
                    .get(index)
                    .copied()
                    .filter(|words| *words > CONTROL_PX)
                else {
                    continue;
                };
                let needed = words - CONTROL_PX;
                if extra + needed <= spare {
                    extra += needed;
                    if let Some(part) = kept.get_mut(index) {
                        *part = words;
                    }
                }
            }
            if extra > 0.0 {
                let labelled = Self::arranged(toolbar, row, views, shown, (&[], &kept));
                if labelled.folds_as(&parts) {
                    parts = labelled;
                }
            }
        }
        if !parts.labelled
            && let Some(index) = toolbar.leading.iter().position(|action| action.primary)
            && let Some(words) = labels
                .get(index)
                .copied()
                .filter(|words| *words > CONTROL_PX)
            && spare + CONTROL_PX >= words
        {
            let mut kept = vec![0.0; labels.len()];
            if let Some(part) = kept.get_mut(index) {
                *part = words;
            }
            let labelled = Self::arranged(toolbar, row, views, shown, (&kept, &[]));
            if labelled.folds_as(&parts) {
                parts = labelled;
            }
        }
        parts.settle(form, labels, action_labels, notice, forms);
        let extra: f64 = action_labels
            .iter()
            .take(parts.actions.len())
            .map(|words| (words - CONTROL_PX).max(0.0))
            .sum();
        if !parts.actions_labelled
            && extra > 0.0
            && extra <= parts.spare
            && !parts.views_in_location()
        {
            let moved = |rect: Rect| Rect::new(rect.x0 - extra, rect.y0, rect.x1 - extra, rect.y1);
            let mut x1 = parts.actions.iter().map(|rect| rect.x1).fold(0.0, f64::max);
            for (rect, words) in parts.actions.iter_mut().zip(action_labels).rev() {
                *rect = Rect::new(x1 - CONTROL_PX.max(*words), rect.y0, x1, rect.y1);
                x1 = rect.x0 - CONTROL_GAP;
            }
            for rect in &mut parts.views {
                *rect = moved(*rect);
            }
            if parts.views_menu.area() > 0.0 {
                parts.views_menu = moved(parts.views_menu);
            }
            parts.location.x1 -= extra;
            parts.actions_labelled = true;
            let notice = forms.notice(toolbar, leftover);
            parts.settle(form, labels, action_labels, notice, forms);
        }
        if toolbar.notice.is_none() && !parts.views_in_location() {
            parts.fit_shown_form(row, forms);
        }
        parts
    }

    /// The location's form stands whole: where the row left it short of that, it takes back the
    /// gap beside it, each side. Where even that does not hold it, a location with shorter forms
    /// leaves the row rather than be cut, and one without is cut inside its place.
    fn fit_shown_form(&mut self, row: Rect, forms: &LocationForms) {
        self.form = forms.fit(self.location.width());
        // A breadcrumb's form is its own name, not a line of the location's text.
        let need = if forms.text.is_empty() {
            forms.whole()
        } else {
            forms.width(self.form) + forms.chrome
        };
        if self.location.area() <= 0.0 || need <= self.location.width() {
            return;
        }
        let short = (need - self.location.width()).ceil();
        let shared = SHARED_GAP / 2.0;
        let left = if self.location.x0 > row.x0 {
            shared.min(short)
        } else {
            0.0
        };
        let right = if self.location.x1 < row.x1 {
            shared.min(short - left)
        } else {
            0.0
        };
        self.location.x0 -= left;
        self.location.x1 += right;
        if forms.text.len() > 1 && self.location.width() < need {
            self.location = Rect::ZERO;
        }
    }

    fn settle(
        &mut self,
        form: usize,
        labels: &[f64],
        action_labels: &[f64],
        notice: Option<NoticeFit>,
        forms: &LocationForms,
    ) {
        self.form = form;
        self.leading_words = labels.to_vec();
        self.action_words = action_labels.to_vec();
        if let Some(notice) = notice {
            self.place_notice(notice, forms.width(form) + forms.chrome);
        }
        self.spare =
            (self.location.width() - (forms.width(form) + forms.chrome) - super::MIN_LOCATION_PX)
                .max(0.0);
    }

    fn place_notice(&mut self, notice: NoticeFit, text: f64) {
        let location = self.location;
        if notice.buttons.is_empty() || location.area() <= 0.0 {
            return;
        }
        let span = notice.buttons.iter().sum::<f64>()
            + CONTROL_GAP
                * f64::from(u32::try_from(notice.buttons.len().saturating_sub(1)).unwrap_or(0));
        let first = (location.x0 + text + CONTROL_GAP)
            .min(location.x1 - span)
            .max(location.x0);
        let mut x0 = first;
        self.notice = notice
            .buttons
            .iter()
            .map(|width| {
                let rect = Rect::new(x0, location.y0, (x0 + width).min(location.x1), location.y1);
                x0 = rect.x1 + CONTROL_GAP;
                rect
            })
            .collect();
        self.notice_labelled = notice.labelled;
        self.location.x1 = (first - CONTROL_GAP).max(location.x0);
    }

    pub(super) fn folds_nothing(&self, toolbar: &WindowToolbar) -> bool {
        let views = toolbar
            .views
            .as_ref()
            .map_or(0, |views| views.choices.len());
        self.leading.iter().all(|rect| rect.area() > 0.0)
            && self.actions.len()
                == toolbar
                    .row_actions()
                    .min(crate::window_toolbar::MAX_ACTIONS)
            && self.views.len() == views
    }

    pub(super) fn folds_as(&self, other: &Self) -> bool {
        let shown = |parts: &Self| -> Vec<bool> {
            parts.leading.iter().map(|rect| rect.area() > 0.0).collect()
        };
        shown(self) == shown(other)
            && (self.sidebar.area() > 0.0) == (other.sidebar.area() > 0.0)
            && (self.forward.area() > 0.0) == (other.forward.area() > 0.0)
            && self.views.len() == other.views.len()
            && (self.views_menu.area() > 0.0) == (other.views_menu.area() > 0.0)
            && self.views_in_location() == other.views_in_location()
            && self.actions.len() == other.actions.len()
            && (self.overflow.area() > 0.0) == (other.overflow.area() > 0.0)
            && (self.location.area() > 0.0) == (other.location.area() > 0.0)
    }

    /// Lay the row out: the parts stand while they fit, the actions folding into the overflow
    /// menu last first as it narrows. A row that folds down to one action keeps it unless its
    /// words are what it cannot show: then the menu's row says what its glyph cannot.
    pub(super) fn arranged(
        toolbar: &WindowToolbar,
        row: Rect,
        views: ViewsSize,
        (form_w, least): (f64, f64),
        (labels, action_labels): (&[f64], &[f64]),
    ) -> Self {
        let row_actions = toolbar
            .row_actions()
            .min(crate::window_toolbar::MAX_ACTIONS);
        let measured = views
            .filter(|_| {
                toolbar
                    .views
                    .as_ref()
                    .is_some_and(|views| !views.choices.is_empty())
            })
            .map(|m| ViewsMeasure {
                location: m
                    .location
                    .filter(|_| picks_views(toolbar))
                    .map(|chrome| form_w.ceil() + chrome),
                ..m
            });
        let lone = |inline: usize, place: ViewsPlace| {
            row_actions - inline == 1
                && toolbar.actions.len() == row_actions
                && (toolbar.views.is_none() || place != ViewsPlace::Overflow)
        };
        let places = match measured {
            Some(m) => vec![
                ViewsPlace::Strip(m.strip.0, m.strip.1),
                folded_views(m),
                ViewsPlace::Overflow,
            ],
            None => vec![ViewsPlace::Overflow],
        };
        let all = Kept::all(toolbar, row_actions);
        let floor = main_action_floor(toolbar, row_actions);
        for &place in &places {
            let mut inline = row_actions;
            loop {
                let parts = Self::with_inline(
                    toolbar,
                    row,
                    Kept { inline, ..all },
                    place,
                    (labels, action_labels),
                );
                let placed =
                    !matches!(place, ViewsPlace::InLocation(_)) || parts.views_menu.area() > 0.0;
                if parts.fits(row)
                    && parts.location.width() >= least
                    && placed
                    && !lone(inline, place)
                {
                    return parts;
                }
                if inline <= floor {
                    break;
                }
                inline -= 1;
            }
        }
        let mut last = Self::default();
        for (kept, place) in Self::yielding(all, measured) {
            let kept = Kept {
                inline: kept.inline.max(floor),
                ..kept
            };
            let inline = if kept.plain == all.plain
                && kept.navigating == all.navigating
                && lone(kept.inline, place)
            {
                row_actions
            } else {
                kept.inline
            };
            let parts = Self::with_inline(
                toolbar,
                row,
                Kept { inline, ..kept },
                place,
                (labels, action_labels),
            );
            let located = match place {
                ViewsPlace::InLocation(least) => parts.location.width() >= least,
                _ => {
                    toolbar.location.is_none()
                        || (parts.location.area() > 0.0 && parts.location.width() >= least)
                }
            };
            if parts.fits(row) && located {
                return parts;
            }
            last = parts;
        }
        last
    }

    /// What goes into the overflow menu, in order, where the row is too
    /// narrow for the location beside everything else: the plain leading
    /// actions, the last first; then the views (the views menu's place);
    /// then the leading actions that move through the content, the last
    /// first; then, where the location still has not its least width (a
    /// phone's), the forward button and after it the sidebar's toggle. The
    /// location yields to none of them: it stays, elided. Back and the
    /// window's own controls never go.
    pub(super) fn yielding(all: Kept, measured: ViewsSize) -> Vec<(Kept, ViewsPlace)> {
        let all = Kept { inline: 0, ..all };
        let views = measured.map_or(ViewsPlace::Overflow, folded_views);
        let plain = (0..=all.plain)
            .rev()
            .map(|plain| (Kept { plain, ..all }, views));
        // The leading actions that move through the content are a pair: half of one is worse
        // than neither, so they stand together or fold together.
        let navigating = std::iter::once((
            Kept {
                plain: 0,
                navigating: 0,
                inline: 0,
                ..all
            },
            ViewsPlace::Overflow,
        ));
        let bare = Kept {
            plain: 0,
            navigating: 0,
            inline: 0,
            ..all
        };
        let navigation = [
            Kept {
                forward: false,
                ..bare
            },
            Kept {
                forward: false,
                sidebar: false,
                ..bare
            },
        ]
        .map(|kept| (kept, ViewsPlace::Overflow));
        plain.chain(navigating).chain(navigation).collect()
    }

    pub(super) fn with_inline(
        toolbar: &WindowToolbar,
        row: Rect,
        kept: Kept,
        place: ViewsPlace,
        (labels, action_labels): (&[f64], &[f64]),
    ) -> Self {
        let mut parts = Self::default();
        let y0 = row.center().y - CONTROL_PX / 2.0;
        let square = |x0: f64| Rect::new(x0, y0, x0 + CONTROL_PX, y0 + CONTROL_PX);
        let mut x0 = row.x0;
        let mut folded = false;
        if toolbar.sidebar.is_some() {
            if kept.sidebar {
                parts.sidebar = square(x0);
                x0 = parts.sidebar.x1 + SECTION_GAP;
            } else {
                folded = true;
            }
        }
        if toolbar.navigation.is_some() {
            parts.back = square(x0);
            x0 = parts.back.x1;
            if kept.forward {
                parts.forward = square(x0 + CONTROL_GAP);
                x0 = parts.forward.x1;
            } else {
                folded = true;
            }
            x0 += SECTION_GAP;
        }
        let mut any = false;
        for index in 0..toolbar.leading.len() {
            if kept.shows(toolbar, index) {
                let mut rect = square(x0);
                rect.x1 = rect.x1.max(x0 + labels.get(index).copied().unwrap_or(0.0));
                x0 = rect.x1 + CONTROL_GAP;
                parts.leading.push(rect);
                any = true;
            } else {
                parts.leading.push(Rect::ZERO);
                folded = true;
            }
        }
        if any {
            x0 = x0 - CONTROL_GAP + SECTION_GAP;
        }
        let inline = kept.inline;
        let mut x1 = row.x1;
        let mut trailing = false;
        let in_menu = toolbar.actions.len() > inline
            || folded
            || (toolbar.views.is_some() && place == ViewsPlace::Overflow);
        if in_menu {
            parts.overflow = Rect::new(x1 - CONTROL_PX, y0, x1, y0 + CONTROL_PX);
            x1 = parts.overflow.x0 - CONTROL_GAP;
            trailing = true;
        }
        let mut actions = Vec::with_capacity(inline);
        for index in (0..inline).rev() {
            let width = CONTROL_PX.max(action_labels.get(index).copied().unwrap_or(0.0));
            let rect = Rect::new(x1 - width, y0, x1, y0 + CONTROL_PX);
            x1 = rect.x0 - CONTROL_GAP;
            actions.push(rect);
            trailing = true;
        }
        actions.reverse();
        parts.actions_labelled = action_labels.iter().take(inline).any(|width| *width > 0.0);
        parts.actions = actions;
        if trailing {
            x1 = x1 + CONTROL_GAP - SECTION_GAP;
        }
        let choices = toolbar
            .views
            .as_ref()
            .map_or(0, |views| views.choices.len());
        match place {
            ViewsPlace::Strip(width, height) if choices > 0 => {
                let strip = Rect::new(
                    x1 - width,
                    row.center().y - height / 2.0,
                    x1,
                    row.center().y + height / 2.0,
                );
                parts.views = equal_segments(strip, choices);
                x1 = strip.x0 - SECTION_GAP;
            }
            ViewsPlace::Menu(width) if choices > 0 => {
                parts.views_menu = Rect::new(x1 - width, y0, x1, y0 + CONTROL_PX);
                x1 = parts.views_menu.x0 - SECTION_GAP;
            }
            _ => {}
        }
        if toolbar.location.is_some() && x1 > x0 {
            parts.location = Rect::new(x0, y0, x1, y0 + CONTROL_PX);
        }
        if let ViewsPlace::InLocation(least) = place
            && choices > 0
            && parts.location.width() >= least
        {
            parts.views_menu = parts.location;
        }
        parts
    }

    pub(super) fn fits(&self, row: Rect) -> bool {
        let lead = self.leading.iter().map(|rect| rect.x1).fold(
            self.forward
                .x1
                .max(self.back.x1)
                .max(self.sidebar.x1)
                .max(row.x0),
            f64::max,
        );
        self.actions
            .iter()
            .chain(self.views.first())
            .chain((self.views_menu.area() > 0.0).then_some(&self.views_menu))
            .chain((self.overflow.area() > 0.0).then_some(&self.overflow))
            .all(|rect| rect.x0 >= lead)
    }

    pub(crate) fn views_in_location(&self) -> bool {
        self.views_menu.area() > 0.0 && self.views_menu == self.location
    }

    pub(crate) fn forward_folded(&self, toolbar: &WindowToolbar) -> bool {
        toolbar.navigation.is_some() && self.forward.area() <= 0.0
    }

    pub(crate) fn sidebar_folded(&self, toolbar: &WindowToolbar) -> bool {
        toolbar.sidebar.is_some() && self.sidebar.area() <= 0.0
    }

    pub(super) fn folded_leading(&self) -> impl Iterator<Item = usize> + '_ {
        self.leading
            .iter()
            .enumerate()
            .filter(|(_, rect)| rect.area() <= 0.0)
            .map(|(index, _)| index)
    }

    pub(super) fn hit(&self, p: Point) -> Option<Part> {
        self.hit_where(p, |_| Some(Vec2::ZERO))
    }

    /// The part at `p` with each part drawn `offset` from where it is laid
    /// out (a part gliding to its new place); a part whose offset is `None`
    /// is not looked at.
    pub(super) fn hit_where(
        &self,
        p: Point,
        offset: impl Fn(&Part) -> Option<Vec2>,
    ) -> Option<Part> {
        let at = |part: Part, rect: &Rect| {
            offset(&part)
                .is_some_and(|offset| (*rect + offset).contains(p))
                .then_some(part)
        };
        at(Part::Sidebar, &self.sidebar)
            .or_else(|| at(Part::Back, &self.back))
            .or_else(|| at(Part::Forward, &self.forward))
            .or_else(|| {
                self.leading
                    .iter()
                    .enumerate()
                    .find_map(|(i, rect)| at(Part::Leading(i), rect))
            })
            .or_else(|| {
                self.views
                    .iter()
                    .enumerate()
                    .find_map(|(i, rect)| at(Part::View(i), rect))
            })
            .or_else(|| at(Part::ViewsMenu, &self.views_menu))
            .or_else(|| {
                self.actions
                    .iter()
                    .enumerate()
                    .find_map(|(i, rect)| at(Part::Action(i), rect))
            })
            .or_else(|| {
                self.notice
                    .iter()
                    .enumerate()
                    .find_map(|(i, rect)| at(Part::Notice(i), rect))
            })
            .or_else(|| at(Part::Overflow, &self.overflow))
            .or_else(|| {
                self.reactions
                    .iter()
                    .enumerate()
                    .find_map(|(i, rect)| at(Part::Reaction(i), rect))
            })
            .or_else(|| at(Part::ReactionAdd, &self.reaction_add))
            .or_else(|| at(Part::Location, &self.location))
    }
}

pub(super) fn equal_segments(strip: Rect, n: usize) -> Vec<Rect> {
    let Some(count) = u32::try_from(n).ok().filter(|n| *n > 0) else {
        return Vec::new();
    };
    let share = strip.width() / f64::from(count);
    (0..count)
        .map(|i| {
            let x0 = strip.x0 + share * f64::from(i);
            let x1 = if i.saturating_add(1) == count {
                strip.x1
            } else {
                x0 + share
            };
            Rect::new(x0, strip.y0, x1, strip.y1)
        })
        .collect()
}
