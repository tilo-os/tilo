"""Types for Settings sections (`so.tilo.settings.sections`), from the app's surface.

`tilo sdk gen --lang python` writes this file from the app's `surface.toml`;
write it again rather than editing it.

`Appearance` is the app's `settings.appearance` at `sections/<resource>`, read with
`await ….load()`: its fields are the app's fields (read-only ones without a setter)
and its methods are its actions. A method answers what the app declares; an action
that takes longer answers an `Invocation`. A record is the dict the wire carries, and an
enum whose cases carry fields is an object whose tag the app's schema names. An
action whose name is one the class already has is run with `action("…")`, which
the file says where it is. An undoable action that answers nothing also has a
`<action>_many`, which applies it to every argument object of a list in a few
all-or-nothing steps, all of them one invocation group.
"""

from __future__ import annotations

from typing import Any, Literal, NotRequired, TypedDict
from tilo import Entry, Invocation, Resource

__all__ = [
    "Ink",
    "Presence",
    "Status",
    "StatusTone",
    "Surfaces",
    "ThemeRecord",
    "ColorFilter",
    "MagnifierView",
    "OnScreenKeyboard",
    "ScreenReaderVoice",
    "SwitchKeyPair",
    "TouchpadClickMethod",
    "TouchpadScrollMethod",
    "EraserAction",
    "KeyboardProfile",
    "PadDialAction",
    "ShortcutAction",
    "SourceOptions",
    "StylusButtonAction",
    "TabletConfig",
    "PowerActionPref",
    "Place",
    "ShowMode",
    "WidgetPrefs",
    "AppNotificationRule",
    "FocusSchedule",
    "SystemSoundEvents",
    "SystemSoundPrefs",
    "PlacedDisplay",
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
    "DisplayConfig",
    "DisplaySet",
    "DisplayTransaction",
    "DisplayTransactionState",
    "OutputConfig",
    "OutputMode",
    "OutputTransform",
    "Bytes",
    "ConnectionKind",
    "ConnectionUsage",
    "Reading",
    "Restart",
    "AgentModelInUse",
    "AgentModelShown",
    "AgentProviderShown",
    "ActionShortcut",
    "ShortcutChord",
    "Appearance",
    "AppearanceDevice",
    "Accessibility",
    "AccessibilityDevice",
    "Input",
    "InputDevice",
    "TextAssistance",
    "Power",
    "Widgets",
    "SearchHistory",
    "Updates",
    "Notifications",
    "Audio",
    "AudioDevice",
    "Display",
    "DisplayPerson",
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
    "DisplayChange",
    "OskSources",
    "DeviceVpn",
    "Identity",
    "NetworkUsage",
    "AgentModels",
    "ShortcutsTable",
    "ChooseThemeArgs",
    "SetDarkModeArgs",
    "UseThemeArgs",
    "NewThemeArgs",
    "ImportThemeArgs",
    "ResetThemeArgs",
    "DeleteThemeArgs",
    "SwitchPolarityArgs",
    "ReviewThemeArgs",
    "StepMagnifierArgs",
    "SetScreenReaderArgs",
    "SetKeyRepeatArgs",
    "ChooseLayoutArgs",
    "SetLayoutsArgs",
    "SetPasteShortcutArgs",
    "SetKeyboardLayoutsArgs",
    "UseSharedLayoutsArgs",
    "SetKeyboardModelArgs",
    "ShowKeyboardLayoutArgs",
    "InspectKeyboardArgs",
    "SetTabletOutputArgs",
    "SetTabletKeepAspectArgs",
    "SetTabletLeftHandedArgs",
    "SetTabletTipFeelArgs",
    "SetTabletButtonArgs",
    "SetTabletEraserArgs",
    "SetPadButtonArgs",
    "SetPadDialArgs",
    "SetSuspendPluggedInArgs",
    "SetSuspendOnBatteryArgs",
    "SetWidgetEnabledArgs",
    "SetWidgetShowArgs",
    "PlaceWidgetArgs",
    "ResizeWidgetArgs",
    "RecordAutomaticUpdatesArgs",
    "RecordUpdateCheckArgs",
    "MuteAppArgs",
    "SetQuietHoursArgs",
    "SetMonoOutputArgs",
    "SetEchoCancellationArgs",
    "RememberOutputDeviceArgs",
    "RememberInputDeviceArgs",
    "SetAutomaticBrightnessArgs",
    "SetRotationLockArgs",
    "PlaceDisplayArgs",
    "AllowContinuityArgs",
    "AllowCapabilityArgs",
    "SetAutomaticDisplayInputArgs",
    "RebindArgs",
    "ResetArgs",
    "ResetAllArgs",
    "SetActionChordsArgs",
    "BindArgs",
    "ShellViewsResetArgs",
    "SetProvidersArgs",
    "FollowLocalModelsArgs",
    "HostedAgentProvidersSetProvidersArgs",
    "SetDefaultAppHandlerArgs",
    "RequestArgs",
    "OskSourcesSetLayoutsArgs",
    "RouteThroughArgs",
    "SetKillSwitchArgs",
    "StopArgs",
    "SetProfileKillSwitchArgs",
    "ReleaseProfileArgs",
    "CountDataUsedArgs",
    "ResetDataUsedArgs",
    "ForgetDataUsedArgs",
]

class Ink(TypedDict):
    primary: float
    secondary: float
    tertiary: float

class Presence(TypedDict):
    lightness: float
    chroma: float

class Status(TypedDict):
    danger: StatusTone
    warning: StatusTone
    success: StatusTone
    info: StatusTone

class StatusTone(TypedDict):
    lightness: float
    chroma: float

class Surfaces(TypedDict):
    base: float
    content: float
    sidebar: float
    inspector: float
    raised: float
    overlay: float

class ThemeRecord(TypedDict):
    name: str
    dark: bool
    high_contrast: bool
    hue: float | None
    chroma: float
    surfaces: Surfaces
    ink: Ink
    status: Status
    tint: float | None
    monochrome: bool
    presence: Presence
    window_radius: float
    density: float
    face: str
    base_size: float
    distinct_letters: bool | None
    motion: float
    blur: float
    tint_opacity: float
    frosted_strip: bool
    rim_span: float
    rim_alpha: float
    hover_alpha: float
    refraction: float
    origin: str | None
    findings: list[str]

ColorFilter = Literal["none", "grayscale", "inverted", "grayscale_inverted"]

MagnifierView = Literal["lens", "full_screen"]

OnScreenKeyboard = Literal["auto", "always", "never"]

ScreenReaderVoice = Literal["system", "english_us", "english_gb", "german", "french", "spanish"]

SwitchKeyPair = Literal["f6_f7", "f9_f10", "digits12"]

TouchpadClickMethod = Literal["areas", "clickfinger", "default"]

TouchpadScrollMethod = Literal["edge", "two_finger"]

EraserAction = dict[str, Any]
"""One of native, left, middle, right, back, forward, shortcut: an object whose case the app tags it with."""

class KeyboardProfile(TypedDict):
    sources: list[str]
    active: str
    options: dict[str, SourceOptions]

PadDialAction = Literal["nothing", "scroll", "zoom", "brush_size", "volume", "brightness"]

ShortcutAction = Literal["search", "workspace_overview", "lock", "quick_settings", "shortcut_help", "focus_previous_window", "focus_next_window", "previous_tab", "next_tab", "tab_menu", "previous_strip", "next_strip", "previous_workspace", "next_workspace", "toggle_maximize", "maximize_window", "restore_window", "detach_tab", "attach_tab_left", "attach_tab_right", "move_group_left", "move_group_right", "move_group_to_previous_strip", "move_group_to_next_strip", "move_group_to_previous_display", "move_group_to_next_display", "resize_narrower", "resize_wider", "reopen_closed_tab", "focus_group1", "focus_group2", "focus_group3", "focus_group4", "focus_group5", "focus_group6", "focus_group7", "focus_group8", "focus_group9", "focus_strip1", "focus_strip2", "focus_strip3", "focus_strip4", "focus_strip5", "focus_strip6", "focus_strip7", "focus_strip8", "focus_strip9", "open_files", "open_terminal", "clipboard_history", "notifications", "toggle_fullscreen", "toggle_magnifier", "magnifier_zoom_in", "magnifier_zoom_out", "power_menu", "next_input_source", "emoji_picker", "ask_agent", "say_something", "report_problem", "dictation_toggle", "dictation_finish", "dictation_cancel", "toggle_float"]

class SourceOptions(TypedDict):
    model: str | None
    compose_key: str | None
    caps_lock: str | None
    customised: bool

StylusButtonAction = dict[str, Any]
"""One of default, left, middle, right, back, forward, shortcut, disabled: an object whose case the app tags it with."""

class TabletConfig(TypedDict):
    output: str | None
    keep_aspect: bool
    left_handed: bool
    pressure_curve: list[float]
    lower_button: StylusButtonAction
    upper_button: StylusButtonAction
    third_button: StylusButtonAction
    eraser: EraserAction
    pad_buttons: dict[str, ShortcutAction]
    pad_map_button: int | None
    pad_rings: dict[str, list[PadDialAction]]
    pad_strips: dict[str, list[PadDialAction]]

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

class PlacedDisplay(TypedDict):
    device: str
    output: str
    order: int
    placed: bool
    x: int
    y: int

Currency = Literal["USD", "EUR", "GBP", "JPY", "CHF", "CAD", "AUD", "CNY", "INR"]

DateStyle = Literal["Language", "ISO", "US", "EU", "Long"]

FirstDayOfWeek = Literal["Monday", "Sunday", "Saturday"]

Measurement = Literal["Metric", "Imperial"]

NumberStyle = Literal["DecimalPoint", "DecimalComma"]

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

class DisplayConfig(TypedDict):
    outputs: dict[str, OutputConfig]

class DisplaySet(TypedDict):
    key: str
    members: dict[str, str]

class DisplayTransaction(TypedDict):
    id: int
    state: DisplayTransactionState
    set: DisplaySet
    previous: DisplayConfig
    candidate: DisplayConfig
    deadline_unix_ms: int | None
    message: str | None

DisplayTransactionState = Literal["requested", "awaiting_confirmation", "confirm_requested", "revert_requested", "confirmed", "reverted", "rejected"]

class OutputConfig(TypedDict):
    enabled: bool
    primary: bool
    x: int
    y: int
    scale: float
    transform: OutputTransform
    mode: OutputMode | None
    mirror_of: str | None
    hdr: bool
    sdr_brightness_nits: int
    vrr: bool
    color_profile: str | None

class OutputMode(TypedDict):
    width: int
    height: int
    refresh_mhz: int

OutputTransform = Literal["normal", "rotate90", "rotate180", "rotate270"]

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

class AgentModelInUse(TypedDict):
    provider: str
    model: str
    model_label: str

class AgentModelShown(TypedDict):
    id: str
    label: str

class AgentProviderShown(TypedDict):
    id: str
    label: str
    enabled: bool
    models: list[AgentModelShown]
    model: str
    model_label: str

class ActionShortcut(TypedDict):
    label: str
    chord: str

class ShortcutChord(TypedDict):
    id: str
    chord: str

ChooseThemeArgs = TypedDict("ChooseThemeArgs", {
    "theme": "str",
})
"""The arguments of `choose-theme`."""

SetDarkModeArgs = TypedDict("SetDarkModeArgs", {
    "on": "bool",
})
"""The arguments of `set-dark-mode`."""

UseThemeArgs = TypedDict("UseThemeArgs", {
    "theme": "str",
})
"""The arguments of `use-theme`."""

NewThemeArgs = TypedDict("NewThemeArgs", {
    "id": "str",
    "base": "str",
})
"""The arguments of `new-theme`."""

ImportThemeArgs = TypedDict("ImportThemeArgs", {
    "id": "str",
    "document": "str",
})
"""The arguments of `import-theme`."""

ResetThemeArgs = TypedDict("ResetThemeArgs", {
    "theme": "str",
})
"""The arguments of `reset-theme`."""

DeleteThemeArgs = TypedDict("DeleteThemeArgs", {
    "theme": "str",
})
"""The arguments of `delete-theme`."""

SwitchPolarityArgs = TypedDict("SwitchPolarityArgs", {
    "theme": "str",
    "dark": "bool",
})
"""The arguments of `switch-polarity`."""

ReviewThemeArgs = TypedDict("ReviewThemeArgs", {
    "theme": "str",
    "role": "str",
    "value": NotRequired["float | None"],
})
"""The arguments of `review-theme`."""

StepMagnifierArgs = TypedDict("StepMagnifierArgs", {
    "step": "str",
})
"""The arguments of `step-magnifier`."""

SetScreenReaderArgs = TypedDict("SetScreenReaderArgs", {
    "on": "bool",
})
"""The arguments of `set-screen-reader`."""

SetKeyRepeatArgs = TypedDict("SetKeyRepeatArgs", {
    "rate": "float",
})
"""The arguments of `set-key-repeat`."""

ChooseLayoutArgs = TypedDict("ChooseLayoutArgs", {
    "layout": "str",
})
"""The arguments of `choose-layout`."""

SetLayoutsArgs = TypedDict("SetLayoutsArgs", {
    "layouts": "list[str]",
    "active": "str",
})
"""The arguments of `set-layouts`."""

SetPasteShortcutArgs = TypedDict("SetPasteShortcutArgs", {
    "app": "str",
    "shortcut": "str",
})
"""The arguments of `set-paste-shortcut`."""

SetKeyboardLayoutsArgs = TypedDict("SetKeyboardLayoutsArgs", {
    "keyboard": "str",
    "profile": "KeyboardProfile",
})
"""The arguments of `set-keyboard-layouts`."""

UseSharedLayoutsArgs = TypedDict("UseSharedLayoutsArgs", {
    "keyboard": "str",
})
"""The arguments of `use-shared-layouts`."""

SetKeyboardModelArgs = TypedDict("SetKeyboardModelArgs", {
    "model": "str",
})
"""The arguments of `set-keyboard-model`."""

ShowKeyboardLayoutArgs = TypedDict("ShowKeyboardLayoutArgs", {
    "layout": NotRequired["str | None"],
})
"""The arguments of `show-keyboard-layout`."""

InspectKeyboardArgs = TypedDict("InspectKeyboardArgs", {
    "level": "int",
    "extra_levels": "bool",
    "key": "str",
})
"""The arguments of `inspect-keyboard`."""

SetTabletOutputArgs = TypedDict("SetTabletOutputArgs", {
    "device": "str",
    "output": "str",
})
"""The arguments of `set-tablet-output`."""

SetTabletKeepAspectArgs = TypedDict("SetTabletKeepAspectArgs", {
    "device": "str",
    "on": "bool",
})
"""The arguments of `set-tablet-keep-aspect`."""

SetTabletLeftHandedArgs = TypedDict("SetTabletLeftHandedArgs", {
    "device": "str",
    "on": "bool",
})
"""The arguments of `set-tablet-left-handed`."""

SetTabletTipFeelArgs = TypedDict("SetTabletTipFeelArgs", {
    "device": "str",
    "feel": "float",
})
"""The arguments of `set-tablet-tip-feel`."""

SetTabletButtonArgs = TypedDict("SetTabletButtonArgs", {
    "device": "str",
    "button": "str",
    "action": "str",
})
"""The arguments of `set-tablet-button`."""

SetTabletEraserArgs = TypedDict("SetTabletEraserArgs", {
    "device": "str",
    "action": "str",
})
"""The arguments of `set-tablet-eraser`."""

SetPadButtonArgs = TypedDict("SetPadButtonArgs", {
    "device": "str",
    "button": "int",
    "action": "str",
})
"""The arguments of `set-pad-button`."""

SetPadDialArgs = TypedDict("SetPadDialArgs", {
    "device": "str",
    "dial": "str",
    "index": "int",
    "mode": "int",
    "action": "str",
})
"""The arguments of `set-pad-dial`."""

SetSuspendPluggedInArgs = TypedDict("SetSuspendPluggedInArgs", {
    "minutes": "int",
})
"""The arguments of `set-suspend-plugged-in`."""

SetSuspendOnBatteryArgs = TypedDict("SetSuspendOnBatteryArgs", {
    "minutes": "int",
})
"""The arguments of `set-suspend-on-battery`."""

SetWidgetEnabledArgs = TypedDict("SetWidgetEnabledArgs", {
    "widget": "str",
    "enabled": "bool",
})
"""The arguments of `set-widget-enabled`."""

SetWidgetShowArgs = TypedDict("SetWidgetShowArgs", {
    "widget": "str",
    "show": "str",
})
"""The arguments of `set-widget-show`."""

PlaceWidgetArgs = TypedDict("PlaceWidgetArgs", {
    "widget": "str",
    "display": "str",
    "x": "float",
    "y": "float",
})
"""The arguments of `place-widget`."""

ResizeWidgetArgs = TypedDict("ResizeWidgetArgs", {
    "widget": "str",
    "width": "float",
})
"""The arguments of `resize-widget`."""

RecordAutomaticUpdatesArgs = TypedDict("RecordAutomaticUpdatesArgs", {
    "on": "bool",
})
"""The arguments of `record-automatic-updates`."""

RecordUpdateCheckArgs = TypedDict("RecordUpdateCheckArgs", {
    "at": "str",
})
"""The arguments of `record-update-check`."""

MuteAppArgs = TypedDict("MuteAppArgs", {
    "app": "str",
    "duration": "str",
})
"""The arguments of `mute-app`."""

SetQuietHoursArgs = TypedDict("SetQuietHoursArgs", {
    "start_minute": "int",
    "end_minute": "int",
    "weekdays": "int",
})
"""The arguments of `set-quiet-hours`."""

SetMonoOutputArgs = TypedDict("SetMonoOutputArgs", {
    "on": "bool",
})
"""The arguments of `set-mono-output`."""

SetEchoCancellationArgs = TypedDict("SetEchoCancellationArgs", {
    "on": "bool",
})
"""The arguments of `set-echo-cancellation`."""

RememberOutputDeviceArgs = TypedDict("RememberOutputDeviceArgs", {
    "device": "str",
})
"""The arguments of `remember-output-device`."""

RememberInputDeviceArgs = TypedDict("RememberInputDeviceArgs", {
    "device": "str",
})
"""The arguments of `remember-input-device`."""

SetAutomaticBrightnessArgs = TypedDict("SetAutomaticBrightnessArgs", {
    "on": "bool",
})
"""The arguments of `set-automatic-brightness`."""

SetRotationLockArgs = TypedDict("SetRotationLockArgs", {
    "locked": "bool",
})
"""The arguments of `set-rotation-lock`."""

PlaceDisplayArgs = TypedDict("PlaceDisplayArgs", {
    "device": "str",
    "output": "str",
    "x": "int",
    "y": "int",
    "order": NotRequired["int | None"],
    "placed": NotRequired["bool | None"],
})
"""The arguments of `place-display`."""

AllowContinuityArgs = TypedDict("AllowContinuityArgs", {
    "on": "bool",
})
"""The arguments of `allow-continuity`."""

AllowCapabilityArgs = TypedDict("AllowCapabilityArgs", {
    "capability": "str",
    "on": "bool",
})
"""The arguments of `allow-capability`."""

SetAutomaticDisplayInputArgs = TypedDict("SetAutomaticDisplayInputArgs", {
    "on": "bool",
})
"""The arguments of `set-automatic-display-input`."""

RebindArgs = TypedDict("RebindArgs", {
    "shortcut": "str",
    "chord": "str",
})
"""The arguments of `rebind`."""

ResetArgs = TypedDict("ResetArgs", {
    "shortcuts": "list[str]",
})
"""The arguments of `reset`."""

ResetAllArgs = TypedDict("ResetAllArgs", {
})
"""The arguments of `reset-all`."""

SetActionChordsArgs = TypedDict("SetActionChordsArgs", {
    "chords": "Any",
})
"""The arguments of `set-action-chords`."""

BindArgs = TypedDict("BindArgs", {
    "part": "str",
    "app": "str",
    "view": "str",
    "on": NotRequired["str | None"],
})
"""The arguments of `bind`."""

ShellViewsResetArgs = TypedDict("ShellViewsResetArgs", {
    "part": "str",
    "on": NotRequired["str | None"],
})
"""The arguments of `reset`."""

SetProvidersArgs = TypedDict("SetProvidersArgs", {
    "config": "Any",
})
"""The arguments of `set-providers`."""

FollowLocalModelsArgs = TypedDict("FollowLocalModelsArgs", {
    "profile": "Any",
})
"""The arguments of `follow-local-models`."""

HostedAgentProvidersSetProvidersArgs = TypedDict("HostedAgentProvidersSetProvidersArgs", {
    "hosted": "Any",
})
"""The arguments of `set-providers`."""

SetDefaultAppHandlerArgs = TypedDict("SetDefaultAppHandlerArgs", {
    "kind": "str",
    "app": "str",
})
"""The arguments of `set-default-app-handler`."""

RequestArgs = TypedDict("RequestArgs", {
    "change": "DisplayTransaction",
})
"""The arguments of `request`."""

OskSourcesSetLayoutsArgs = TypedDict("OskSourcesSetLayoutsArgs", {
    "layouts": "list[str]",
    "active": "str",
})
"""The arguments of `set-layouts`."""

RouteThroughArgs = TypedDict("RouteThroughArgs", {
    "device": "str",
    "on": "bool",
})
"""The arguments of `route-through`."""

SetKillSwitchArgs = TypedDict("SetKillSwitchArgs", {
    "on": "bool",
})
"""The arguments of `set-kill-switch`."""

StopArgs = TypedDict("StopArgs", {
    "device": "str",
})
"""The arguments of `stop`."""

SetProfileKillSwitchArgs = TypedDict("SetProfileKillSwitchArgs", {
    "profile": "str",
    "on": "bool",
    "allow_local": NotRequired["bool | None"],
})
"""The arguments of `set-profile-kill-switch`."""

ReleaseProfileArgs = TypedDict("ReleaseProfileArgs", {
    "profile": "str",
})
"""The arguments of `release-profile`."""

CountDataUsedArgs = TypedDict("CountDataUsedArgs", {
    "readings": "list[Reading]",
    "boot": NotRequired["str | None"],
    "at": "int",
    "offset": "int",
})
"""The arguments of `count-data-used`."""

ResetDataUsedArgs = TypedDict("ResetDataUsedArgs", {
    "connection": NotRequired["str | None"],
})
"""The arguments of `reset-data-used`."""

ForgetDataUsedArgs = TypedDict("ForgetDataUsedArgs", {
    "connection": NotRequired["str | None"],
})
"""The arguments of `forget-data-used`."""

class Appearance(Resource):
    """Appearance — `settings.appearance`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    mode: str
    dark_theme: str
    light_theme: str
    themes: list[Entry[ThemeRecord]]
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

    def choose_theme(self, *, theme: str) -> None:
        """Runs the app's action."""
        return self.action("choose-theme", theme=theme)

    def choose_theme_many(self, items: list[ChooseThemeArgs]) -> list[dict[str, Any]]:
        """Runs `choose-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("choose-theme", items)

    def set_dark_mode(self, *, on: bool) -> None:
        """Runs the app's action."""
        return self.action("set-dark-mode", on=on)

    def set_dark_mode_many(self, items: list[SetDarkModeArgs]) -> list[dict[str, Any]]:
        """Runs `set-dark-mode` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-dark-mode", items)

    def use_theme(self, *, theme: str) -> None:
        """Runs the app's action."""
        return self.action("use-theme", theme=theme)

    def use_theme_many(self, items: list[UseThemeArgs]) -> list[dict[str, Any]]:
        """Runs `use-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("use-theme", items)

    def new_theme(self,
        *,
        id: str,
        base: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("new-theme", id=id, base=base)

    def new_theme_many(self, items: list[NewThemeArgs]) -> list[dict[str, Any]]:
        """Runs `new-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("new-theme", items)

    def import_theme(self,
        *,
        id: str,
        document: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("import-theme", id=id, document=document)

    def import_theme_many(self, items: list[ImportThemeArgs]) -> list[dict[str, Any]]:
        """Runs `import-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("import-theme", items)

    def reset_theme(self, *, theme: str) -> None:
        """Runs the app's action."""
        return self.action("reset-theme", theme=theme)

    def reset_theme_many(self, items: list[ResetThemeArgs]) -> list[dict[str, Any]]:
        """Runs `reset-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reset-theme", items)

    def delete_theme(self, *, theme: str) -> None:
        """Runs the app's action."""
        return self.action("delete-theme", theme=theme)

    def delete_theme_many(self, items: list[DeleteThemeArgs]) -> list[dict[str, Any]]:
        """Runs `delete-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("delete-theme", items)

    def switch_polarity(self,
        *,
        theme: str,
        dark: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("switch-polarity", theme=theme, dark=dark)

    def switch_polarity_many(self, items: list[SwitchPolarityArgs]) -> list[dict[str, Any]]:
        """Runs `switch-polarity` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("switch-polarity", items)

    def review_theme(self,
        *,
        theme: str,
        role: str,
        value: float | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["theme"] = theme
        args["role"] = role
        if value is not None:
            args["value"] = value
        return self.action("review-theme", **args)

    def review_theme_many(self, items: list[ReviewThemeArgs]) -> list[dict[str, Any]]:
        """Runs `review-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("review-theme", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"choose_theme", "choose_theme_many", "set_dark_mode", "set_dark_mode_many", "use_theme", "use_theme_many", "new_theme", "new_theme_many", "import_theme", "import_theme_many", "reset_theme", "reset_theme_many", "delete_theme", "delete_theme_many", "switch_polarity", "switch_polarity_many", "review_theme", "review_theme_many"})


class AppearanceDevice(Resource):
    """Appearance on this device — `settings.appearance-device`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    @property
    def distribution_defaults(self) -> Any | None:
        """Read only."""
        return self.fields["distribution_defaults"]


class Accessibility(Resource):
    """Accessibility — `settings.accessibility`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
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
    dictation_language: str
    dictation_literal: bool

    def step_magnifier(self, *, step: str) -> None:
        """Runs the app's action."""
        return self.action("step-magnifier", step=step)

    def step_magnifier_many(self, items: list[StepMagnifierArgs]) -> list[dict[str, Any]]:
        """Runs `step-magnifier` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("step-magnifier", items)

    def set_screen_reader(self, *, on: bool) -> None:
        """Runs the app's action."""
        return self.action("set-screen-reader", on=on)

    def set_screen_reader_many(self, items: list[SetScreenReaderArgs]) -> list[dict[str, Any]]:
        """Runs `set-screen-reader` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-screen-reader", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"step_magnifier", "step_magnifier_many", "set_screen_reader", "set_screen_reader_many"})


class AccessibilityDevice(Resource):
    """Accessibility on this device — `settings.accessibility-device`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    apply_to_sign_in: bool


class Input(Resource):
    """Input — `settings.input`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
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

    def set_key_repeat(self, *, rate: float) -> None:
        """Runs the app's action."""
        return self.action("set-key-repeat", rate=rate)

    def set_key_repeat_many(self, items: list[SetKeyRepeatArgs]) -> list[dict[str, Any]]:
        """Runs `set-key-repeat` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-key-repeat", items)

    def choose_layout(self, *, layout: str) -> None:
        """Runs the app's action."""
        return self.action("choose-layout", layout=layout)

    def choose_layout_many(self, items: list[ChooseLayoutArgs]) -> list[dict[str, Any]]:
        """Runs `choose-layout` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("choose-layout", items)

    def set_layouts(self,
        *,
        layouts: list[str],
        active: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-layouts", layouts=layouts, active=active)

    def set_layouts_many(self, items: list[SetLayoutsArgs]) -> list[dict[str, Any]]:
        """Runs `set-layouts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-layouts", items)

    def set_paste_shortcut(self,
        *,
        app: str,
        shortcut: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-paste-shortcut", app=app, shortcut=shortcut)

    def set_paste_shortcut_many(self, items: list[SetPasteShortcutArgs]) -> list[dict[str, Any]]:
        """Runs `set-paste-shortcut` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-paste-shortcut", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_key_repeat", "set_key_repeat_many", "choose_layout", "choose_layout_many", "set_layouts", "set_layouts_many", "set_paste_shortcut", "set_paste_shortcut_many"})


class InputDevice(Resource):
    """Input on this device — `settings.input-device`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
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
    def tablets(self) -> dict[str, TabletConfig]:
        """Read only."""
        return self.fields["tablets"]

    def set_keyboard_layouts(self,
        *,
        keyboard: str,
        profile: KeyboardProfile,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-keyboard-layouts", keyboard=keyboard, profile=profile)

    def set_keyboard_layouts_many(self, items: list[SetKeyboardLayoutsArgs]) -> list[dict[str, Any]]:
        """Runs `set-keyboard-layouts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-keyboard-layouts", items)

    def use_shared_layouts(self, *, keyboard: str) -> None:
        """Runs the app's action."""
        return self.action("use-shared-layouts", keyboard=keyboard)

    def use_shared_layouts_many(self, items: list[UseSharedLayoutsArgs]) -> list[dict[str, Any]]:
        """Runs `use-shared-layouts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("use-shared-layouts", items)

    def set_keyboard_model(self, *, model: str) -> None:
        """Runs the app's action."""
        return self.action("set-keyboard-model", model=model)

    def set_keyboard_model_many(self, items: list[SetKeyboardModelArgs]) -> list[dict[str, Any]]:
        """Runs `set-keyboard-model` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-keyboard-model", items)

    def show_keyboard_layout(self,
        *,
        layout: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if layout is not None:
            args["layout"] = layout
        return self.action("show-keyboard-layout", **args)

    def show_keyboard_layout_many(self, items: list[ShowKeyboardLayoutArgs]) -> list[dict[str, Any]]:
        """Runs `show-keyboard-layout` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("show-keyboard-layout", items)

    def inspect_keyboard(self,
        *,
        level: int,
        extra_levels: bool,
        key: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("inspect-keyboard", level=level, extra_levels=extra_levels, key=key)

    def inspect_keyboard_many(self, items: list[InspectKeyboardArgs]) -> list[dict[str, Any]]:
        """Runs `inspect-keyboard` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("inspect-keyboard", items)

    def set_tablet_output(self,
        *,
        device: str,
        output: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-tablet-output", device=device, output=output)

    def set_tablet_output_many(self, items: list[SetTabletOutputArgs]) -> list[dict[str, Any]]:
        """Runs `set-tablet-output` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-tablet-output", items)

    def set_tablet_keep_aspect(self,
        *,
        device: str,
        on: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-tablet-keep-aspect", device=device, on=on)

    def set_tablet_keep_aspect_many(self, items: list[SetTabletKeepAspectArgs]) -> list[dict[str, Any]]:
        """Runs `set-tablet-keep-aspect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-tablet-keep-aspect", items)

    def set_tablet_left_handed(self,
        *,
        device: str,
        on: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-tablet-left-handed", device=device, on=on)

    def set_tablet_left_handed_many(self, items: list[SetTabletLeftHandedArgs]) -> list[dict[str, Any]]:
        """Runs `set-tablet-left-handed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-tablet-left-handed", items)

    def set_tablet_tip_feel(self,
        *,
        device: str,
        feel: float,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-tablet-tip-feel", device=device, feel=feel)

    def set_tablet_tip_feel_many(self, items: list[SetTabletTipFeelArgs]) -> list[dict[str, Any]]:
        """Runs `set-tablet-tip-feel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-tablet-tip-feel", items)

    def set_tablet_button(self,
        *,
        device: str,
        button: str,
        action: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-tablet-button", device=device, button=button, action=action)

    def set_tablet_button_many(self, items: list[SetTabletButtonArgs]) -> list[dict[str, Any]]:
        """Runs `set-tablet-button` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-tablet-button", items)

    def set_tablet_eraser(self,
        *,
        device: str,
        action: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-tablet-eraser", device=device, action=action)

    def set_tablet_eraser_many(self, items: list[SetTabletEraserArgs]) -> list[dict[str, Any]]:
        """Runs `set-tablet-eraser` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-tablet-eraser", items)

    def set_pad_button(self,
        *,
        device: str,
        button: int,
        action: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-pad-button", device=device, button=button, action=action)

    def set_pad_button_many(self, items: list[SetPadButtonArgs]) -> list[dict[str, Any]]:
        """Runs `set-pad-button` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-pad-button", items)

    def set_pad_dial(self,
        *,
        device: str,
        dial: str,
        index: int,
        mode: int,
        action: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-pad-dial", device=device, dial=dial, index=index, mode=mode, action=action)

    def set_pad_dial_many(self, items: list[SetPadDialArgs]) -> list[dict[str, Any]]:
        """Runs `set-pad-dial` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-pad-dial", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_keyboard_layouts", "set_keyboard_layouts_many", "use_shared_layouts", "use_shared_layouts_many", "set_keyboard_model", "set_keyboard_model_many", "show_keyboard_layout", "show_keyboard_layout_many", "inspect_keyboard", "inspect_keyboard_many", "set_tablet_output", "set_tablet_output_many", "set_tablet_keep_aspect", "set_tablet_keep_aspect_many", "set_tablet_left_handed", "set_tablet_left_handed_many", "set_tablet_tip_feel", "set_tablet_tip_feel_many", "set_tablet_button", "set_tablet_button_many", "set_tablet_eraser", "set_tablet_eraser_many", "set_pad_button", "set_pad_button_many", "set_pad_dial", "set_pad_dial_many"})


class TextAssistance(Resource):
    """Spelling and text — `settings.text-assistance`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    check_spelling: bool
    auto_correct: bool
    spelling_language: str


class Power(Resource):
    """Power — `settings.power`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
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

    def set_suspend_plugged_in(self, *, minutes: int) -> None:
        """Runs the app's action."""
        return self.action("set-suspend-plugged-in", minutes=minutes)

    def set_suspend_plugged_in_many(self, items: list[SetSuspendPluggedInArgs]) -> list[dict[str, Any]]:
        """Runs `set-suspend-plugged-in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-suspend-plugged-in", items)

    def set_suspend_on_battery(self, *, minutes: int) -> None:
        """Runs the app's action."""
        return self.action("set-suspend-on-battery", minutes=minutes)

    def set_suspend_on_battery_many(self, items: list[SetSuspendOnBatteryArgs]) -> list[dict[str, Any]]:
        """Runs `set-suspend-on-battery` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-suspend-on-battery", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_suspend_plugged_in", "set_suspend_plugged_in_many", "set_suspend_on_battery", "set_suspend_on_battery_many"})


class Widgets(Resource):
    """Widgets — `settings.widgets`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    music: WidgetPrefs
    team: WidgetPrefs
    calendar: WidgetPrefs
    system_monitor: WidgetPrefs

    def set_widget_enabled(self,
        *,
        widget: str,
        enabled: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-widget-enabled", widget=widget, enabled=enabled)

    def set_widget_enabled_many(self, items: list[SetWidgetEnabledArgs]) -> list[dict[str, Any]]:
        """Runs `set-widget-enabled` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-widget-enabled", items)

    def set_widget_show(self,
        *,
        widget: str,
        show: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-widget-show", widget=widget, show=show)

    def set_widget_show_many(self, items: list[SetWidgetShowArgs]) -> list[dict[str, Any]]:
        """Runs `set-widget-show` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-widget-show", items)

    def place_widget(self,
        *,
        widget: str,
        display: str,
        x: float,
        y: float,
    ) -> None:
        """Runs the app's action."""
        return self.action("place-widget", widget=widget, display=display, x=x, y=y)

    def place_widget_many(self, items: list[PlaceWidgetArgs]) -> list[dict[str, Any]]:
        """Runs `place-widget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("place-widget", items)

    def resize_widget(self,
        *,
        widget: str,
        width: float,
    ) -> None:
        """Runs the app's action."""
        return self.action("resize-widget", widget=widget, width=width)

    def resize_widget_many(self, items: list[ResizeWidgetArgs]) -> list[dict[str, Any]]:
        """Runs `resize-widget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("resize-widget", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_widget_enabled", "set_widget_enabled_many", "set_widget_show", "set_widget_show_many", "place_widget", "place_widget_many", "resize_widget", "resize_widget_many"})


class SearchHistory(Resource):
    """Search history — `settings.search_history`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    chosen: str
    chosen_for_text: str
    places: Any


class Updates(Resource):
    """Updates — `settings.updates`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    @property
    def auto(self) -> bool:
        """Read only."""
        return self.fields["auto"]
    @property
    def last_checked(self) -> str:
        """Read only."""
        return self.fields["last_checked"]

    def record_automatic_updates(self, *, on: bool) -> None:
        """Runs the app's action."""
        return self.action("record-automatic-updates", on=on)

    def record_automatic_updates_many(self, items: list[RecordAutomaticUpdatesArgs]) -> list[dict[str, Any]]:
        """Runs `record-automatic-updates` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record-automatic-updates", items)

    def record_update_check(self, *, at: str) -> None:
        """Runs the app's action."""
        return self.action("record-update-check", at=at)

    def record_update_check_many(self, items: list[RecordUpdateCheckArgs]) -> list[dict[str, Any]]:
        """Runs `record-update-check` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("record-update-check", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"record_automatic_updates", "record_automatic_updates_many", "record_update_check", "record_update_check_many"})


class Notifications(Resource):
    """Notifications — `settings.notifications`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    do_not_disturb: bool
    show_banners: bool
    show_previews: bool
    play_sounds: bool
    show_on_lock_screen: bool
    show_badges: bool
    focus: FocusSchedule
    per_app: dict[str, AppNotificationRule]

    def mute_app(self,
        *,
        app: str,
        duration: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("mute-app", app=app, duration=duration)

    def mute_app_many(self, items: list[MuteAppArgs]) -> list[dict[str, Any]]:
        """Runs `mute-app` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("mute-app", items)

    def set_quiet_hours(self,
        *,
        start_minute: int,
        end_minute: int,
        weekdays: int,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-quiet-hours", start_minute=start_minute, end_minute=end_minute, weekdays=weekdays)

    def set_quiet_hours_many(self, items: list[SetQuietHoursArgs]) -> list[dict[str, Any]]:
        """Runs `set-quiet-hours` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-quiet-hours", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"mute_app", "mute_app_many", "set_quiet_hours", "set_quiet_hours_many"})


class Audio(Resource):
    """Sound — `settings.audio`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
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

    def set_mono_output(self, *, on: bool) -> None:
        """Runs the app's action."""
        return self.action("set-mono-output", on=on)

    def set_mono_output_many(self, items: list[SetMonoOutputArgs]) -> list[dict[str, Any]]:
        """Runs `set-mono-output` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-mono-output", items)

    def set_echo_cancellation(self, *, on: bool) -> None:
        """Runs the app's action."""
        return self.action("set-echo-cancellation", on=on)

    def set_echo_cancellation_many(self, items: list[SetEchoCancellationArgs]) -> list[dict[str, Any]]:
        """Runs `set-echo-cancellation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-echo-cancellation", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_mono_output", "set_mono_output_many", "set_echo_cancellation", "set_echo_cancellation_many"})


class AudioDevice(Resource):
    """Sound on this device — `settings.audio-device`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    @property
    def output_device(self) -> str | None:
        """Read only."""
        return self.fields["output_device"]
    @property
    def input_device(self) -> str | None:
        """Read only."""
        return self.fields["input_device"]

    def remember_output_device(self, *, device: str) -> None:
        """Runs the app's action."""
        return self.action("remember-output-device", device=device)

    def remember_output_device_many(self, items: list[RememberOutputDeviceArgs]) -> list[dict[str, Any]]:
        """Runs `remember-output-device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remember-output-device", items)

    def remember_input_device(self, *, device: str) -> None:
        """Runs the app's action."""
        return self.action("remember-input-device", device=device)

    def remember_input_device_many(self, items: list[RememberInputDeviceArgs]) -> list[dict[str, Any]]:
        """Runs `remember-input-device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("remember-input-device", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"remember_output_device", "remember_output_device_many", "remember_input_device", "remember_input_device_many"})


class Display(Resource):
    """Displays — `settings.display`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    @property
    def auto_brightness(self) -> bool:
        """Read only."""
        return self.fields["auto_brightness"]
    @property
    def rotation_lock(self) -> bool:
        """Read only."""
        return self.fields["rotation_lock"]
    scale: float

    def set_automatic_brightness(self, *, on: bool) -> None:
        """Runs the app's action."""
        return self.action("set-automatic-brightness", on=on)

    def set_automatic_brightness_many(self, items: list[SetAutomaticBrightnessArgs]) -> list[dict[str, Any]]:
        """Runs `set-automatic-brightness` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-automatic-brightness", items)

    def set_rotation_lock(self, *, locked: bool) -> None:
        """Runs the app's action."""
        return self.action("set-rotation-lock", locked=locked)

    def set_rotation_lock_many(self, items: list[SetRotationLockArgs]) -> list[dict[str, Any]]:
        """Runs `set-rotation-lock` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-rotation-lock", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_automatic_brightness", "set_automatic_brightness_many", "set_rotation_lock", "set_rotation_lock_many"})


class DisplayPerson(Resource):
    """Night light — `settings.display-person`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    night_light: bool
    night_light_temperature: int
    night_light_schedule: bool
    @property
    def placement(self) -> list[Entry[PlacedDisplay]]:
        """Read only."""
        return self.fields["placement"]

    def place_display(self,
        *,
        device: str,
        output: str,
        x: int,
        y: int,
        order: int | None = None,
        placed: bool | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["device"] = device
        args["output"] = output
        args["x"] = x
        args["y"] = y
        if order is not None:
            args["order"] = order
        if placed is not None:
            args["placed"] = placed
        return self.action("place-display", **args)

    def place_display_many(self, items: list[PlaceDisplayArgs]) -> list[dict[str, Any]]:
        """Runs `place-display` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("place-display", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"place_display", "place_display_many"})


class Locale(Resource):
    """Region and language — `settings.locale`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    currency: Currency
    date_format: DateStyle
    time_format: TimeStyle
    measurement: Measurement
    number_style: NumberStyle
    first_day_of_week: FirstDayOfWeek


class Continuity(Resource):
    """Continuity — `settings.continuity`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
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
    @property
    def automatic_display_input(self) -> bool:
        """Read only."""
        return self.fields["automatic_display_input"]

    def allow_continuity(self, *, on: bool) -> None:
        """Runs the app's action."""
        return self.action("allow-continuity", on=on)

    def allow_continuity_many(self, items: list[AllowContinuityArgs]) -> list[dict[str, Any]]:
        """Runs `allow-continuity` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("allow-continuity", items)

    def allow_capability(self,
        *,
        capability: str,
        on: bool,
    ) -> None:
        """Runs the app's action."""
        return self.action("allow-capability", capability=capability, on=on)

    def allow_capability_many(self, items: list[AllowCapabilityArgs]) -> list[dict[str, Any]]:
        """Runs `allow-capability` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("allow-capability", items)

    def set_automatic_display_input(self, *, on: bool) -> None:
        """Runs the app's action."""
        return self.action("set-automatic-display-input", on=on)

    def set_automatic_display_input_many(self, items: list[SetAutomaticDisplayInputArgs]) -> list[dict[str, Any]]:
        """Runs `set-automatic-display-input` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-automatic-display-input", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"allow_continuity", "allow_continuity_many", "allow_capability", "allow_capability_many", "set_automatic_display_input", "set_automatic_display_input_many"})


class Shortcuts(Resource):
    """Keyboard shortcuts — `settings.shortcuts`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
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
    ) -> None:
        """Runs the app's action."""
        return self.action("rebind", shortcut=shortcut, chord=chord)

    def rebind_many(self, items: list[RebindArgs]) -> list[dict[str, Any]]:
        """Runs `rebind` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("rebind", items)

    def reset(self, *, shortcuts: list[str]) -> None:
        """Runs the app's action."""
        return self.action("reset", shortcuts=shortcuts)

    def reset_many(self, items: list[ResetArgs]) -> list[dict[str, Any]]:
        """Runs `reset` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reset", items)

    def reset_all(self) -> None:
        """Runs the app's action."""
        return self.action("reset-all")

    def reset_all_many(self, items: list[ResetAllArgs]) -> list[dict[str, Any]]:
        """Runs `reset-all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reset-all", items)

    def set_action_chords(self, *, chords: Any) -> None:
        """Runs the app's action."""
        return self.action("set-action-chords", chords=chords)

    def set_action_chords_many(self, items: list[SetActionChordsArgs]) -> list[dict[str, Any]]:
        """Runs `set-action-chords` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-action-chords", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"rebind", "rebind_many", "reset", "reset_many", "reset_all", "reset_all_many", "set_action_chords", "set_action_chords_many"})


class ShellViews(Resource):
    """Shell views — `shell-views`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
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
        part: str,
        app: str,
        view: str,
        on: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["part"] = part
        args["app"] = app
        args["view"] = view
        if on is not None:
            args["on"] = on
        return self.action("bind", **args)

    def bind_many(self, items: list[BindArgs]) -> list[dict[str, Any]]:
        """Runs `bind` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("bind", items)

    def reset(self,
        *,
        part: str,
        on: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["part"] = part
        if on is not None:
            args["on"] = on
        return self.action("reset", **args)

    def reset_many(self, items: list[ShellViewsResetArgs]) -> list[dict[str, Any]]:
        """Runs `reset` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reset", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"bind", "bind_many", "reset", "reset_many"})


class DesktopControls(Resource):
    """Desktop controls — `settings.desktop-controls`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    vim_mode: bool


class DesktopControlsDevice(Resource):
    """Desktop controls on this device — `settings.desktop-controls-device`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    hide_top_bar: bool


class AgentProviders(Resource):
    """Agent model providers — `settings.agent-providers`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    @property
    def config(self) -> ProviderConfig:
        """Read only."""
        return self.fields["config"]

    def set_providers(self, *, config: Any) -> None:
        """Runs the app's action."""
        return self.action("set-providers", config=config)

    def set_providers_many(self, items: list[SetProvidersArgs]) -> list[dict[str, Any]]:
        """Runs `set-providers` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-providers", items)

    def follow_local_models(self, *, profile: Any) -> None:
        """Runs the app's action."""
        return self.action("follow-local-models", profile=profile)

    def follow_local_models_many(self, items: list[FollowLocalModelsArgs]) -> list[dict[str, Any]]:
        """Runs `follow-local-models` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("follow-local-models", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_providers", "set_providers_many", "follow_local_models", "follow_local_models_many"})


class HostedAgentProviders(Resource):
    """Hosted agent model providers — `settings.hosted-agent-providers`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    @property
    def hosted(self) -> HostedProfiles:
        """Read only."""
        return self.fields["hosted"]

    def set_providers(self, *, hosted: Any) -> None:
        """Runs the app's action."""
        return self.action("set-providers", hosted=hosted)

    def set_providers_many(self, items: list[HostedAgentProvidersSetProvidersArgs]) -> list[dict[str, Any]]:
        """Runs `set-providers` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-providers", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_providers", "set_providers_many"})


class DefaultApps(Resource):
    """Default apps — `settings.default-apps`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    @property
    def handlers(self) -> dict[str, str]:
        """Read only."""
        return self.fields["handlers"]

    def set_default_app_handler(self,
        *,
        kind: str,
        app: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("set-default-app-handler", kind=kind, app=app)

    def set_default_app_handler_many(self, items: list[SetDefaultAppHandlerArgs]) -> list[dict[str, Any]]:
        """Runs `set-default-app-handler` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-default-app-handler", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_default_app_handler", "set_default_app_handler_many"})


class Clock(Resource):
    """Clock — `settings.clock`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    automatic_zone: bool


class DisplayChange(Resource):
    """Display change — `settings.display-change`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    @property
    def id(self) -> int:
        """Read only."""
        return self.fields["id"]
    @property
    def state(self) -> Any:
        """Read only."""
        return self.fields["state"]
    @property
    def deadline_unix_ms(self) -> int | None:
        """Read only."""
        return self.fields["deadline_unix_ms"]
    @property
    def message(self) -> str | None:
        """Read only."""
        return self.fields["message"]

    def request(self, *, change: DisplayTransaction) -> None:
        """Runs the app's action."""
        return self.action("request", change=change)

    def request_many(self, items: list[RequestArgs]) -> list[dict[str, Any]]:
        """Runs `request` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("request", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"request", "request_many"})


class OskSources(Resource):
    """On-screen keyboard layouts — `settings.osk-sources`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
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
    ) -> None:
        """Runs the app's action."""
        return self.action("set-layouts", layouts=layouts, active=active)

    def set_layouts_many(self, items: list[OskSourcesSetLayoutsArgs]) -> list[dict[str, Any]]:
        """Runs `set-layouts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-layouts", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"set_layouts", "set_layouts_many"})


class DeviceVpn(Resource):
    """Your devices as a VPN — `settings.device-vpn`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
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
    ) -> None:
        """Runs the app's action."""
        return self.action("route-through", device=device, on=on)

    def route_through_many(self, items: list[RouteThroughArgs]) -> list[dict[str, Any]]:
        """Runs `route-through` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("route-through", items)

    def set_kill_switch(self, *, on: bool) -> None:
        """Runs the app's action."""
        return self.action("set-kill-switch", on=on)

    def set_kill_switch_many(self, items: list[SetKillSwitchArgs]) -> list[dict[str, Any]]:
        """Runs `set-kill-switch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-kill-switch", items)

    # the app's `stop` action is named as a field of this kind: run it with
    # `action("stop")`.
    def set_profile_kill_switch(self,
        *,
        profile: str,
        on: bool,
        allow_local: bool | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["profile"] = profile
        args["on"] = on
        if allow_local is not None:
            args["allow_local"] = allow_local
        return self.action("set-profile-kill-switch", **args)

    def set_profile_kill_switch_many(self, items: list[SetProfileKillSwitchArgs]) -> list[dict[str, Any]]:
        """Runs `set-profile-kill-switch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("set-profile-kill-switch", items)

    def release_profile(self, *, profile: str) -> None:
        """Runs the app's action."""
        return self.action("release-profile", profile=profile)

    def release_profile_many(self, items: list[ReleaseProfileArgs]) -> list[dict[str, Any]]:
        """Runs `release-profile` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("release-profile", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"route_through", "route_through_many", "set_kill_switch", "set_kill_switch_many", "set_profile_kill_switch", "set_profile_kill_switch_many", "release_profile", "release_profile_many"})


class Identity(Resource):
    """Your colour — `settings.identity`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    colour: str | None


class NetworkUsage(Resource):
    """Data usage — `settings.network-usage`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    __setters__ = True
    reset_day: int
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
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        args["readings"] = readings
        if boot is not None:
            args["boot"] = boot
        args["at"] = at
        args["offset"] = offset
        return self.action("count-data-used", **args)

    def count_data_used_many(self, items: list[CountDataUsedArgs]) -> list[dict[str, Any]]:
        """Runs `count-data-used` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("count-data-used", items)

    def reset_data_used(self,
        *,
        connection: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if connection is not None:
            args["connection"] = connection
        return self.action("reset-data-used", **args)

    def reset_data_used_many(self, items: list[ResetDataUsedArgs]) -> list[dict[str, Any]]:
        """Runs `reset-data-used` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("reset-data-used", items)

    def forget_data_used(self,
        *,
        connection: str | None = None,
    ) -> None:
        """Runs the app's action."""
        args: dict[str, Any] = {}
        if connection is not None:
            args["connection"] = connection
        return self.action("forget-data-used", **args)

    def forget_data_used_many(self, items: list[ForgetDataUsedArgs]) -> list[dict[str, Any]]:
        """Runs `forget-data-used` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back."""
        return self.many("forget-data-used", items)

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"count_data_used", "count_data_used_many", "reset_data_used", "reset_data_used_many", "forget_data_used", "forget_data_used_many"})


class AgentModels(Resource):
    """Agent model — `settings.agent-models`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    @property
    def providers(self) -> list[AgentProviderShown]:
        """Read only."""
        return self.fields["providers"]
    @property
    def in_use(self) -> AgentModelInUse:
        """Read only."""
        return self.fields["in_use"]

    def use_model(self,
        *,
        provider: str,
        model: str,
    ) -> None:
        """Runs the app's action."""
        return self.action("use-model", provider=provider, model=model)

    def undo_model(self) -> None:
        """Runs the app's action."""
        return self.action("undo-model")

    def redo_model(self) -> None:
        """Runs the app's action."""
        return self.action("redo-model")

    # The actions a call reaches before the resource is read: `await tracker.create(...)`
    # needs no `load()` first.
    __actions__ = frozenset({"use_model", "undo_model", "redo_model"})


class ShortcutsTable(Resource):
    """Keyboard shortcuts — `settings.shortcuts-table`."""
    __surface__ = "so.tilo.settings.sections@f00b3793a302d0f0"
    @property
    def bindings(self) -> list[ShortcutChord]:
        """Read only."""
        return self.fields["bindings"]
    @property
    def actions(self) -> list[ActionShortcut]:
        """Read only."""
        return self.fields["actions"]


