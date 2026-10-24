//! Where each slot of a window bar's row stands: the fold order that
//! decides what stays in the row and what goes into the overflow menu,
//! the location's forms, and what the views and labels measure.

use super::{
    COMFORTABLE_LOCATION_PX, CONTROL_GAP, CONTROL_PX, FieldFace, LOCATION_ICON_PX, MIN_LOCATION_PX,
    Point, Rect, SECTION_GAP, Theme, WindowToolbar, picker_width,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    Sidebar,
    Back,
    Forward,
    /// One of the actions after the navigation.
    Leading(usize),
    Location,
    /// One choice of the view switcher.
    View(usize),
    /// The views folded into a menu: the chosen one named, the others a
    /// press away.
    ViewsMenu,
    Action(usize),
    /// Button `n` of the notice's own, after its text.
    Notice(usize),
    /// The button whose menu holds the actions the row has no room for.
    Overflow,
}

/// Where each slot of one toolbar sits.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Slots {
    pub sidebar: Rect,
    pub back: Rect,
    pub forward: Rect,
    /// The leading actions, after the navigation.
    pub leading: Vec<Rect>,
    pub location: Rect,
    /// The view switcher's segments (none: folded into the views menu, or
    /// into the overflow menu).
    pub views: Vec<Rect>,
    /// The views menu, where the row has no room for the switcher
    /// (`Rect::ZERO`: none).
    pub views_menu: Rect,
    /// The actions standing in the row, the first of the declared ones.
    pub actions: Vec<Rect>,
    /// The overflow button (`Rect::ZERO`: every action is in the row).
    pub overflow: Rect,
    /// Which form of its label the location shows
    /// ([`crate::window_toolbar::ToolbarLocation::form`]).
    pub form: usize,
    /// The labelled leading actions in the row are text buttons
    /// ([`crate::window_toolbar::ToolbarAction::labelled`]), not icons.
    pub labelled: bool,
    /// The notice's buttons, in order right after its text (none: it has
    /// none, or the row has no room for its text).
    pub notice: Vec<Rect>,
    /// Which of the notice's buttons name themselves, not only their icon.
    pub notice_labelled: Vec<bool>,
}

/// What the row lays out beside the toolbar: the view switcher's size and
/// the views menu's width, as the theme draws their labels (none before
/// they are measured).
pub(crate) type ViewsSize = Option<ViewsMeasure>;

/// The two forms the views take in the row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ViewsMeasure {
    /// The segmented switcher's width and height.
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

/// The location's label and its narrower forms as the theme draws them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LocationForms {
    /// Each form's text width, the label's first.
    pub text: Vec<f64>,
    /// What stands beside the text in the location at rest: its icon, a
    /// field's frame.
    pub chrome: f64,
    /// Each of the notice's buttons as a text button (none: the notice up
    /// has none, or it is not measured).
    pub actions: Vec<f64>,
}

impl LocationForms {
    /// The widest form that stands whole at rest in `room`, else the
    /// narrowest.
    pub(super) fn fitting(&self, room: f64) -> usize {
        self.text
            .iter()
            .position(|width| width + self.chrome <= room)
            .unwrap_or_else(|| self.text.len().saturating_sub(1))
    }

    /// The room the location at rest keeps for `form` to stand whole: none
    /// where it has no narrower form to choose, or `form` is cut short in
    /// `room` all the same.
    pub(super) fn least(&self, form: usize, room: f64) -> f64 {
        match self.text.get(form) {
            Some(width) if self.text.len() > 1 && width + self.chrome <= room => {
                width + self.chrome
            }
            _ => 0.0,
        }
    }

    /// The text width of `form` (nothing measured: none).
    pub(super) fn width(&self, form: usize) -> f64 {
        self.text.get(form).copied().unwrap_or(0.0)
    }

    /// How a notice up in the location stands where the location has
    /// `room` beside the navigation alone: its buttons' widths, which name
    /// themselves, and the room the location keeps for the notice to stand
    /// whole, its buttons included. The buttons name themselves, in order,
    /// as far as they stand so beside the whole text in `room`: the last
    /// is its icon first. The location keeps no more than `room`,
    /// everything that yields before it in the menu, the text cut short
    /// beside the buttons.
    pub(super) fn notice(&self, toolbar: &WindowToolbar, room: f64) -> Option<NoticeFit> {
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
                labelled.iter().all(|named| !named) || whole + beside <= room
            })
            .unwrap_or_else(|| fit(0));
        Some(NoticeFit {
            buttons,
            labelled,
            least: (whole + beside).min(room),
        })
    }
}

/// How a notice up in the location stands in the row
/// ([`LocationForms::notice`]).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct NoticeFit {
    /// Its buttons' widths, in order.
    pub(super) buttons: Vec<f64>,
    /// Which of them name themselves.
    pub(super) labelled: Vec<bool>,
    /// The room the location keeps for it.
    pub(super) least: f64,
}

/// The views, the location's forms and the labelled leading actions as
/// the theme draws them, and what they were measured for.
#[derive(Clone, Debug)]
pub(super) struct Measured {
    pub(super) key: MeasureKey,
    pub(super) views: ViewsSize,
    pub(super) forms: LocationForms,
    /// Each view's tab width.
    pub(super) tabs: Vec<f64>,
    /// Each leading action's text button width (`0.0`: not labelled).
    pub(super) leading: Vec<f64>,
}

/// What the views and the location's forms were measured for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MeasureKey {
    pub(super) labels: Vec<String>,
    /// Each leading action's name where it is labelled, else empty.
    pub(super) leading: Vec<String>,
    pub(super) forms: Vec<String>,
    pub(super) editable: bool,
    pub(super) picker: bool,
    pub(super) icon: bool,
    /// The notice's buttons' names.
    pub(super) notice: Vec<String>,
    pub(super) text_scale: u32,
}

/// What stands beside the label's text in the location at rest
/// ([`paint_location`]): a picker's face and chevron where it chooses
/// among places, a field's frame where it is edited, its icon.
pub(super) fn location_chrome((editable, picker, icon): (bool, bool, bool), theme: &Theme) -> f64 {
    if picker {
        return picker_width(0.0, theme);
    }
    let mut chrome = 0.0;
    if editable {
        let rect = Rect::new(0.0, 0.0, COMFORTABLE_LOCATION_PX, CONTROL_PX);
        chrome += rect.width() - FieldFace::new(rect, "").text_rect(theme).width();
    }
    if icon {
        chrome += LOCATION_ICON_PX + f64::from(theme.tokens.spacing.s2);
    }
    chrome
}

/// Where the views stand in one layout of the row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum ViewsPlace {
    /// The segmented switcher, this wide and tall.
    Strip(f64, f64),
    /// The views menu, this wide.
    Menu(f64),
    /// The location as the views' picker, at least this wide.
    InLocation(f64),
    /// The overflow menu's first rows (or there are no views).
    Overflow,
}

/// Where the views go once the switcher has no room: into the location,
/// as its picker, where it can be one ([`picks_views`]), else into a views
/// menu of their own.
pub(super) fn folded_views(measured: ViewsMeasure) -> ViewsPlace {
    measured
        .location
        .map_or(ViewsPlace::Menu(measured.menu), ViewsPlace::InLocation)
}

/// Whether `toolbar`'s location can be the views' picker: a plain label,
/// neither edited, nor a picker of its own places, nor a breadcrumb.
pub(super) fn picks_views(toolbar: &WindowToolbar) -> bool {
    // A notice says what happened; it is no place to pick a view from.
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
/// and the first `inline` actions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Kept {
    pub(super) plain: usize,
    pub(super) navigating: usize,
    pub(super) inline: usize,
}

impl Kept {
    /// Every leading action, and the first `inline` actions.
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
        }
    }

    /// Whether leading action `index` stands in the row.
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

impl Slots {
    /// Lay `toolbar`'s slots out across `row`, centred on its centre line:
    /// navigation and the leading actions leading, the views and the
    /// actions trailing, the location filling between. At most
    /// [`crate::window_toolbar::MAX_ACTIONS`] actions stand in the row;
    /// while the location would be narrower than
    /// [`COMFORTABLE_LOCATION_PX`] (or, without one, while the buttons would
    /// overlap) the last of them move into the overflow menu, as do the
    /// declared actions past that many and those declared for the menu, and
    /// the view switcher folds into the views menu. Where that leaves the
    /// location narrower than [`MIN_LOCATION_PX`], the rest goes into the
    /// overflow menu in [`Slots::yielding`]'s order, so the location stays
    /// in the row, elided. One action never goes into the menu alone: the
    /// menu's button would take its place and hide it.
    ///
    /// The location shows the widest form of its label that stands whole
    /// beside the navigation, with everything that yields before it in the
    /// menu, and that yields only where that form would not stand whole:
    /// a wider row never shows a narrower form. The location is the views'
    /// picker only where that same form stands whole beside its chevron.
    /// The plain leading actions yield before the label shortens: they
    /// stand only beside its widest form.
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
        (forms, labels): (&LocationForms, &[f64]),
    ) -> Self {
        if row.width() <= 0.0 || row.height() <= 0.0 {
            return Self::default();
        }
        // The room the location has beside the navigation alone, everything
        // that yields before it ([`Self::yielding`]) in the menu.
        let navigation = Kept {
            plain: 0,
            inline: 0,
            ..Kept::all(toolbar, 0)
        };
        let room = Self::with_inline(toolbar, row, navigation, ViewsPlace::Overflow, &[])
            .location
            .width();
        let form = forms.fitting(room);
        let notice = forms.notice(toolbar, room);
        let least = notice
            .as_ref()
            .map_or_else(|| forms.least(form, room), |notice| notice.least);
        let shown = (forms.width(form), least, form > 0);
        let icons = Self::arranged(toolbar, row, views, shown, &[]);
        let mut slots = if form == 0
            && labels.iter().any(|width| *width > CONTROL_PX)
            && icons.folds_nothing(toolbar)
        {
            let labelled = Self::arranged(toolbar, row, views, shown, labels);
            if labelled.folds_as(&icons) {
                Self {
                    labelled: true,
                    ..labelled
                }
            } else {
                icons
            }
        } else {
            icons
        };
        slots.form = form;
        if let Some(notice) = notice {
            slots.place_notice(notice, forms.width(form) + forms.chrome);
        }
        slots
    }

    /// Put the notice's buttons in order right after its text, `text` wide
    /// with its icon, and inside the location's end where the text is cut
    /// short; the location keeps what is left before them.
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

    /// Whether these slots keep everything of `toolbar` in the row: every
    /// leading action, every action the row can hold, the views as their
    /// strip.
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

    /// Whether these slots keep in the row exactly what `other` keeps, and
    /// the views in the same place.
    pub(super) fn folds_as(&self, other: &Self) -> bool {
        let shown = |slots: &Self| -> Vec<bool> {
            slots.leading.iter().map(|rect| rect.area() > 0.0).collect()
        };
        shown(self) == shown(other)
            && self.views.len() == other.views.len()
            && (self.views_menu.area() > 0.0) == (other.views_menu.area() > 0.0)
            && self.views_in_location() == other.views_in_location()
            && self.actions.len() == other.actions.len()
            && (self.overflow.area() > 0.0) == (other.overflow.area() > 0.0)
            && (self.location.area() > 0.0) == (other.location.area() > 0.0)
    }

    /// [`Self::lay_out`] for a location showing a form `form_w` wide, which
    /// at rest stands whole in a location `least` wide, and leading actions
    /// `labels` wide (none, or `0.0`: an icon). Where the form is
    /// `shortened` the plain leading actions are in the menu: they yield
    /// before the location's label does.
    pub(super) fn arranged(
        toolbar: &WindowToolbar,
        row: Rect,
        views: ViewsSize,
        (form_w, least, shortened): (f64, f64, bool),
        labels: &[f64],
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
        // A menu holding one action alone saves no width: its button takes
        // the action's place and hides it.
        let lone = |inline: usize, place: ViewsPlace| {
            row_actions - inline == 1
                && toolbar.actions.len() == row_actions
                && (toolbar.views.is_none() || place != ViewsPlace::Overflow)
        };
        let places = match measured {
            Some(m) => vec![ViewsPlace::Strip(m.strip.0, m.strip.1), folded_views(m)],
            None => vec![ViewsPlace::Overflow],
        };
        let mut all = Kept::all(toolbar, row_actions);
        if shortened {
            all.plain = 0;
        }
        for &place in &places {
            let mut inline = row_actions;
            loop {
                let slots = Self::with_inline(toolbar, row, Kept { inline, ..all }, place, labels);
                let room = if toolbar.location.is_some() {
                    slots.location.width() >= COMFORTABLE_LOCATION_PX.max(least)
                } else {
                    slots.fits(row)
                };
                let placed =
                    !matches!(place, ViewsPlace::InLocation(_)) || slots.views_menu.area() > 0.0;
                if room && placed && !lone(inline, place) {
                    return slots;
                }
                if inline == 0 {
                    break;
                }
                inline -= 1;
            }
        }
        let mut last = Self::default();
        for (kept, place) in Self::yielding(all, measured) {
            let inline =
                if kept.plain == all.plain && kept.navigating == all.navigating && lone(0, place) {
                    row_actions
                } else {
                    0
                };
            let slots = Self::with_inline(toolbar, row, Kept { inline, ..kept }, place, labels);
            let located = match place {
                ViewsPlace::InLocation(least) => slots.location.width() >= least,
                _ => {
                    toolbar.location.is_none()
                        || (slots.location.area() > 0.0 && slots.location.width() >= least)
                }
            };
            if slots.fits(row) && located {
                return slots;
            }
            last = slots;
        }
        last
    }

    /// What goes into the overflow menu, in order, where the row is too
    /// narrow for the location beside everything else: the plain leading
    /// actions, the last first; then the views (the views menu's place);
    /// then the leading actions that move through the content, the last
    /// first. The location yields to none of them: it stays, elided.
    pub(super) fn yielding(all: Kept, measured: ViewsSize) -> Vec<(Kept, ViewsPlace)> {
        let all = Kept { inline: 0, ..all };
        let views = measured.map_or(ViewsPlace::Overflow, folded_views);
        let plain = (0..=all.plain)
            .rev()
            .map(|plain| (Kept { plain, ..all }, views));
        let navigating = (0..=all.navigating).rev().map(|navigating| {
            let kept = Kept {
                plain: 0,
                navigating,
                inline: 0,
            };
            (kept, ViewsPlace::Overflow)
        });
        plain.chain(navigating).collect()
    }

    /// The slots with the leading actions and the actions `kept` keeps in
    /// the row and the rest in the overflow menu, and the views at `place`;
    /// leading action `n` is `labels[n]` wide where that is wider than an
    /// icon.
    pub(super) fn with_inline(
        toolbar: &WindowToolbar,
        row: Rect,
        kept: Kept,
        place: ViewsPlace,
        labels: &[f64],
    ) -> Self {
        let mut slots = Self::default();
        let y0 = row.center().y - CONTROL_PX / 2.0;
        let square = |x0: f64| Rect::new(x0, y0, x0 + CONTROL_PX, y0 + CONTROL_PX);
        let mut x0 = row.x0;
        if toolbar.sidebar.is_some() {
            slots.sidebar = square(x0);
            x0 = slots.sidebar.x1 + SECTION_GAP;
        }
        if toolbar.navigation.is_some() {
            slots.back = square(x0);
            slots.forward = square(slots.back.x1 + CONTROL_GAP);
            x0 = slots.forward.x1 + SECTION_GAP;
        }
        let mut folded = false;
        let mut any = false;
        for index in 0..toolbar.leading.len() {
            if kept.shows(toolbar, index) {
                let mut rect = square(x0);
                rect.x1 = rect.x1.max(x0 + labels.get(index).copied().unwrap_or(0.0));
                x0 = rect.x1 + CONTROL_GAP;
                slots.leading.push(rect);
                any = true;
            } else {
                slots.leading.push(Rect::ZERO);
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
            slots.overflow = Rect::new(x1 - CONTROL_PX, y0, x1, y0 + CONTROL_PX);
            x1 = slots.overflow.x0 - CONTROL_GAP;
            trailing = true;
        }
        let mut actions = Vec::with_capacity(inline);
        for _ in 0..inline {
            let rect = Rect::new(x1 - CONTROL_PX, y0, x1, y0 + CONTROL_PX);
            x1 = rect.x0 - CONTROL_GAP;
            actions.push(rect);
            trailing = true;
        }
        actions.reverse();
        slots.actions = actions;
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
                slots.views = equal_segments(strip, choices);
                x1 = strip.x0 - SECTION_GAP;
            }
            ViewsPlace::Menu(width) if choices > 0 => {
                slots.views_menu = Rect::new(x1 - width, y0, x1, y0 + CONTROL_PX);
                x1 = slots.views_menu.x0 - SECTION_GAP;
            }
            _ => {}
        }
        if toolbar.location.is_some() && x1 - x0 >= MIN_LOCATION_PX {
            slots.location = Rect::new(x0, y0, x1, y0 + CONTROL_PX);
        }
        if let ViewsPlace::InLocation(least) = place
            && choices > 0
            && slots.location.width() >= least
        {
            slots.views_menu = slots.location;
        }
        slots
    }

    /// Every button stands inside `row`, clear of the ones before it.
    pub(super) fn fits(&self, row: Rect) -> bool {
        let lead = self
            .leading
            .iter()
            .map(|rect| rect.x1)
            .fold(self.forward.x1.max(self.sidebar.x1).max(row.x0), f64::max);
        self.actions
            .iter()
            .chain(self.views.first())
            .chain((self.views_menu.area() > 0.0).then_some(&self.views_menu))
            .chain((self.overflow.area() > 0.0).then_some(&self.overflow))
            .all(|rect| rect.x0 >= lead)
    }

    /// Whether the location is the views' picker.
    pub(crate) fn views_in_location(&self) -> bool {
        self.views_menu.area() > 0.0 && self.views_menu == self.location
    }

    /// The leading actions the row has no room for, in their order.
    pub(super) fn folded_leading(&self) -> impl Iterator<Item = usize> + '_ {
        self.leading
            .iter()
            .enumerate()
            .filter(|(_, rect)| rect.area() <= 0.0)
            .map(|(index, _)| index)
    }

    /// The view switcher's whole strip (`Rect::ZERO`: in the menu).
    #[cfg(test)]
    pub(crate) fn views_strip(&self) -> Rect {
        match (self.views.first(), self.views.last()) {
            (Some(first), Some(last)) => first.union(*last),
            _ => Rect::ZERO,
        }
    }

    pub(super) fn hit(&self, p: Point) -> Option<Slot> {
        if self.sidebar.contains(p) {
            return Some(Slot::Sidebar);
        }
        if self.back.contains(p) {
            return Some(Slot::Back);
        }
        if self.forward.contains(p) {
            return Some(Slot::Forward);
        }
        if let Some(i) = self.leading.iter().position(|r| r.contains(p)) {
            return Some(Slot::Leading(i));
        }
        if let Some(i) = self.views.iter().position(|r| r.contains(p)) {
            return Some(Slot::View(i));
        }
        if self.views_menu.contains(p) {
            return Some(Slot::ViewsMenu);
        }
        if let Some(i) = self.actions.iter().position(|r| r.contains(p)) {
            return Some(Slot::Action(i));
        }
        if let Some(i) = self.notice.iter().position(|r| r.contains(p)) {
            return Some(Slot::Notice(i));
        }
        if self.overflow.contains(p) {
            return Some(Slot::Overflow);
        }
        self.location.contains(p).then_some(Slot::Location)
    }
}

/// `strip` split into `n` equal segments, as a segmented control of
/// labels that all fit splits itself.
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
