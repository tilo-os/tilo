"""Types for Settings (`so.tilo.settings`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Root` is the app's `settings.root` at `settings/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers a `Run`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is.
"""

from __future__ import annotations

from typing import Any, Literal, TypedDict
from tilo import Resource, Run

__all__ = [
    "ColorFilter",
    "MagnifierView",
    "OnScreenKeyboard",
    "ScreenReaderVoice",
    "SwitchKeyPair",
    "TouchpadClickMethod",
    "TouchpadScrollMethod",
    "KeyboardProfile",
    "SourceOptions",
    "PowerActionPref",
    "Place",
    "ShowMode",
    "WidgetPrefs",
    "AppNotificationRule",
    "FocusSchedule",
    "SystemSoundEvents",
    "SystemSoundPrefs",
    "Currency",
    "DateStyle",
    "FirstDayOfWeek",
    "Measurement",
    "NumberStyle",
    "TimeStyle",
    "AuditEntry",
    "ContinuityCapability",
    "Binding",
    "ProviderConfig",
    "HostedProfiles",
    "Bytes",
    "ConnectionKind",
    "ConnectionUsage",
    "Reading",
    "Restart",
    "PersonAccount",
    "Root",
    "Appearance",
    "AppearanceDevice",
    "Accessibility",
    "AccessibilityDevice",
    "Input",
    "InputDevice",
    "TextAssistance",
    "Power",
    "DeskWidgets",
    "SearchHistory",
    "Updates",
    "Notifications",
    "Audio",
    "AudioDevice",
    "Display",
    "NightLight",
    "Locale",
    "Continuity",
    "Shortcuts",
    "ShellViews",
    "DesktopControls",
    "DesktopControlsDevice",
    "AgentProviders",
    "HostedAgentProviders",
    "DefaultApps",
    "Clock",
    "DisplayArrangement",
    "AutomaticDisplay",
    "DisplayChange",
    "OskSources",
    "DeviceVpn",
    "Identity",
    "NetworkUsage",
    "AgentDoor",
    "Accounts",
]

ColorFilter = Literal["none", "grayscale", "inverted", "grayscale_inverted"]

MagnifierView = Literal["lens", "full_screen"]

OnScreenKeyboard = Literal["auto", "always", "never"]

ScreenReaderVoice = Literal["system", "english_us", "english_gb", "german", "french", "spanish"]

SwitchKeyPair = Literal["f6_f7", "f9_f10", "digits12"]

TouchpadClickMethod = Literal["areas", "clickfinger", "default"]

TouchpadScrollMethod = Literal["edge", "two_finger"]

class KeyboardProfile(TypedDict):
    sources: list[str]
    active: str
    options: dict[str, SourceOptions]

class SourceOptions(TypedDict):
    model: str | None
    compose_key: str | None
    caps_lock: str | None
    customised: bool

PowerActionPref = Literal["suspend", "lock", "ignore"]

class Place(TypedDict):
    display: str
    x: float
    y: float

ShowMode = Literal["pause", "always", "ask"]

class WidgetPrefs(TypedDict):
    enabled: bool
    show: ShowMode
    always_on_strips: list[int]
    places: list[Place]
    width: float | None

class AppNotificationRule(TypedDict):
    banners: bool | None
    sounds: bool | None
    badges: bool | None
    previews: bool | None
    lock_screen: bool | None
    cross_device: bool | None
    priority: bool
    priority_contacts: bool
    muted_until: int | None

class FocusSchedule(TypedDict):
    enabled: bool
    start_minute: int
    end_minute: int
    weekdays: int
    allow_priority: bool

class SystemSoundEvents(TypedDict):
    notification: bool
    message: bool
    volume_change: bool
    screenshot: bool
    device_connected: bool
    device_disconnected: bool
    low_battery: bool
    error: bool

class SystemSoundPrefs(TypedDict):
    enabled: bool
    volume: float
    theme: str | None
    events: SystemSoundEvents

Currency = Literal["USD", "EUR", "GBP", "JPY", "CHF", "CAD", "AUD", "CNY", "INR"]

DateStyle = Literal["Language", "ISO", "US", "EU", "Long"]

FirstDayOfWeek = Literal["monday", "sunday", "saturday"]

Measurement = Literal["metric", "imperial"]

NumberStyle = Literal["decimal_point", "decimal_comma"]

TimeStyle = Literal["Language", "H24", "H12"]

class AuditEntry(TypedDict):
    recorded_at_secs: int
    capability: ContinuityCapability | None
    enabled: bool

ContinuityCapability = Literal["clipboard", "notifications", "wifi_settings", "media", "display_input", "file_transfer", "realtime_sessions", "typed_gamepad", "app_continuations"]

class Binding(TypedDict):
    app: str
    view: str

class ProviderConfig(TypedDict):
    profiles: list[Any]
    default_profile: str | None
    agent_models: dict[str, Any]
    interaction_models: dict[str, Any]

class HostedProfiles(TypedDict):
    profiles: list[Any]

class Bytes(TypedDict):
    received: int
    sent: int

ConnectionKind = Literal["wifi", "wired", "mobile"]

class ConnectionUsage(TypedDict):
    kind: Any
    name: str
    days: dict[str, Bytes]
    restart: Restart | None

class Reading(TypedDict):
    counter: str
    connection: str
    kind: ConnectionKind
    name: str
    received: int
    sent: int

class Restart(TypedDict):
    day: str
    before: int

class PersonAccount(TypedDict):
    type_id: str
    display_name: str
    config: Any
    enabled: bool
    tint: int

class Root(Resource):
    """Settings — `settings.root`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    def inspect_input_sources(self,
        *,
        query: str | None = None,
        page: int | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if query is not None:
            args["query"] = query
        if page is not None:
            args["page"] = page
        return self.action("inspect-input-sources", **args)

    def inspect_keyboard_map(self) -> None:
        """Runs the app's action."""
        return self.action("inspect-keyboard-map")

    def inspect_keyboard_sources(self) -> None:
        """Runs the app's action."""
        return self.action("inspect-keyboard-sources")

    def storage_summary(self) -> None:
        """Runs the app's action."""
        return self.action("storage-summary")

    def search_hub_models(self,
        *,
        query: str,
        format: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["query"] = query
        if format is not None:
            args["format"] = format
        return self.action("search-hub-models", **args)

    def show_hub_model(self, *, model: str) -> None:
        """Runs the app's action."""
        return self.action("show-hub-model", model=model)

    def discover_thunderbird_accounts(self, *, prefs: str) -> None:
        """Runs the app's action."""
        return self.action("discover-thunderbird-accounts", prefs=prefs)

    def choose_thunderbird_metadata(self) -> None:
        """Runs the app's action."""
        return self.action("choose-thunderbird-metadata")

    def inspect_thunderbird_import(self) -> None:
        """Runs the app's action."""
        return self.action("inspect-thunderbird-import")

    def inspect_calendar_choices(self) -> None:
        """Runs the app's action."""
        return self.action("inspect-calendar-choices")


class Appearance(Resource):
    """Appearance — `settings.appearance`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    mode: str
    dark_theme: str
    light_theme: str
    @property
    def custom_themes(self) -> list[Any]:
        """Read only."""
        return self.fields["custom_themes"]
    density: float
    high_contrast: bool
    reduce_motion: bool
    reduce_transparency: bool
    text_scale: float
    ui_scale: float
    schedule: str
    sunrise: str
    sunset: str
    wallpaper_scope: str
    wallpaper_target: str
    wallpaper: str | None
    forced_colors: bool
    distinct_letters: bool | None
    language: str | None
    ui_face: str | None
    accent: str | None
    translucency: float | None

    def choose_theme(self, *, theme: str) -> Run | None:
        """Runs the app's action."""
        return self.action("choose-theme", theme=theme)

    def set_dark_mode(self, *, on: bool) -> Run | None:
        """Runs the app's action."""
        return self.action("set-dark-mode", on=on)


class AppearanceDevice(Resource):
    """Appearance on this device — `settings.appearance-device`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    @property
    def distribution_defaults(self) -> Any | None:
        """Read only."""
        return self.fields["distribution_defaults"]


class Accessibility(Resource):
    """Accessibility — `settings.accessibility`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    large_cursor: bool
    cursor_size: int
    cursor_theme: str
    @property
    def screen_reader(self) -> bool:
        """Read only."""
        return self.fields["screen_reader"]
    on_screen_keyboard: OnScreenKeyboard
    sticky_keys: bool
    slow_keys: bool
    bounce_keys: bool
    bounce_keys_delay_ms: int
    bounce_keys_alert: bool
    mouse_keys: bool
    visual_alerts: bool
    color_filter: ColorFilter
    magnifier_zoom: int
    magnifier_view: MagnifierView
    @property
    def magnifier_last_zoom(self) -> int:
        """Read only."""
        return self.fields["magnifier_last_zoom"]
    cursor_crosshair: bool
    thick_text_caret: bool
    switch_control: bool
    switch_key_pair: SwitchKeyPair
    switch_scan_ms: int
    screen_reader_rate: int
    screen_reader_voice: ScreenReaderVoice
    dictation_language: Any
    dictation_literal: bool

    def step_magnifier(self, *, step: str) -> Run | None:
        """Runs the app's action."""
        return self.action("step-magnifier", step=step)

    def set_screen_reader(self, *, on: bool) -> Run | None:
        """Runs the app's action."""
        return self.action("set-screen-reader", on=on)


class AccessibilityDevice(Resource):
    """Accessibility on this device — `settings.accessibility-device`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    apply_to_sign_in: bool


class Input(Resource):
    """Input — `settings.input`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    touchpad_natural_scroll: bool
    mouse_natural_scroll: bool
    tap_to_click: bool
    pointer_speed: float
    touchpad_pointer_speed: float | None
    mouse_pointer_speed: float | None
    key_repeat: float
    @property
    def key_repeat_delay_ms(self) -> int | None:
        """Read only."""
        return self.fields["key_repeat_delay_ms"]
    @property
    def key_repeat_rate_hz(self) -> int | None:
        """Read only."""
        return self.fields["key_repeat_rate_hz"]
    @property
    def keyboard_layout(self) -> str:
        """Read only."""
        return self.fields["keyboard_layout"]
    tap_drag: bool
    tap_drag_lock: bool
    disable_while_typing: bool
    middle_emulation: bool
    left_handed: bool
    accel_profile: str
    touchpad_gestures: bool
    touchpad_click_method: TouchpadClickMethod
    touchpad_scroll_method: TouchpadScrollMethod
    scroll_speed: float
    @property
    def compose_key(self) -> str | None:
        """Read only."""
        return self.fields["compose_key"]
    @property
    def caps_lock(self) -> str | None:
        """Read only."""
        return self.fields["caps_lock"]
    @property
    def keyboard_layouts(self) -> list[str]:
        """Read only."""
        return self.fields["keyboard_layouts"]
    @property
    def paste_shortcuts(self) -> dict[str, str]:
        """Read only."""
        return self.fields["paste_shortcuts"]

    def set_key_repeat(self, *, rate: float) -> Run | None:
        """Runs the app's action."""
        return self.action("set-key-repeat", rate=rate)

    def choose_layout(self, *, layout: str) -> Run | None:
        """Runs the app's action."""
        return self.action("choose-layout", layout=layout)

    def set_layouts(self,
        *,
        layouts: list[str],
        active: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-layouts", layouts=layouts, active=active)

    def set_paste_shortcut(self,
        *,
        app: str,
        shortcut: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-paste-shortcut", app=app, shortcut=shortcut)


class InputDevice(Resource):
    """Input on this device — `settings.input-device`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    touchpad_enabled: bool
    touchpad_disabled_on_external_mouse: bool
    @property
    def keyboard_model(self) -> str:
        """Read only."""
        return self.fields["keyboard_model"]
    @property
    def keyboard_assignments(self) -> dict[str, KeyboardProfile]:
        """Read only."""
        return self.fields["keyboard_assignments"]
    @property
    def keyboard_preview(self) -> str | None:
        """Read only."""
        return self.fields["keyboard_preview"]
    @property
    def keyboard_preview_extra_levels(self) -> bool:
        """Read only."""
        return self.fields["keyboard_preview_extra_levels"]
    @property
    def keyboard_preview_level(self) -> int:
        """Read only."""
        return self.fields["keyboard_preview_level"]
    @property
    def keyboard_inspected_key(self) -> str:
        """Read only."""
        return self.fields["keyboard_inspected_key"]
    @property
    def tablets(self) -> dict[str, Any]:
        """Read only."""
        return self.fields["tablets"]

    def set_keyboard_layouts(self,
        *,
        keyboard: str,
        profile: KeyboardProfile,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-keyboard-layouts", keyboard=keyboard, profile=profile)

    def use_shared_layouts(self, *, keyboard: str) -> Run | None:
        """Runs the app's action."""
        return self.action("use-shared-layouts", keyboard=keyboard)

    def set_keyboard_model(self, *, model: str) -> Run | None:
        """Runs the app's action."""
        return self.action("set-keyboard-model", model=model)

    def show_keyboard_layout(self,
        *,
        layout: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if layout is not None:
            args["layout"] = layout
        return self.action("show-keyboard-layout", **args)

    def inspect_keyboard(self,
        *,
        level: int,
        extra_levels: bool,
        key: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("inspect-keyboard", level=level, extra_levels=extra_levels, key=key)

    def set_tablet_output(self,
        *,
        device: str,
        output: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-tablet-output", device=device, output=output)

    def set_tablet_keep_aspect(self,
        *,
        device: str,
        on: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-tablet-keep-aspect", device=device, on=on)

    def set_tablet_left_handed(self,
        *,
        device: str,
        on: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-tablet-left-handed", device=device, on=on)

    def set_tablet_tip_feel(self,
        *,
        device: str,
        feel: float,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-tablet-tip-feel", device=device, feel=feel)

    def set_tablet_button(self,
        *,
        device: str,
        button: str,
        action: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-tablet-button", device=device, button=button, action=action)

    def set_tablet_eraser(self,
        *,
        device: str,
        action: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-tablet-eraser", device=device, action=action)

    def set_pad_button(self,
        *,
        device: str,
        button: int,
        action: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-pad-button", device=device, button=button, action=action)

    def set_pad_dial(self,
        *,
        device: str,
        dial: str,
        index: int,
        mode: int,
        action: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-pad-dial", device=device, dial=dial, index=index, mode=mode, action=action)


class TextAssistance(Resource):
    """Spelling and text — `settings.text-assistance`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    check_spelling: bool
    auto_correct: bool
    spelling_language: str


class Power(Resource):
    """Power — `settings.power`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    auto_suspend: bool
    suspend_minutes: int
    battery_suspend_minutes: int | None
    saver_suspend_minutes: int | None
    balanced_suspend_minutes: int | None
    performance_suspend_minutes: int | None
    blank_minutes: int
    lock_minutes: int
    lock_on_sleep: bool
    lid_close_action: PowerActionPref
    power_key_action: PowerActionPref
    low_battery_percent: int
    critical_battery_percent: int
    critical_action: PowerActionPref

    def set_suspend_plugged_in(self, *, minutes: int) -> Run | None:
        """Runs the app's action."""
        return self.action("set-suspend-plugged-in", minutes=minutes)

    def set_suspend_on_battery(self, *, minutes: int) -> Run | None:
        """Runs the app's action."""
        return self.action("set-suspend-on-battery", minutes=minutes)


class DeskWidgets(Resource):
    """Desk widgets — `settings.desk_widgets`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    music: WidgetPrefs
    team: WidgetPrefs
    calendar: WidgetPrefs
    system_monitor: WidgetPrefs


class SearchHistory(Resource):
    """Search history — `settings.search_history`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    chosen: str
    chosen_for_text: str
    places: Any


class Updates(Resource):
    """Updates — `settings.updates`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    @property
    def auto(self) -> bool:
        """Read only."""
        return self.fields["auto"]
    @property
    def last_checked(self) -> str:
        """Read only."""
        return self.fields["last_checked"]

    def record_automatic_updates(self, *, on: bool) -> Run | None:
        """Runs the app's action."""
        return self.action("record-automatic-updates", on=on)

    def record_update_check(self, *, at: str) -> Run | None:
        """Runs the app's action."""
        return self.action("record-update-check", at=at)


class Notifications(Resource):
    """Notifications — `settings.notifications`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    do_not_disturb: bool
    show_banners: bool
    show_previews: bool
    play_sounds: bool
    show_on_lock_screen: bool
    show_badges: bool
    focus: FocusSchedule
    per_app: dict[str, AppNotificationRule]

    def set_do_not_disturb(self, *, on: bool) -> Run | None:
        """Runs the app's action."""
        return self.action("set-do-not-disturb", on=on)

    def mute_app(self,
        *,
        app: str,
        duration: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("mute-app", app=app, duration=duration)


class Audio(Resource):
    """Sound — `settings.audio`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    system_sounds: SystemSoundPrefs
    balance: float
    @property
    def mono_output(self) -> bool:
        """Read only."""
        return self.fields["mono_output"]
    @property
    def echo_cancellation(self) -> bool:
        """Read only."""
        return self.fields["echo_cancellation"]

    def set_mono_output(self, *, on: bool) -> Run | None:
        """Runs the app's action."""
        return self.action("set-mono-output", on=on)

    def set_echo_cancellation(self, *, on: bool) -> Run | None:
        """Runs the app's action."""
        return self.action("set-echo-cancellation", on=on)


class AudioDevice(Resource):
    """Sound on this device — `settings.audio-device`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    @property
    def output_device(self) -> str | None:
        """Read only."""
        return self.fields["output_device"]
    @property
    def input_device(self) -> str | None:
        """Read only."""
        return self.fields["input_device"]

    def remember_output_device(self, *, device: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remember-output-device", device=device)

    def remember_input_device(self, *, device: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remember-input-device", device=device)


class Display(Resource):
    """Displays — `settings.display`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    @property
    def auto_brightness(self) -> bool:
        """Read only."""
        return self.fields["auto_brightness"]
    @property
    def rotation_lock(self) -> bool:
        """Read only."""
        return self.fields["rotation_lock"]
    scale: float

    def set_automatic_brightness(self, *, on: bool) -> Run | None:
        """Runs the app's action."""
        return self.action("set-automatic-brightness", on=on)

    def set_rotation_lock(self, *, locked: bool) -> Run | None:
        """Runs the app's action."""
        return self.action("set-rotation-lock", locked=locked)


class NightLight(Resource):
    """Night light — `settings.night-light`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    night_light: bool
    night_light_temperature: int
    night_light_schedule: bool


class Locale(Resource):
    """Region and language — `settings.locale`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    currency: Currency
    date_format: DateStyle
    time_format: TimeStyle
    measurement: Measurement
    number_style: NumberStyle
    first_day_of_week: FirstDayOfWeek


class Continuity(Resource):
    """Continuity — `settings.continuity`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    @property
    def enabled(self) -> bool:
        """Read only."""
        return self.fields["enabled"]
    @property
    def clipboard(self) -> bool:
        """Read only."""
        return self.fields["clipboard"]
    @property
    def notifications(self) -> bool:
        """Read only."""
        return self.fields["notifications"]
    @property
    def wifi_settings(self) -> bool:
        """Read only."""
        return self.fields["wifi_settings"]
    @property
    def media(self) -> bool:
        """Read only."""
        return self.fields["media"]
    @property
    def display_input(self) -> bool:
        """Read only."""
        return self.fields["display_input"]
    @property
    def file_transfer(self) -> bool:
        """Read only."""
        return self.fields["file_transfer"]
    @property
    def realtime_sessions(self) -> bool:
        """Read only."""
        return self.fields["realtime_sessions"]
    @property
    def typed_gamepad(self) -> bool:
        """Read only."""
        return self.fields["typed_gamepad"]
    @property
    def app_continuations(self) -> bool:
        """Read only."""
        return self.fields["app_continuations"]
    @property
    def audit(self) -> list[AuditEntry]:
        """Read only."""
        return self.fields["audit"]

    def allow_continuity(self, *, on: bool) -> Run | None:
        """Runs the app's action."""
        return self.action("allow-continuity", on=on)

    def allow_capability(self,
        *,
        capability: str,
        on: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("allow-capability", capability=capability, on=on)


class Shortcuts(Resource):
    """Keyboard shortcuts — `settings.shortcuts`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    @property
    def bindings(self) -> Any:
        """Read only."""
        return self.fields["bindings"]
    @property
    def actions(self) -> Any:
        """Read only."""
        return self.fields["actions"]

    def rebind(self,
        *,
        shortcut: str,
        chord: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("rebind", shortcut=shortcut, chord=chord)

    def reset(self, *, shortcuts: list[str]) -> Run | None:
        """Runs the app's action."""
        return self.action("reset", shortcuts=shortcuts)

    def reset_all(self) -> Run | None:
        """Runs the app's action."""
        return self.action("reset-all")

    def set_action_chords(self, *, chords: Any) -> Run | None:
        """Runs the app's action."""
        return self.action("set-action-chords", chords=chords)


class ShellViews(Resource):
    """Shell views — `shell-views`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    @property
    def bindings(self) -> dict[str, Binding]:
        """Read only."""
        return self.fields["bindings"]
    @property
    def on_device(self) -> dict[str, dict[str, Binding]]:
        """Read only."""
        return self.fields["on_device"]

    def bind(self,
        *,
        slot: str,
        app: str,
        view: str,
        on: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["slot"] = slot
        args["app"] = app
        args["view"] = view
        if on is not None:
            args["on"] = on
        return self.action("bind", **args)

    def reset(self,
        *,
        slot: str,
        on: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["slot"] = slot
        if on is not None:
            args["on"] = on
        return self.action("reset", **args)


class DesktopControls(Resource):
    """Desktop controls — `settings.desktop-controls`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    vim_mode: bool


class DesktopControlsDevice(Resource):
    """Desktop controls on this device — `settings.desktop-controls-device`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    hide_top_bar: bool


class AgentProviders(Resource):
    """Agent model providers — `settings.agent-providers`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    @property
    def config(self) -> ProviderConfig:
        """Read only."""
        return self.fields["config"]

    def set_providers(self, *, config: Any) -> Run | None:
        """Runs the app's action."""
        return self.action("set-providers", config=config)

    def follow_local_models(self, *, profile: Any) -> Run | None:
        """Runs the app's action."""
        return self.action("follow-local-models", profile=profile)


class HostedAgentProviders(Resource):
    """Hosted agent model providers — `settings.hosted-agent-providers`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    @property
    def hosted(self) -> HostedProfiles:
        """Read only."""
        return self.fields["hosted"]

    def set_providers(self, *, hosted: Any) -> Run | None:
        """Runs the app's action."""
        return self.action("set-providers", hosted=hosted)


class DefaultApps(Resource):
    """Default apps — `settings.default-apps`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    @property
    def handlers(self) -> dict[str, str]:
        """Read only."""
        return self.fields["handlers"]

    def set_default_app_handler(self,
        *,
        kind: str,
        app: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-default-app-handler", kind=kind, app=app)


class Clock(Resource):
    """Clock — `settings.clock`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    automatic_zone: bool


class DisplayArrangement(Resource):
    """Display arrangement — `settings.display-arrangement`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    @property
    def arrangement(self) -> Any:
        """Read only."""
        return self.fields["arrangement"]

    def arrange_displays(self, *, arrangement: Any) -> Run | None:
        """Runs the app's action."""
        return self.action("arrange-displays", arrangement=arrangement)


class AutomaticDisplay(Resource):
    """Automatic display input — `settings.automatic-display`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    @property
    def automatic_display_input(self) -> bool:
        """Read only."""
        return self.fields["automatic_display_input"]

    def set_automatic_display_input(self, *, on: bool) -> Run | None:
        """Runs the app's action."""
        return self.action("set-automatic-display-input", on=on)


class DisplayChange(Resource):
    """Display change — `settings.display-change`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    @property
    def id(self) -> int:
        """Read only."""
        return self.fields["id"]
    @property
    def state(self) -> Any:
        """Read only."""
        return self.fields["state"]
    @property
    def set(self) -> Any:
        """Read only."""
        return self.fields["set"]
    @property
    def previous(self) -> Any:
        """Read only."""
        return self.fields["previous"]
    @property
    def candidate(self) -> Any:
        """Read only."""
        return self.fields["candidate"]
    @property
    def deadline_unix_ms(self) -> int | None:
        """Read only."""
        return self.fields["deadline_unix_ms"]
    @property
    def message(self) -> str | None:
        """Read only."""
        return self.fields["message"]


class OskSources(Resource):
    """On-screen keyboard layouts — `settings.osk-sources`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    @property
    def layout(self) -> str:
        """Read only."""
        return self.fields["layout"]
    @property
    def layouts(self) -> list[str]:
        """Read only."""
        return self.fields["layouts"]

    def set_layouts(self,
        *,
        layouts: list[str],
        active: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("set-layouts", layouts=layouts, active=active)


class DeviceVpn(Resource):
    """Your devices as a VPN — `settings.device-vpn`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    exit: bool
    @property
    def through(self) -> Any | None:
        """Read only."""
        return self.fields["through"]
    @property
    def stop(self) -> Any | None:
        """Read only."""
        return self.fields["stop"]
    @property
    def profile_kill_switches(self) -> dict[str, Any]:
        """Read only."""
        return self.fields["profile_kill_switches"]
    @property
    def profile_release(self) -> Any | None:
        """Read only."""
        return self.fields["profile_release"]

    def route_through(self,
        *,
        device: str,
        on: bool,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("route-through", device=device, on=on)

    def set_kill_switch(self, *, on: bool) -> Run | None:
        """Runs the app's action."""
        return self.action("set-kill-switch", on=on)

    # the app's `stop` action is named as a field of this kind: run it with
    # `action("stop")`.
    def set_profile_kill_switch(self,
        *,
        profile: str,
        on: bool,
        allow_local: bool | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["profile"] = profile
        args["on"] = on
        if allow_local is not None:
            args["allow_local"] = allow_local
        return self.action("set-profile-kill-switch", **args)

    def release_profile(self, *, profile: str) -> Run | None:
        """Runs the app's action."""
        return self.action("release-profile", profile=profile)


class Identity(Resource):
    """Your colour — `settings.identity`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    colour: str | None


class NetworkUsage(Resource):
    """Data usage — `settings.network-usage`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    reset_day: Any
    limit_gb: int
    @property
    def connections(self) -> dict[str, ConnectionUsage]:
        """Read only."""
        return self.fields["connections"]
    @property
    def counters(self) -> dict[str, Bytes]:
        """Read only."""
        return self.fields["counters"]
    @property
    def boot(self) -> str | None:
        """Read only."""
        return self.fields["boot"]

    def count_data_used(self,
        *,
        readings: list[Reading],
        at: int,
        offset: int,
        boot: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["readings"] = readings
        if boot is not None:
            args["boot"] = boot
        args["at"] = at
        args["offset"] = offset
        return self.action("count-data-used", **args)

    def reset_data_used(self,
        *,
        connection: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if connection is not None:
            args["connection"] = connection
        return self.action("reset-data-used", **args)

    def forget_data_used(self,
        *,
        connection: str | None = None,
    ) -> Run | None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if connection is not None:
            args["connection"] = connection
        return self.action("forget-data-used", **args)


class AgentDoor(Resource):
    """Agents from your other devices — `settings.agent-door`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    @property
    def accept_own_devices(self) -> bool:
        """Read only."""
        return self.fields["accept_own_devices"]

    def set_accept_own_devices(self, *, on: bool) -> Run | None:
        """Runs the app's action."""
        return self.action("set-accept-own-devices", on=on)


class Accounts(Resource):
    """Accounts — `tilo.accounts`."""
    __surface__ = "so.tilo.settings@7656a83c6a7e67b1"
    accounts: list[PersonAccount]
    active: dict[str, str]

    def publish_browser_identity(self,
        *,
        root: str,
        origin: str,
        display_name: str,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("publish-browser-identity", root=root, origin=origin, display_name=display_name)

    def save_account(self,
        *,
        id: str,
        type_id: str,
        display_name: str,
        config: Any,
        enabled: bool,
        tint: int,
    ) -> Run | None:
        """Runs the app's action."""
        return self.action("save-account", id=id, type_id=type_id, display_name=display_name, config=config, enabled=enabled, tint=tint)

    def remove_account(self, *, account: str) -> Run | None:
        """Runs the app's action."""
        return self.action("remove-account", account=account)

    def set_active_account(self, *, account: str) -> Run | None:
        """Runs the app's action."""
        return self.action("set-active-account", account=account)


