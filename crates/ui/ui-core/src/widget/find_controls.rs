//! The find controls a window's bar holds open, described: the field, what it found, its
//! options, replacing where the content allows it, and the steps that widen the search to the
//! platform's own.
//!
//! The controls never search anything themselves. What a window's find acts on and what it
//! found is the app's, read from the window's offer ([`crate::window_toolbar::WindowOffer`]);
//! what the person types, steps, replaces and asks for is sent back to the app with
//! [`crate::window_toolbar::ToolbarEvent`]. In the shell the bar draws these controls and hands
//! their events to the session; an app that frames itself over the same offer may too. One
//! find, one path: the app's.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_ui_base::component::Bound;
use tilo_ui_base::component::Component;
use tilo_ui_base::component::Described;
use tilo_ui_base::component::Event;
use tilo_ui_base::component::Value;
use tilo_ui_base::icons::Icon;
use tilo_ui_base::tokens::Rhythm;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::Decorator;
use tilo_ui_base::widget::Widget;

use super::Button;
use super::ButtonRole;
use super::Container;
use super::ControlSize;
use super::MenuButton;
use super::MenuItem;
use super::Row;
use super::Text;
use super::TextInput;
use super::find_bar::FindAbilities;
use super::find_bar::FindMode;
use super::find_bar::FindOptions;
use super::search_field::FilterCount;
use super::window_search::InPlace;

fn tr(key: &str) -> String {
    tilo_ui_base::i18n::widget_format(key, &[])
}

/// The width a find field reads well at in a bar beside what it found and what steps it: wider
/// than a query usually is, narrow enough to leave the controls their room.
const FIELD_WIDTH: f32 = 240.0;

/// The window's find controls: the bar's field, the match it stands on of how many, the find's
/// options, the replacement where the content allows one, and the steps from this view out to
/// everything.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the find's abilities, the options it runs under and whether it opened to replace are independent facts, and the bar reads each by name"
)]
#[derive(Component, Default)]
#[component(name = "FindControls", role = "Toolbar")]
pub struct FindControlsProps {
    /// The query, edited in the field.
    #[component(bind(merge = "splice"))]
    pub query: Bound<String>,
    /// What to type, shown while the field is empty.
    pub placeholder: Option<String>,
    /// What the window's find acts on, as the window's reading spells it: `document` (its
    /// matches are stepped), `list` (the list narrows), `query` (the app searches itself) or
    /// `nothing`. Only a document's matches are stepped; everything else widens on Enter.
    pub acts_on: Option<String>,
    /// The match the document stands on, counting from one; `0` with none.
    pub current: Option<i64>,
    /// How many it found.
    pub total: Option<i64>,
    /// The content's find offers replacing.
    pub replaces: bool,
    /// And a regular expression.
    pub regex: bool,
    /// And hex: the content is bytes.
    pub hex: bool,
    /// The options the find runs under now: whether it matches case.
    pub match_case: bool,
    /// And whether it matches whole words only.
    pub match_whole_word: bool,
    /// And whether it reads the query as a regular expression.
    pub match_regex: bool,
    /// And whether it reads the query as bytes, in hex.
    pub match_hex: bool,
    /// The window's name, for the step that searches what it shows alone ("Search Release
    /// planning").
    pub window: Option<String>,
    /// The app's name, for the step that searches all of it ("Search Notes").
    pub app: Option<String>,
    /// It opened to replace: the replacement takes the keyboard, as Ctrl+H asks.
    pub replacing: bool,
    /// The query changed as the person typed: what the app's find runs now.
    #[component(event(value = String))]
    pub changed: Event,
    /// Enter: the field's query is asked again, or, where the find acts on no document, the
    /// search widens to the whole app.
    #[component(event(value = String))]
    pub submit: Event,
    /// The person stepped to the next match (`true`) or the one before.
    #[component(event(value = bool))]
    pub step: Event,
    /// The person chose an option: `case`, `whole`, `regex`, `text` or `hex`, to toggle or to
    /// set the mode.
    #[component(event(value = String))]
    pub option: Event,
    /// The person replaced this match with the text carried.
    #[component(event(value = String))]
    pub replace: Event,
    /// The person replaced every match with the text carried.
    #[component(event(value = String))]
    pub replace_all: Event,
    /// The person widened the search: `window` for what the window shows alone, `app` for
    /// everything the app holds, `everywhere` for the platform's search over the workspace.
    #[component(event(value = String))]
    pub widen: Event,
    /// Find (Ctrl+F) was pressed while the field had the keyboard: the search widens one step, as
    /// the bar's own chord always has.
    #[component(event)]
    pub find_chord: Event,
    /// The person closed the find, or let go of the field with Escape.
    #[component(event)]
    pub close: Event,
}

/// Where each control of the row stands, in the order it is built. The parts that only some
/// finds have are absent.
#[derive(Clone, Copy, Debug)]
struct Slots {
    query: usize,
    previous: Option<usize>,
    next: Option<usize>,
    options: usize,
    replacement: Option<usize>,
    replace: Option<usize>,
    replace_all: Option<usize>,
    window: Option<usize>,
    app: Option<usize>,
    everywhere: usize,
    close: usize,
}

/// The find controls: a row of the shared controls, the field's state bound to the view's and
/// everything else read from what the window's find offers.
pub struct FindControls {
    row: Row,
    slots: Slots,
    /// The name the controls stand under, which keys their parts.
    key: String,
    /// What the app found: the match it stands on of how many, as the props last said.
    count: Option<FilterCount>,
    /// The count as the row's label reads it, updated with the props and with what is typed.
    count_text: Rc<RefCell<String>>,
    /// What has been typed into the replacement field, as the buttons read it.
    replacement: Rc<RefCell<String>>,
}

impl FindControls {
    fn child_mut<T: 'static>(&mut self, index: usize) -> Option<&mut T> {
        let child = self.row.children_mut().into_iter().nth(index)?;
        let any = Widget::as_any_mut(child)?;
        any.downcast_mut::<T>()
    }

    /// The field a slot holds, behind the container that keeps it a bar's width.
    fn field_mut(&mut self, index: usize) -> Option<&mut TextInput> {
        let container = self.child_mut::<Container>(index)?;
        let child = container.children_mut().into_iter().next()?;
        let any = Widget::as_any_mut(child)?;
        any.downcast_mut::<TextInput>()
    }

    /// The options menu, for a test that reads what it offers and chooses a row.
    #[cfg(any(test, feature = "test-support"))]
    pub fn options_menu(&mut self) -> Option<&mut MenuButton> {
        self.child_mut::<MenuButton>(self.slots.options)
    }

    /// What the row's count reads for `count`: the match of how many, "No matches" with none,
    /// and nothing while nothing is typed.
    fn count_label(count: Option<FilterCount>, typed: bool) -> String {
        let Some(count) = count.filter(|_| typed) else {
            return String::new();
        };
        if count.total == 0 {
            return tr("find-bar-no-matches");
        }
        tilo_ui_base::i18n::widget_format(
            "find-bar-position",
            &[
                ("current", count.shown.into()),
                ("total", count.total.into()),
            ],
        )
    }

    fn options_items(
        abilities: FindAbilities,
        options: FindOptions,
        option: &Event,
    ) -> Vec<MenuItem> {
        let choose = |bit: &str, label: String, checked: bool| {
            let fired = option.clone();
            let bit = bit.to_owned();
            MenuItem::action(label, move || fired.fire(Some(Value::Text(bit.clone()))))
                .with_checked(checked)
        };
        let mut items = vec![
            choose("case", tr("find-bar-match-case"), options.match_case),
            choose("whole", tr("find-bar-whole-word"), options.whole_word),
        ];
        if abilities.hex {
            items.extend([
                MenuItem::separator(),
                choose("text", tr("find-bar-text"), options.mode != FindMode::Hex),
                choose("hex", tr("find-bar-hex"), options.mode == FindMode::Hex),
            ]);
        } else if abilities.regex {
            items.push(choose(
                "regex",
                tr("find-bar-regex"),
                options.mode == FindMode::Regex,
            ));
        }
        items
    }

    fn app_label(app: Option<&str>) -> String {
        match app.filter(|app| !app.trim().is_empty()) {
            Some(app) => tilo_ui_base::i18n::widget_format(
                "window-search-app",
                &[("app", app.to_owned().into())],
            ),
            None => tr("window-search-this-app"),
        }
    }

    fn window_label(window: Option<&str>) -> String {
        match window.filter(|window| !window.trim().is_empty()) {
            Some(window) => tilo_ui_base::i18n::widget_format(
                "window-search-window",
                &[("window", window.to_owned().into())],
            ),
            None => tr("window-search-this-window"),
        }
    }

    fn count_of(props: &FindControlsProps) -> Option<FilterCount> {
        match (props.current, props.total) {
            (Some(current), Some(total)) => Some(FilterCount::new(
                usize::try_from(current.max(0)).unwrap_or(0),
                usize::try_from(total.max(0)).unwrap_or(0),
            )),
            _ => None,
        }
    }

    fn options_of(props: &FindControlsProps) -> FindOptions {
        FindOptions {
            match_case: props.match_case,
            whole_word: props.match_whole_word,
            mode: if props.match_hex {
                FindMode::Hex
            } else if props.match_regex {
                FindMode::Regex
            } else {
                FindMode::Text
            },
        }
    }

    /// What the find acts on, as the window's reading spells it.
    fn target_of(props: &FindControlsProps) -> InPlace {
        match props.acts_on.as_deref().unwrap_or("document") {
            "nothing" => InPlace::Nothing,
            "list" => InPlace::List,
            "query" => InPlace::Query,
            _ => InPlace::Document,
        }
    }

    fn abilities_of(props: &FindControlsProps) -> FindAbilities {
        FindAbilities {
            replace: props.replaces,
            regex: props.regex,
            hex: props.hex,
        }
    }

    /// Whether the row built stands for what `props` asks of it: the stepper for a document,
    /// the replacement field for a content that can replace, the app's step unless the app's
    /// own query does the searching.
    fn fits(&self, props: &FindControlsProps) -> bool {
        let in_place = Self::target_of(props);
        (in_place == InPlace::Document) == self.slots.previous.is_some()
            && props.replaces == self.slots.replacement.is_some()
            && (in_place != InPlace::Query) == self.slots.app.is_some()
            && (in_place != InPlace::Query) == self.slots.window.is_some()
    }

    /// Build the row for `props`, wiring every control's event.
    #[expect(
        clippy::too_many_lines,
        reason = "one row of controls, and one place that wires every act of it"
    )]
    fn build(&mut self, props: &FindControlsProps) {
        let abilities = Self::abilities_of(props);
        let options = Self::options_of(props);
        let in_place = Self::target_of(props);
        let count = Self::count_of(props);
        let typed = !props.query.state.get().is_empty();
        self.count = count;
        self.count_text.replace(Self::count_label(count, typed));
        self.replacement.replace(String::new());

        let key = self.key.clone();
        let mut row = Row::new().no_wrap().with_rhythm(Rhythm::Tight);
        let mut slots = Slots {
            query: 0,
            previous: None,
            next: None,
            options: 0,
            replacement: None,
            replace: None,
            replace_all: None,
            window: None,
            app: None,
            everywhere: 0,
            close: 0,
        };
        let mut field = TextInput::from_state(&props.query.state).with_id(&format!("{key}.query"));
        if let Some(placeholder) = &props.placeholder {
            field = field.with_placeholder(placeholder.clone());
        }
        field.set_changed_event(props.changed.clone());
        field.set_cancel_event(props.close.clone());
        let back = props.step.clone();
        field.set_on_shift_submit(Some(Box::new(move |_: &str| {
            back.fire(Some(Value::Bool(false)));
        })));
        let chord = props.find_chord.clone();
        field.set_on_find_chord(Some(Box::new(move || chord.fire(None))));
        let submit = props.submit.clone();
        let field = field.on_submit(move |text: &str| {
            submit.fire(Some(Value::Text(text.to_owned())));
        });
        // A field takes its row when it is alone; here it shares one with what it found and
        // what steps it, so it is held to the width a bar's find reads well at.
        row = row.push(Box::new(
            Container::new()
                .with_no_padding()
                .with_width(FIELD_WIDTH)
                .with_child(Box::new(field)),
        ));
        let count_label = Text::dynamic({
            let count = Rc::clone(&self.count_text);
            move || count.borrow().clone()
        })
        .with_role(TextRole::Caption)
        .dimmed()
        .with_id(format!("{key}.count"));
        row = row.push(Box::new(count_label));
        if in_place == InPlace::Document {
            let fired = props.step.clone();
            slots.previous = Some(row.children().len());
            row = row.push(Box::new(
                Button::icon_only(
                    &format!("{key}.previous"),
                    Icon::ChevronUp,
                    &tr("find-bar-previous"),
                )
                .control_size(ControlSize::Small)
                .role(ButtonRole::Plain)
                .on_open(move || fired.fire(Some(Value::Bool(false)))),
            ));
            let fired = props.step.clone();
            slots.next = Some(row.children().len());
            row = row.push(Box::new(
                Button::icon_only(
                    &format!("{key}.next"),
                    Icon::ChevronDown,
                    &tr("find-bar-next"),
                )
                .control_size(ControlSize::Small)
                .role(ButtonRole::Plain)
                .on_open(move || fired.fire(Some(Value::Bool(true)))),
            ));
        }
        slots.options = row.children().len();
        row = row.push(Box::new(
            Button::icon_only(
                &format!("{key}.options"),
                Icon::Settings2,
                &tr("window-search-options"),
            )
            .control_size(ControlSize::Small)
            .role(ButtonRole::Plain)
            .with_menu(Self::options_items(abilities, options, &props.option)),
        ));
        if props.replaces {
            let mut replacement = TextInput::local()
                .with_id(&format!("{key}.replacement"))
                .with_placeholder(tr("find-bar-replace-placeholder"));
            let text = Rc::clone(&self.replacement);
            replacement.set_on_change(move |value: &str| {
                text.replace(value.to_owned());
            });
            slots.replacement = Some(row.children().len());
            row = row.push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_width(FIELD_WIDTH)
                    .with_child(Box::new(replacement)),
            ));
            let fired = props.replace.clone();
            let text = Rc::clone(&self.replacement);
            slots.replace = Some(row.children().len());
            row = row.push(Box::new(
                Button::new(&format!("{key}.replace"), &tr("find-bar-replace"))
                    .control_size(ControlSize::Small)
                    .role(ButtonRole::Plain)
                    .on_open(move || fired.fire(Some(Value::Text(text.borrow().clone())))),
            ));
            let fired = props.replace_all.clone();
            let text = Rc::clone(&self.replacement);
            slots.replace_all = Some(row.children().len());
            row = row.push(Box::new(
                Button::new(&format!("{key}.replace-all"), &tr("find-bar-replace-all"))
                    .control_size(ControlSize::Small)
                    .role(ButtonRole::Plain)
                    .on_open(move || fired.fire(Some(Value::Text(text.borrow().clone())))),
            ));
        }
        if in_place != InPlace::Query {
            slots.window = Some(row.children().len());
            let fired = props.widen.clone();
            row = row.push(Box::new(
                Button::new(
                    &format!("{key}.window"),
                    &Self::window_label(props.window.as_deref()),
                )
                .control_size(ControlSize::Small)
                .role(ButtonRole::Plain)
                .on_open(move || fired.fire(Some(Value::Text("window".to_owned())))),
            ));
            slots.app = Some(row.children().len());
            let fired = props.widen.clone();
            row = row.push(Box::new(
                Button::new(
                    &format!("{key}.app"),
                    &Self::app_label(props.app.as_deref()),
                )
                .control_size(ControlSize::Small)
                .role(ButtonRole::Plain)
                .on_open(move || fired.fire(Some(Value::Text("app".to_owned())))),
            ));
        }
        slots.everywhere = row.children().len();
        let fired = props.widen.clone();
        row = row.push(Box::new(
            Button::new(
                &format!("{key}.everywhere"),
                &tr("window-search-everywhere"),
            )
            .control_size(ControlSize::Small)
            .role(ButtonRole::Plain)
            .on_open(move || fired.fire(Some(Value::Text("everywhere".to_owned())))),
        ));
        slots.close = row.children().len();
        let fired = props.close.clone();
        row = row.push(Box::new(
            Button::icon_only(&format!("{key}.close"), Icon::X, &tr("window-search-close"))
                .control_size(ControlSize::Small)
                .role(ButtonRole::Plain)
                .on_open(move || fired.fire(None)),
        ));
        self.slots = slots;
        self.row = row;
        // Opened to replace, the replacement takes the keyboard; otherwise the query does.
        let focused = if props.replacing {
            self.slots.replacement
        } else {
            Some(self.slots.query)
        };
        if let Some(field) = focused.and_then(|index| self.field_mut(index)) {
            field.request_focus();
        }
    }
}

impl Decorator for FindControls {
    type Inner = Row;

    fn inner(&self) -> &Row {
        &self.row
    }

    fn inner_mut(&mut self) -> &mut Row {
        &mut self.row
    }

    /// The count follows what is typed: the field's own text settles after the props of the frame
    /// that opened it, so it is read here, once a frame, rather than only when the props change.
    fn sync(
        &mut self,
        state: &tilo_ui_base::widget::AppModel,
        path: &tilo_ui_base::widget::ModelPath,
    ) {
        let typed = self
            .field_mut(self.slots.query)
            .is_some_and(|field| !field.value().is_empty());
        let label = Self::count_label(self.count, typed);
        let mut held = self.count_text.borrow_mut();
        if *held != label {
            *held = label;
        }
        drop(held);
        Widget::sync(&mut self.row, state, path);
    }
}

impl Described for FindControls {
    type Props = FindControlsProps;

    fn from_props(key: &str, props: FindControlsProps) -> Self {
        let mut controls = Self {
            row: Row::new(),
            slots: Slots {
                query: 0,
                previous: None,
                next: None,
                options: 0,
                replacement: None,
                replace: None,
                replace_all: None,
                window: None,
                app: None,
                everywhere: 0,
                close: 0,
            },
            key: key.to_owned(),
            count: None,
            count_text: Rc::new(RefCell::new(String::new())),
            replacement: Rc::new(RefCell::new(String::new())),
        };
        controls.build(&props);
        controls
    }
    fn set_props(&mut self, props: FindControlsProps) -> bool {
        if !self.fits(&props) {
            self.build(&props);
            return true;
        }
        let abilities = Self::abilities_of(&props);
        let options = Self::options_of(&props);
        let count = Self::count_of(&props);
        let typed = !props.query.state.get().is_empty();
        // A find the app ran on its own put a query in the bar's field: it shows there.
        let text = props.query.state.get();
        if let Some(field) = self.field_mut(self.slots.query) {
            if field.value() != text {
                field.set_text(&text);
            }
            let back = props.step.clone();
            field.set_on_shift_submit(Some(Box::new(move |_: &str| {
                back.fire(Some(Value::Bool(false)));
            })));
            let chord = props.find_chord.clone();
            field.set_on_find_chord(Some(Box::new(move || chord.fire(None))));
        }
        self.count = count;
        self.count_text.replace(Self::count_label(count, typed));
        if let Some(menu) = self.child_mut::<MenuButton>(self.slots.options) {
            menu.set_items(Self::options_items(abilities, options, &props.option));
        }
        let found = count.is_none_or(|count| count.total > 0);
        if let Some(previous) = self.slots.previous
            && let Some(button) = self.child_mut::<Button>(previous)
        {
            button.set_enabled(found);
        }
        if let Some(next) = self.slots.next
            && let Some(button) = self.child_mut::<Button>(next)
        {
            button.set_enabled(found);
        }
        true
    }
}
