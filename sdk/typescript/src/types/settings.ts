/**
 * Types for Settings (`so.tilo.settings`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Root` is a `Resource` of the app's `settings.root`, made by its factory with the
 * resource's path (`settings/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "@tilo/sdk";
import type * as tilo from "@tilo/sdk";

/** One of "none", "grayscale", "inverted", "grayscale_inverted". */
export type ColorFilter = "none" | "grayscale" | "inverted" | "grayscale_inverted";

/** One of "lens", "full_screen". */
export type MagnifierView = "lens" | "full_screen";

/** One of "auto", "always", "never". */
export type OnScreenKeyboard = "auto" | "always" | "never";

/** One of "system", "english_us", "english_gb", "german", "french", "spanish". */
export type ScreenReaderVoice = "system" | "english_us" | "english_gb" | "german" | "french" | "spanish";

/** One of "f6_f7", "f9_f10", "digits12". */
export type SwitchKeyPair = "f6_f7" | "f9_f10" | "digits12";

/** One of "areas", "clickfinger", "default". */
export type TouchpadClickMethod = "areas" | "clickfinger" | "default";

/** One of "edge", "two_finger". */
export type TouchpadScrollMethod = "edge" | "two_finger";

export interface KeyboardProfile {
  sources: string[];
  active: string;
  options: Record<string, SourceOptions>;
}

export interface SourceOptions {
  model: string | null;
  compose_key: string | null;
  caps_lock: string | null;
  customised: boolean;
}

/** One of "suspend", "lock", "ignore". */
export type PowerActionPref = "suspend" | "lock" | "ignore";

export interface Place {
  display: string;
  x: number;
  y: number;
}

/** One of "pause", "always", "ask". */
export type ShowMode = "pause" | "always" | "ask";

export interface WidgetPrefs {
  enabled: boolean;
  show: ShowMode;
  always_on_strips: number[];
  places: Place[];
  width: number | null;
}

export interface AppNotificationRule {
  banners: boolean | null;
  sounds: boolean | null;
  badges: boolean | null;
  previews: boolean | null;
  lock_screen: boolean | null;
  cross_device: boolean | null;
  priority: boolean;
  priority_contacts: boolean;
  muted_until: number | null;
}

export interface FocusSchedule {
  enabled: boolean;
  start_minute: number;
  end_minute: number;
  weekdays: number;
  allow_priority: boolean;
}

export interface SystemSoundEvents {
  notification: boolean;
  message: boolean;
  volume_change: boolean;
  screenshot: boolean;
  device_connected: boolean;
  device_disconnected: boolean;
  low_battery: boolean;
  error: boolean;
}

export interface SystemSoundPrefs {
  enabled: boolean;
  volume: number;
  theme: string | null;
  events: SystemSoundEvents;
}

/** One of "USD", "EUR", "GBP", "JPY", "CHF", "CAD", "AUD", "CNY", "INR". */
export type Currency = "USD" | "EUR" | "GBP" | "JPY" | "CHF" | "CAD" | "AUD" | "CNY" | "INR";

/** One of "Language", "ISO", "US", "EU", "Long". */
export type DateStyle = "Language" | "ISO" | "US" | "EU" | "Long";

/** One of "monday", "sunday", "saturday". */
export type FirstDayOfWeek = "monday" | "sunday" | "saturday";

/** One of "metric", "imperial". */
export type Measurement = "metric" | "imperial";

/** One of "decimal_point", "decimal_comma". */
export type NumberStyle = "decimal_point" | "decimal_comma";

/** One of "Language", "H24", "H12". */
export type TimeStyle = "Language" | "H24" | "H12";

export interface AuditEntry {
  recorded_at_secs: number;
  capability: ContinuityCapability | null;
  enabled: boolean;
}

/** One of "clipboard", "notifications", "wifi_settings", "media", "display_input", "file_transfer", "realtime_sessions", "typed_gamepad", "app_continuations". */
export type ContinuityCapability = "clipboard" | "notifications" | "wifi_settings" | "media" | "display_input" | "file_transfer" | "realtime_sessions" | "typed_gamepad" | "app_continuations";

export interface Binding {
  app: string;
  view: string;
}

export interface ProviderConfig {
  profiles: unknown[];
  default_profile: string | null;
  agent_models: Record<string, unknown>;
  interaction_models: Record<string, unknown>;
}

export interface HostedProfiles {
  profiles: unknown[];
}

export interface Bytes {
  received: number;
  sent: number;
}

/** One of "wifi", "wired", "mobile". */
export type ConnectionKind = "wifi" | "wired" | "mobile";

export interface ConnectionUsage {
  kind: unknown;
  name: string;
  days: Record<string, Bytes>;
  restart: Restart | null;
}

export interface Reading {
  counter: string;
  connection: string;
  kind: ConnectionKind;
  name: string;
  received: number;
  sent: number;
}

export interface Restart {
  day: string;
  before: number;
}

export interface PersonAccount {
  type_id: string;
  display_name: string;
  config: unknown;
  enabled: boolean;
  tint: number;
}

/** Settings — `settings.root`. */
export interface Root extends tilo.Resource {
  inspectInputSources(args?: {
    query?: string;
    page?: number;
  }): Promise<void>;
  inspectKeyboardMap(): Promise<void>;
  inspectKeyboardSources(): Promise<void>;
  storageSummary(): Promise<void>;
  searchHubModels(args: {
    query: string;
    format?: string;
  }): Promise<void>;
  showHubModel(args: {
    model: string;
  }): Promise<void>;
  discoverThunderbirdAccounts(args: {
    prefs: string;
  }): Promise<void>;
  chooseThunderbirdMetadata(): Promise<void>;
  inspectThunderbirdImport(): Promise<void>;
  inspectCalendarChoices(): Promise<void>;
}

/** The `settings.root` kind at `path`. */
export function root(ws: tilo.Workspace, path: string): Root {
  return resourceProxy(ws, path) as unknown as Root;
}

/** Appearance — `settings.appearance`. */
export interface Appearance extends tilo.Resource {
  mode: string;
  dark_theme: string;
  light_theme: string;
  readonly custom_themes: unknown[];
  density: number;
  high_contrast: boolean;
  reduce_motion: boolean;
  reduce_transparency: boolean;
  text_scale: number;
  ui_scale: number;
  schedule: string;
  sunrise: string;
  sunset: string;
  wallpaper_scope: string;
  wallpaper_target: string;
  wallpaper: string | null;
  forced_colors: boolean;
  distinct_letters: boolean | null;
  language: string | null;
  ui_face: string | null;
  accent: string | null;
  translucency: number | null;
  chooseTheme(args: {
    theme: string;
  }): Promise<tilo.Run | void>;
  setDarkMode(args: {
    on: boolean;
  }): Promise<tilo.Run | void>;
}

/** The `settings.appearance` kind at `path`. */
export function appearance(ws: tilo.Workspace, path: string): Appearance {
  return resourceProxy(ws, path) as unknown as Appearance;
}

/** Appearance on this device — `settings.appearance-device`. */
export interface AppearanceDevice extends tilo.Resource {
  readonly distribution_defaults: unknown | null;
}

/** The `settings.appearance-device` kind at `path`. */
export function appearanceDevice(ws: tilo.Workspace, path: string): AppearanceDevice {
  return resourceProxy(ws, path) as unknown as AppearanceDevice;
}

/** Accessibility — `settings.accessibility`. */
export interface Accessibility extends tilo.Resource {
  large_cursor: boolean;
  cursor_size: number;
  cursor_theme: string;
  readonly screen_reader: boolean;
  on_screen_keyboard: OnScreenKeyboard;
  sticky_keys: boolean;
  slow_keys: boolean;
  bounce_keys: boolean;
  bounce_keys_delay_ms: number;
  bounce_keys_alert: boolean;
  mouse_keys: boolean;
  visual_alerts: boolean;
  color_filter: ColorFilter;
  magnifier_zoom: number;
  magnifier_view: MagnifierView;
  readonly magnifier_last_zoom: number;
  cursor_crosshair: boolean;
  thick_text_caret: boolean;
  switch_control: boolean;
  switch_key_pair: SwitchKeyPair;
  switch_scan_ms: number;
  screen_reader_rate: number;
  screen_reader_voice: ScreenReaderVoice;
  dictation_language: unknown;
  dictation_literal: boolean;
  stepMagnifier(args: {
    step: string;
  }): Promise<tilo.Run | void>;
  setScreenReader(args: {
    on: boolean;
  }): Promise<tilo.Run | void>;
}

/** The `settings.accessibility` kind at `path`. */
export function accessibility(ws: tilo.Workspace, path: string): Accessibility {
  return resourceProxy(ws, path) as unknown as Accessibility;
}

/** Accessibility on this device — `settings.accessibility-device`. */
export interface AccessibilityDevice extends tilo.Resource {
  apply_to_sign_in: boolean;
}

/** The `settings.accessibility-device` kind at `path`. */
export function accessibilityDevice(ws: tilo.Workspace, path: string): AccessibilityDevice {
  return resourceProxy(ws, path) as unknown as AccessibilityDevice;
}

/** Input — `settings.input`. */
export interface Input extends tilo.Resource {
  touchpad_natural_scroll: boolean;
  mouse_natural_scroll: boolean;
  tap_to_click: boolean;
  pointer_speed: number;
  touchpad_pointer_speed: number | null;
  mouse_pointer_speed: number | null;
  key_repeat: number;
  readonly key_repeat_delay_ms: number | null;
  readonly key_repeat_rate_hz: number | null;
  readonly keyboard_layout: string;
  tap_drag: boolean;
  tap_drag_lock: boolean;
  disable_while_typing: boolean;
  middle_emulation: boolean;
  left_handed: boolean;
  accel_profile: string;
  touchpad_gestures: boolean;
  touchpad_click_method: TouchpadClickMethod;
  touchpad_scroll_method: TouchpadScrollMethod;
  scroll_speed: number;
  readonly compose_key: string | null;
  readonly caps_lock: string | null;
  readonly keyboard_layouts: string[];
  readonly paste_shortcuts: Record<string, string>;
  setKeyRepeat(args: {
    rate: number;
  }): Promise<tilo.Run | void>;
  chooseLayout(args: {
    layout: string;
  }): Promise<tilo.Run | void>;
  setLayouts(args: {
    layouts: string[];
    active: string;
  }): Promise<tilo.Run | void>;
  setPasteShortcut(args: {
    app: string;
    shortcut: string;
  }): Promise<tilo.Run | void>;
}

/** The `settings.input` kind at `path`. */
export function input(ws: tilo.Workspace, path: string): Input {
  return resourceProxy(ws, path) as unknown as Input;
}

/** Input on this device — `settings.input-device`. */
export interface InputDevice extends tilo.Resource {
  touchpad_enabled: boolean;
  touchpad_disabled_on_external_mouse: boolean;
  readonly keyboard_model: string;
  readonly keyboard_assignments: Record<string, KeyboardProfile>;
  readonly keyboard_preview: string | null;
  readonly keyboard_preview_extra_levels: boolean;
  readonly keyboard_preview_level: number;
  readonly keyboard_inspected_key: string;
  readonly tablets: Record<string, unknown>;
  setKeyboardLayouts(args: {
    keyboard: string;
    profile: KeyboardProfile;
  }): Promise<tilo.Run | void>;
  useSharedLayouts(args: {
    keyboard: string;
  }): Promise<tilo.Run | void>;
  setKeyboardModel(args: {
    model: string;
  }): Promise<tilo.Run | void>;
  showKeyboardLayout(args?: {
    layout?: string;
  }): Promise<tilo.Run | void>;
  inspectKeyboard(args: {
    level: number;
    extra_levels: boolean;
    key: string;
  }): Promise<tilo.Run | void>;
  setTabletOutput(args: {
    device: string;
    output: string;
  }): Promise<tilo.Run | void>;
  setTabletKeepAspect(args: {
    device: string;
    on: boolean;
  }): Promise<tilo.Run | void>;
  setTabletLeftHanded(args: {
    device: string;
    on: boolean;
  }): Promise<tilo.Run | void>;
  setTabletTipFeel(args: {
    device: string;
    feel: number;
  }): Promise<tilo.Run | void>;
  setTabletButton(args: {
    device: string;
    button: string;
    action: string;
  }): Promise<tilo.Run | void>;
  setTabletEraser(args: {
    device: string;
    action: string;
  }): Promise<tilo.Run | void>;
  setPadButton(args: {
    device: string;
    button: number;
    action: string;
  }): Promise<tilo.Run | void>;
  setPadDial(args: {
    device: string;
    dial: string;
    index: number;
    mode: number;
    action: string;
  }): Promise<tilo.Run | void>;
}

/** The `settings.input-device` kind at `path`. */
export function inputDevice(ws: tilo.Workspace, path: string): InputDevice {
  return resourceProxy(ws, path) as unknown as InputDevice;
}

/** Spelling and text — `settings.text-assistance`. */
export interface TextAssistance extends tilo.Resource {
  check_spelling: boolean;
  auto_correct: boolean;
  spelling_language: string;
}

/** The `settings.text-assistance` kind at `path`. */
export function textAssistance(ws: tilo.Workspace, path: string): TextAssistance {
  return resourceProxy(ws, path) as unknown as TextAssistance;
}

/** Power — `settings.power`. */
export interface Power extends tilo.Resource {
  auto_suspend: boolean;
  suspend_minutes: number;
  battery_suspend_minutes: number | null;
  saver_suspend_minutes: number | null;
  balanced_suspend_minutes: number | null;
  performance_suspend_minutes: number | null;
  blank_minutes: number;
  lock_minutes: number;
  lock_on_sleep: boolean;
  lid_close_action: PowerActionPref;
  power_key_action: PowerActionPref;
  low_battery_percent: number;
  critical_battery_percent: number;
  critical_action: PowerActionPref;
  setSuspendPluggedIn(args: {
    minutes: number;
  }): Promise<tilo.Run | void>;
  setSuspendOnBattery(args: {
    minutes: number;
  }): Promise<tilo.Run | void>;
}

/** The `settings.power` kind at `path`. */
export function power(ws: tilo.Workspace, path: string): Power {
  return resourceProxy(ws, path) as unknown as Power;
}

/** Desk widgets — `settings.desk_widgets`. */
export interface DeskWidgets extends tilo.Resource {
  music: WidgetPrefs;
  team: WidgetPrefs;
  calendar: WidgetPrefs;
  system_monitor: WidgetPrefs;
}

/** The `settings.desk_widgets` kind at `path`. */
export function deskWidgets(ws: tilo.Workspace, path: string): DeskWidgets {
  return resourceProxy(ws, path) as unknown as DeskWidgets;
}

/** Search history — `settings.search_history`. */
export interface SearchHistory extends tilo.Resource {
  chosen: string;
  chosen_for_text: string;
  places: unknown;
}

/** The `settings.search_history` kind at `path`. */
export function searchHistory(ws: tilo.Workspace, path: string): SearchHistory {
  return resourceProxy(ws, path) as unknown as SearchHistory;
}

/** Updates — `settings.updates`. */
export interface Updates extends tilo.Resource {
  readonly auto: boolean;
  readonly last_checked: string;
  recordAutomaticUpdates(args: {
    on: boolean;
  }): Promise<tilo.Run | void>;
  recordUpdateCheck(args: {
    at: string;
  }): Promise<tilo.Run | void>;
}

/** The `settings.updates` kind at `path`. */
export function updates(ws: tilo.Workspace, path: string): Updates {
  return resourceProxy(ws, path) as unknown as Updates;
}

/** Notifications — `settings.notifications`. */
export interface Notifications extends tilo.Resource {
  do_not_disturb: boolean;
  show_banners: boolean;
  show_previews: boolean;
  play_sounds: boolean;
  show_on_lock_screen: boolean;
  show_badges: boolean;
  focus: FocusSchedule;
  per_app: Record<string, AppNotificationRule>;
  setDoNotDisturb(args: {
    on: boolean;
  }): Promise<tilo.Run | void>;
  muteApp(args: {
    app: string;
    duration: string;
  }): Promise<tilo.Run | void>;
}

/** The `settings.notifications` kind at `path`. */
export function notifications(ws: tilo.Workspace, path: string): Notifications {
  return resourceProxy(ws, path) as unknown as Notifications;
}

/** Sound — `settings.audio`. */
export interface Audio extends tilo.Resource {
  system_sounds: SystemSoundPrefs;
  balance: number;
  readonly mono_output: boolean;
  readonly echo_cancellation: boolean;
  setMonoOutput(args: {
    on: boolean;
  }): Promise<tilo.Run | void>;
  setEchoCancellation(args: {
    on: boolean;
  }): Promise<tilo.Run | void>;
}

/** The `settings.audio` kind at `path`. */
export function audio(ws: tilo.Workspace, path: string): Audio {
  return resourceProxy(ws, path) as unknown as Audio;
}

/** Sound on this device — `settings.audio-device`. */
export interface AudioDevice extends tilo.Resource {
  readonly output_device: string | null;
  readonly input_device: string | null;
  rememberOutputDevice(args: {
    device: string;
  }): Promise<tilo.Run | void>;
  rememberInputDevice(args: {
    device: string;
  }): Promise<tilo.Run | void>;
}

/** The `settings.audio-device` kind at `path`. */
export function audioDevice(ws: tilo.Workspace, path: string): AudioDevice {
  return resourceProxy(ws, path) as unknown as AudioDevice;
}

/** Displays — `settings.display`. */
export interface Display extends tilo.Resource {
  readonly auto_brightness: boolean;
  readonly rotation_lock: boolean;
  scale: number;
  setAutomaticBrightness(args: {
    on: boolean;
  }): Promise<tilo.Run | void>;
  setRotationLock(args: {
    locked: boolean;
  }): Promise<tilo.Run | void>;
}

/** The `settings.display` kind at `path`. */
export function display(ws: tilo.Workspace, path: string): Display {
  return resourceProxy(ws, path) as unknown as Display;
}

/** Night light — `settings.night-light`. */
export interface NightLight extends tilo.Resource {
  night_light: boolean;
  night_light_temperature: number;
  night_light_schedule: boolean;
}

/** The `settings.night-light` kind at `path`. */
export function nightLight(ws: tilo.Workspace, path: string): NightLight {
  return resourceProxy(ws, path) as unknown as NightLight;
}

/** Region and language — `settings.locale`. */
export interface Locale extends tilo.Resource {
  currency: Currency;
  date_format: DateStyle;
  time_format: TimeStyle;
  measurement: Measurement;
  number_style: NumberStyle;
  first_day_of_week: FirstDayOfWeek;
}

/** The `settings.locale` kind at `path`. */
export function locale(ws: tilo.Workspace, path: string): Locale {
  return resourceProxy(ws, path) as unknown as Locale;
}

/** Continuity — `settings.continuity`. */
export interface Continuity extends tilo.Resource {
  readonly enabled: boolean;
  readonly clipboard: boolean;
  readonly notifications: boolean;
  readonly wifi_settings: boolean;
  readonly media: boolean;
  readonly display_input: boolean;
  readonly file_transfer: boolean;
  readonly realtime_sessions: boolean;
  readonly typed_gamepad: boolean;
  readonly app_continuations: boolean;
  readonly audit: AuditEntry[];
  allowContinuity(args: {
    on: boolean;
  }): Promise<tilo.Run | void>;
  allowCapability(args: {
    capability: string;
    on: boolean;
  }): Promise<tilo.Run | void>;
}

/** The `settings.continuity` kind at `path`. */
export function continuity(ws: tilo.Workspace, path: string): Continuity {
  return resourceProxy(ws, path) as unknown as Continuity;
}

/** Keyboard shortcuts — `settings.shortcuts`. */
export interface Shortcuts extends tilo.Resource {
  readonly bindings: unknown;
  readonly actions: unknown;
  rebind(args: {
    shortcut: string;
    chord: string;
  }): Promise<tilo.Run | void>;
  reset(args: {
    shortcuts: string[];
  }): Promise<tilo.Run | void>;
  resetAll(): Promise<tilo.Run | void>;
  setActionChords(args: {
    chords: unknown;
  }): Promise<tilo.Run | void>;
}

/** The `settings.shortcuts` kind at `path`. */
export function shortcuts(ws: tilo.Workspace, path: string): Shortcuts {
  return resourceProxy(ws, path) as unknown as Shortcuts;
}

/** Shell views — `shell-views`. */
export interface ShellViews extends tilo.Resource {
  readonly bindings: Record<string, Binding>;
  readonly on_device: Record<string, Record<string, Binding>>;
  bind(args: {
    slot: string;
    app: string;
    view: string;
    on?: string;
  }): Promise<tilo.Run | void>;
  reset(args: {
    slot: string;
    on?: string;
  }): Promise<tilo.Run | void>;
}

/** The `shell-views` kind at `path`. */
export function shellViews(ws: tilo.Workspace, path: string): ShellViews {
  return resourceProxy(ws, path) as unknown as ShellViews;
}

/** Desktop controls — `settings.desktop-controls`. */
export interface DesktopControls extends tilo.Resource {
  vim_mode: boolean;
}

/** The `settings.desktop-controls` kind at `path`. */
export function desktopControls(ws: tilo.Workspace, path: string): DesktopControls {
  return resourceProxy(ws, path) as unknown as DesktopControls;
}

/** Desktop controls on this device — `settings.desktop-controls-device`. */
export interface DesktopControlsDevice extends tilo.Resource {
  hide_top_bar: boolean;
}

/** The `settings.desktop-controls-device` kind at `path`. */
export function desktopControlsDevice(ws: tilo.Workspace, path: string): DesktopControlsDevice {
  return resourceProxy(ws, path) as unknown as DesktopControlsDevice;
}

/** Agent model providers — `settings.agent-providers`. */
export interface AgentProviders extends tilo.Resource {
  readonly config: ProviderConfig;
  setProviders(args: {
    config: unknown;
  }): Promise<tilo.Run | void>;
  followLocalModels(args: {
    profile: unknown;
  }): Promise<tilo.Run | void>;
}

/** The `settings.agent-providers` kind at `path`. */
export function agentProviders(ws: tilo.Workspace, path: string): AgentProviders {
  return resourceProxy(ws, path) as unknown as AgentProviders;
}

/** Hosted agent model providers — `settings.hosted-agent-providers`. */
export interface HostedAgentProviders extends tilo.Resource {
  readonly hosted: HostedProfiles;
  setProviders(args: {
    hosted: unknown;
  }): Promise<tilo.Run | void>;
}

/** The `settings.hosted-agent-providers` kind at `path`. */
export function hostedAgentProviders(ws: tilo.Workspace, path: string): HostedAgentProviders {
  return resourceProxy(ws, path) as unknown as HostedAgentProviders;
}

/** Default apps — `settings.default-apps`. */
export interface DefaultApps extends tilo.Resource {
  readonly handlers: Record<string, string>;
  setDefaultAppHandler(args: {
    kind: string;
    app: string;
  }): Promise<tilo.Run | void>;
}

/** The `settings.default-apps` kind at `path`. */
export function defaultApps(ws: tilo.Workspace, path: string): DefaultApps {
  return resourceProxy(ws, path) as unknown as DefaultApps;
}

/** Clock — `settings.clock`. */
export interface Clock extends tilo.Resource {
  automatic_zone: boolean;
}

/** The `settings.clock` kind at `path`. */
export function clock(ws: tilo.Workspace, path: string): Clock {
  return resourceProxy(ws, path) as unknown as Clock;
}

/** Display arrangement — `settings.display-arrangement`. */
export interface DisplayArrangement extends tilo.Resource {
  readonly arrangement: unknown;
  arrangeDisplays(args: {
    arrangement: unknown;
  }): Promise<tilo.Run | void>;
}

/** The `settings.display-arrangement` kind at `path`. */
export function displayArrangement(ws: tilo.Workspace, path: string): DisplayArrangement {
  return resourceProxy(ws, path) as unknown as DisplayArrangement;
}

/** Automatic display input — `settings.automatic-display`. */
export interface AutomaticDisplay extends tilo.Resource {
  readonly automatic_display_input: boolean;
  setAutomaticDisplayInput(args: {
    on: boolean;
  }): Promise<tilo.Run | void>;
}

/** The `settings.automatic-display` kind at `path`. */
export function automaticDisplay(ws: tilo.Workspace, path: string): AutomaticDisplay {
  return resourceProxy(ws, path) as unknown as AutomaticDisplay;
}

/** Display change — `settings.display-change`. */
export interface DisplayChange extends tilo.Resource {
  readonly id: number;
  readonly state: unknown;
  readonly set: unknown;
  readonly previous: unknown;
  readonly candidate: unknown;
  readonly deadline_unix_ms: number | null;
  readonly message: string | null;
}

/** The `settings.display-change` kind at `path`. */
export function displayChange(ws: tilo.Workspace, path: string): DisplayChange {
  return resourceProxy(ws, path) as unknown as DisplayChange;
}

/** On-screen keyboard layouts — `settings.osk-sources`. */
export interface OskSources extends tilo.Resource {
  readonly layout: string;
  readonly layouts: string[];
  setLayouts(args: {
    layouts: string[];
    active: string;
  }): Promise<tilo.Run | void>;
}

/** The `settings.osk-sources` kind at `path`. */
export function oskSources(ws: tilo.Workspace, path: string): OskSources {
  return resourceProxy(ws, path) as unknown as OskSources;
}

/** Your devices as a VPN — `settings.device-vpn`. */
export interface DeviceVpn extends tilo.Resource {
  exit: boolean;
  readonly through: unknown | null;
  readonly stop: unknown | null;
  readonly profile_kill_switches: Record<string, unknown>;
  readonly profile_release: unknown | null;
  routeThrough(args: {
    device: string;
    on: boolean;
  }): Promise<tilo.Run | void>;
  setKillSwitch(args: {
    on: boolean;
  }): Promise<tilo.Run | void>;
  // the app's `stop` action is named as a field of this kind: run it with
  // `action("stop")`.
  setProfileKillSwitch(args: {
    profile: string;
    on: boolean;
    allow_local?: boolean;
  }): Promise<tilo.Run | void>;
  releaseProfile(args: {
    profile: string;
  }): Promise<tilo.Run | void>;
}

/** The `settings.device-vpn` kind at `path`. */
export function deviceVpn(ws: tilo.Workspace, path: string): DeviceVpn {
  return resourceProxy(ws, path) as unknown as DeviceVpn;
}

/** Your colour — `settings.identity`. */
export interface Identity extends tilo.Resource {
  colour: string | null;
}

/** The `settings.identity` kind at `path`. */
export function identity(ws: tilo.Workspace, path: string): Identity {
  return resourceProxy(ws, path) as unknown as Identity;
}

/** Data usage — `settings.network-usage`. */
export interface NetworkUsage extends tilo.Resource {
  reset_day: unknown;
  limit_gb: number;
  readonly connections: Record<string, ConnectionUsage>;
  readonly counters: Record<string, Bytes>;
  readonly boot: string | null;
  countDataUsed(args: {
    readings: Reading[];
    at: number;
    offset: number;
    boot?: string;
  }): Promise<tilo.Run | void>;
  resetDataUsed(args?: {
    connection?: string;
  }): Promise<tilo.Run | void>;
  forgetDataUsed(args?: {
    connection?: string;
  }): Promise<tilo.Run | void>;
}

/** The `settings.network-usage` kind at `path`. */
export function networkUsage(ws: tilo.Workspace, path: string): NetworkUsage {
  return resourceProxy(ws, path) as unknown as NetworkUsage;
}

/** Agents from your other devices — `settings.agent-door`. */
export interface AgentDoor extends tilo.Resource {
  readonly accept_own_devices: boolean;
  setAcceptOwnDevices(args: {
    on: boolean;
  }): Promise<tilo.Run | void>;
}

/** The `settings.agent-door` kind at `path`. */
export function agentDoor(ws: tilo.Workspace, path: string): AgentDoor {
  return resourceProxy(ws, path) as unknown as AgentDoor;
}

/** Accounts — `tilo.accounts`. */
export interface Accounts extends tilo.Resource {
  accounts: PersonAccount[];
  active: Record<string, string>;
  publishBrowserIdentity(args: {
    root: string;
    origin: string;
    display_name: string;
  }): Promise<tilo.Run | void>;
  saveAccount(args: {
    id: string;
    type_id: string;
    display_name: string;
    config: unknown;
    enabled: boolean;
    tint: number;
  }): Promise<tilo.Run | void>;
  removeAccount(args: {
    account: string;
  }): Promise<tilo.Run | void>;
  setActiveAccount(args: {
    account: string;
  }): Promise<tilo.Run | void>;
}

/** The `tilo.accounts` kind at `path`. */
export function accounts(ws: tilo.Workspace, path: string): Accounts {
  return resourceProxy(ws, path) as unknown as Accounts;
}

