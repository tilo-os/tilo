//! The primary bar's compressed command row. Shared Toolbar owns measurement,
//! overflow, press/release, keyboard focus and its menu; the bar owns intents.
use super::{
    Affine, AppModel, AppTrayRequest, AppTrayStatus, CacheFingerprint, CenterWidget, ChromeIntent,
    ModelPath, PresenceCtx, Rect, Scene, TextRenderer, TextRole, Theme, TopBarChrome, Widget,
    pointer_coordinates, t, t_args,
};
use std::cell::RefCell;
use std::rc::Rc;
use ui::icons::Icon;
use ui::widget::Toolbar;
use ui::widget::toolbar::Orientation;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Command {
    Control(&'static str),
    Tray { key: String, menu: bool },
}

#[derive(PartialEq)]
pub(super) struct Spec {
    pub command: Command,
    pub name: String,
    pub icon: Icon,
    pub priority: i32,
    pub enabled: bool,
    pub active: Option<bool>,
    pub dot: bool,
}

pub(super) struct ResponsiveBar {
    pub active: bool,
    pub toolbar: Toolbar,
    pub theme: Theme,
    pub reading_rect: Rect,
    pub dirty: bool,
    reading: String,
    commands: Vec<Command>,
    icons: Vec<Icon>,
    specs: Vec<Spec>,
    requests: Rc<RefCell<Vec<Command>>>,
}

impl ResponsiveBar {
    pub fn new() -> Self {
        Self {
            active: false,
            toolbar: Toolbar::new(Orientation::Horizontal),
            theme: Theme::dark(),
            reading_rect: Rect::ZERO,
            dirty: true,
            reading: String::new(),
            commands: Vec::new(),
            icons: Vec::new(),
            specs: Vec::new(),
            requests: Rc::default(),
        }
    }

    pub fn configure(&mut self, specs: Vec<Spec>) {
        let commands: Vec<_> = specs.iter().map(|spec| spec.command.clone()).collect();
        let icons: Vec<_> = specs.iter().map(|spec| spec.icon).collect();
        let rebuilt = commands != self.commands || icons != self.icons;
        self.dirty |= self.specs != specs;
        if rebuilt {
            self.toolbar.close_menus();
            let mut toolbar = Toolbar::new(Orientation::Horizontal);
            for spec in &specs {
                let request = Rc::clone(&self.requests);
                let command = spec.command.clone();
                let id = match &command {
                    Command::Control(target) => (*target).to_owned(),
                    Command::Tray { key, menu } => format!("topbar.tray:{key}:{menu}"),
                };
                let callback = move || request.borrow_mut().push(command.clone());
                toolbar = if let Some(active) = spec.active {
                    toolbar.push_icon_toggle(spec.icon, &spec.name, active, callback)
                } else {
                    toolbar.push_icon(spec.icon, &spec.name, callback)
                }
                .with_priority(spec.priority)
                .with_enabled(spec.enabled)
                .with_view(&id);
            }
            self.toolbar = toolbar;
            self.commands = commands;
            self.icons = icons;
            self.dirty = true;
        }
        for (index, spec) in specs.iter().enumerate() {
            let previous = if rebuilt { None } else { self.specs.get(index) };
            if previous.is_none_or(|old| old.name != spec.name) {
                self.toolbar.set_label(index, &spec.name);
                self.toolbar.set_accessible_name(index, &spec.name);
            }
            self.toolbar.set_enabled(index, spec.enabled);
            if let Some(active) = spec.active
                && previous.is_none_or(|old| old.active != spec.active)
            {
                self.toolbar.set_active(index, active);
            }
            self.toolbar.set_dot(index, spec.dot);
        }
        self.specs = specs;
    }

    pub fn paint_focus_state(&self) -> Vec<bool> {
        fn collect(node: &ui::a11y::AccessNode, state: &mut Vec<bool>) {
            state.push(node.state.focused);
            for child in &node.children {
                collect(child, state);
            }
        }
        if !self.active {
            return Vec::new();
        }
        let focused = self.toolbar.is_focused();
        let mut state = vec![focused, self.toolbar.is_overflow_open()];
        if focused && let Some(node) = self.toolbar.accessibility() {
            collect(&node, &mut state);
        }
        state
    }

    pub fn rect_for(&self, target: &str) -> Rect {
        self.commands
            .iter()
            .position(|command| matches!(command, Command::Control(control) if *control == target))
            .and_then(|index| self.toolbar.item_rect(index))
            .unwrap_or(Rect::ZERO)
    }

    pub fn sync(&mut self, state: &AppModel, path: &ModelPath) -> Vec<Command> {
        let open = self.toolbar.is_overflow_open();
        self.toolbar.sync(state, path);
        self.dirty |= open != self.toolbar.is_overflow_open();
        self.dirty |= self.toolbar.take_activated().is_some();
        let requests = std::mem::take(&mut *self.requests.borrow_mut());
        self.dirty |= !requests.is_empty();
        requests
    }
}

impl TopBarChrome {
    pub(super) const CONTROL_WIDGET_EDITOR: &'static str = "topbar:widget-editor";

    pub(super) fn use_responsive(&self, theme: &Theme) -> bool {
        !self.rect.is_zero_area()
            && !self.is_compact()
            && self.rect.width()
                < self.center_band_width(theme)
                    + f64::from(theme.tokens.metrics.min_pointer_target) * 7.0
                    + f64::from(theme.tokens.spacing.s3) * 4.0
    }

    pub(super) fn responsive_specs(&self) -> Vec<Spec> {
        let mut targets = vec![
            Self::CONTROL_OVERVIEW,
            Self::CONTROL_WORKSPACE_UP,
            Self::CONTROL_WORKSPACE_DOWN,
            Self::CONTROL_WORKSPACE_SETTINGS,
        ];
        targets.extend(
            self.widget_config
                .widgets
                .iter()
                .copied()
                .map(Self::center_control),
        );
        targets.push(Self::CONTROL_QUICK_SETTINGS);
        if self.collaborators_visible() || self.invite_visible() || self.sharing_label().is_some() {
            targets.push(Self::CONTROL_COLLABORATORS);
        }
        if !self.status.media.is_empty() {
            targets.push(Self::CONTROL_MEDIA);
        }
        if self.continuity_label().is_some() {
            targets.push(Self::CONTROL_CONTINUITY);
            targets.push(Self::CONTROL_CONTINUITY_DISMISS);
        }
        targets.push(Self::CONTROL_WIDGET_EDITOR);
        let offline = self
            .status
            .link
            .as_ref()
            .is_some_and(|link| link.kind == crate::chrome::LinkKind::Offline);
        let mut specs: Vec<_> = targets
            .into_iter()
            .map(|target| self.responsive_control_spec(target, offline))
            .collect();
        for item in self
            .chrome
            .as_ref()
            .map(|chrome| chrome.app_tray())
            .unwrap_or_default()
            .into_iter()
            .filter(|item| item.status != AppTrayStatus::Passive)
        {
            for menu in [false, true]
                .into_iter()
                .filter(|menu| *menu || !item.item_is_menu)
            {
                specs.push(Spec {
                    command: Command::Tray {
                        key: item.key.clone(),
                        menu: menu || item.item_is_menu,
                    },
                    name: t_args(
                        if menu {
                            "topbar-tray-menu"
                        } else {
                            "topbar-tray-open"
                        },
                        &[("app", item.id.as_str().into())],
                    ),
                    icon: Icon::AppWindow,
                    priority: 0,
                    enabled: true,
                    active: None,
                    dot: item.status == AppTrayStatus::NeedsAttention,
                });
                if item.item_is_menu {
                    break;
                }
            }
        }
        specs
    }

    fn responsive_control_spec(&self, target: &'static str, offline: bool) -> Spec {
        let (icon, priority) = match target {
            Self::CONTROL_OVERVIEW => (Self::overview_glyph_icon(), 100),
            Self::CONTROL_WORKSPACE_UP => (Icon::ChevronUp, 10),
            Self::CONTROL_WORKSPACE_DOWN => (Icon::ChevronDown, 10),
            Self::CONTROL_WORKSPACE_SETTINGS => (Icon::Settings, 90),
            Self::CONTROL_CENTER_CLOCK => (Icon::Clock, 70),
            Self::CONTROL_CENTER_MONITOR => (Icon::Monitor, 20),
            Self::CONTROL_CENTER_CALENDAR => (Icon::Calendar, 20),
            Self::CONTROL_QUICK_SETTINGS if offline => (Icon::WifiOff, 110),
            Self::CONTROL_QUICK_SETTINGS => (Icon::Settings2, 110),
            Self::CONTROL_COLLABORATORS if self.sharing_label().is_some() => (Icon::Monitor, 120),
            Self::CONTROL_COLLABORATORS => (Icon::Users, 60),
            Self::CONTROL_MEDIA => (Icon::Mic, 130),
            Self::CONTROL_CONTINUITY => (Icon::ArrowRight, 30),
            Self::CONTROL_CONTINUITY_DISMISS => (Icon::X, 10),
            _ => (Icon::Pencil, 0),
        };
        let name = if target == Self::CONTROL_QUICK_SETTINGS && offline {
            format!(
                "{} · {}",
                self.control_name(target),
                t("topbar-network-offline")
            )
        } else {
            self.control_name(target)
        };
        let active = match target {
            Self::CONTROL_CENTER_CLOCK => Some(self.center_widget == CenterWidget::Clock),
            Self::CONTROL_CENTER_MONITOR => Some(self.center_widget == CenterWidget::SystemMonitor),
            Self::CONTROL_CENTER_CALENDAR => Some(self.center_widget == CenterWidget::Calendar),
            _ => None,
        };
        Spec {
            command: Command::Control(target),
            name,
            icon,
            priority,
            enabled: !self.control_is_disabled(target),
            active,
            dot: (target == Self::CONTROL_QUICK_SETTINGS && self.notification_badge > 0)
                || (target == Self::CONTROL_COLLABORATORS && self.activity_unread > 0),
        }
    }

    pub(super) fn responsive_reading(&self) -> String {
        match self.center_widget {
            CenterWidget::Clock => self.live_clock().clock,
            CenterWidget::SystemMonitor => t_args(
                "topbar-cpu-reading",
                &[(
                    "value",
                    self.telemetry
                        .cpu_percent
                        .map_or_else(|| "—".into(), |value| format!("{value}%"))
                        .into(),
                )],
            ),
            CenterWidget::Calendar => {
                let date = self.live_clock().date;
                if date.is_empty() { "—".into() } else { date }
            }
        }
    }

    pub(super) fn responsive_description(&self) -> String {
        match self.center_widget {
            CenterWidget::Clock => {
                let clock = self.live_clock();
                format!("{} {}", clock.weekday, clock.date)
                    .trim()
                    .to_owned()
            }
            CenterWidget::SystemMonitor => t_args(
                "topbar-monitor-summary",
                &[
                    (
                        "cpu",
                        self.telemetry
                            .cpu_percent
                            .map_or_else(|| "—".into(), |value| format!("{value}%"))
                            .into(),
                    ),
                    (
                        "ram",
                        self.telemetry
                            .ram_percent
                            .map_or_else(|| "—".into(), |value| format!("{value}%"))
                            .into(),
                    ),
                    (
                        "temperature",
                        self.telemetry
                            .temperature_celsius
                            .map_or_else(|| "—".into(), |value| format!("{value}°"))
                            .into(),
                    ),
                ],
            ),
            CenterWidget::Calendar => t("topbar-calendar-none"),
        }
    }

    pub(super) fn prepare_responsive(&mut self, theme: &Theme, text: &mut TextRenderer) {
        let active = self.use_responsive(theme);
        if active != self.responsive.active {
            self.responsive.toolbar.close_menus();
            self.responsive.dirty = true;
            self.cache_fp = CacheFingerprint::default();
        }
        self.responsive.active = active;
        self.responsive.theme = theme.clone();
        if !active {
            self.responsive.reading_rect = Rect::ZERO;
            self.responsive.toolbar.lay_out_in(Rect::ZERO, text, theme);
            return;
        }
        self.center_rect = self.rect;
        self.responsive.configure(self.responsive_specs());
        let style = theme.tokens.typography.style(TextRole::Caption);
        let inset = f64::from(theme.tokens.spacing.s2);
        let gap = f64::from(theme.tokens.spacing.s3);
        let reading_budget = (self.rect.width()
            - 2.0 * f64::from(theme.tokens.metrics.min_pointer_target)
            - 4.0 * gap
            - 2.0 * inset)
            .max(0.0);
        self.responsive.reading = text.elide_to_width_style(
            &self.responsive_reading(),
            ui::num::f32_of_f64(reading_budget),
            &style,
        );
        let (clock_width, _) = text.measure_style(&self.responsive.reading, None, &style);
        let right = self.rect.x1 - inset;
        let clock_left = (right - f64::from(clock_width)).max(self.rect.x0);
        self.responsive.reading_rect = Rect::new(clock_left, self.rect.y0, right, self.rect.y1);
        let row = Rect::new(
            self.rect.x0 + inset,
            self.rect.y0,
            (clock_left - gap).max(self.rect.x0 + inset),
            self.rect.y1,
        );
        self.responsive.toolbar.lay_out_in(row, text, theme);
    }

    pub(super) fn draw_responsive(
        &mut self,
        scene: &mut Scene,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.prepare_responsive(theme, text);
        self.responsive
            .toolbar
            .draw(scene, Affine::IDENTITY, text, theme, presence);
        let style = theme.tokens.typography.style(TextRole::Caption);
        let (_, height) = text.measure_style(&self.responsive.reading, None, &style);
        let rect = self.responsive.reading_rect;
        text.draw_style(
            scene,
            &self.responsive.reading,
            theme.tokens.colors.text_primary,
            Affine::translate((rect.x0, rect.center().y - f64::from(height) / 2.0)),
            None,
            &style,
        );
    }

    pub(super) fn consume_responsive(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let requests = self.responsive.sync(state, path);
        for command in requests {
            match command {
                Command::Control(target) => {
                    self.activate_control(target);
                }
                Command::Tray { key, menu } => {
                    let point = self
                        .last_cursor
                        .unwrap_or_else(|| self.responsive.toolbar.get_rect().center());
                    let (x, y) = pointer_coordinates(point);
                    if let Some(chrome) = &self.chrome {
                        chrome.intent(ChromeIntent::AppTray {
                            key,
                            request: if menu {
                                AppTrayRequest::ContextMenu { x, y }
                            } else {
                                AppTrayRequest::Activate { x, y }
                            },
                        });
                    }
                }
            }
        }
        self.responsive.active && self.responsive.dirty
    }
}
