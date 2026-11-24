/**
 * Types for Settings sections (`so.tilo.settings.sections`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Appearance` is a `Resource` of the app's `settings.appearance`, made by its factory with the
 * resource's path (`sections/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers an `Invocation`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one invocation group.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.settings.sections@ffcedb70cea81fd7";

export interface Ink {
  primary: number;
  secondary: number;
  tertiary: number;
}

export interface Presence {
  lightness: number;
  chroma: number;
}

export interface Status {
  danger: StatusTone;
  warning: StatusTone;
  success: StatusTone;
  info: StatusTone;
}

export interface StatusTone {
  lightness: number;
  chroma: number;
}

export interface Surfaces {
  base: number;
  content: number;
  sidebar: number;
  inspector: number;
  raised: number;
  overlay: number;
}

export interface ThemeRecord {
  name: string;
  dark: boolean;
  high_contrast: boolean;
  hue: number | null;
  chroma: number;
  surfaces: Surfaces;
  ink: Ink;
  status: Status;
  tint: number | null;
  monochrome: boolean;
  presence: Presence;
  window_radius: number;
  density: number;
  face: string;
  base_size: number;
  distinct_letters: boolean | null;
  motion: number;
  blur: number;
  tint_opacity: number;
  frosted_strip: boolean;
  rim_span: number;
  rim_alpha: number;
  hover_alpha: number;
  refraction: number;
  origin: string | null;
  findings: string[];
}

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

/** One of native, left, middle, right, back, forward, shortcut: an object whose case the app tags it with. */
export type EraserAction = Record<string, unknown>;

export interface KeyboardProfile {
  sources: string[];
  active: string;
  options: Record<string, SourceOptions>;
}

/** One of "nothing", "scroll", "zoom", "brush_size", "volume", "brightness". */
export type PadDialAction = "nothing" | "scroll" | "zoom" | "brush_size" | "volume" | "brightness";

/** One of "search", "workspace_overview", "lock", "quick_settings", "shortcut_help", "focus_previous_window", "focus_next_window", "previous_tab", "next_tab", "tab_menu", "previous_strip", "next_strip", "previous_workspace", "next_workspace", "toggle_maximize", "maximize_window", "restore_window", "detach_tab", "attach_tab_left", "attach_tab_right", "move_group_left", "move_group_right", "move_group_to_previous_strip", "move_group_to_next_strip", "move_group_to_previous_display", "move_group_to_next_display", "resize_narrower", "resize_wider", "reopen_closed_tab", "focus_group1", "focus_group2", "focus_group3", "focus_group4", "focus_group5", "focus_group6", "focus_group7", "focus_group8", "focus_group9", "focus_strip1", "focus_strip2", "focus_strip3", "focus_strip4", "focus_strip5", "focus_strip6", "focus_strip7", "focus_strip8", "focus_strip9", "open_files", "open_terminal", "clipboard_history", "notifications", "toggle_fullscreen", "toggle_magnifier", "magnifier_zoom_in", "magnifier_zoom_out", "power_menu", "next_input_source", "emoji_picker", "ask_agent", "say_something", "report_problem", "dictation_toggle", "dictation_finish", "dictation_cancel", "toggle_float". */
export type ShortcutAction = "search" | "workspace_overview" | "lock" | "quick_settings" | "shortcut_help" | "focus_previous_window" | "focus_next_window" | "previous_tab" | "next_tab" | "tab_menu" | "previous_strip" | "next_strip" | "previous_workspace" | "next_workspace" | "toggle_maximize" | "maximize_window" | "restore_window" | "detach_tab" | "attach_tab_left" | "attach_tab_right" | "move_group_left" | "move_group_right" | "move_group_to_previous_strip" | "move_group_to_next_strip" | "move_group_to_previous_display" | "move_group_to_next_display" | "resize_narrower" | "resize_wider" | "reopen_closed_tab" | "focus_group1" | "focus_group2" | "focus_group3" | "focus_group4" | "focus_group5" | "focus_group6" | "focus_group7" | "focus_group8" | "focus_group9" | "focus_strip1" | "focus_strip2" | "focus_strip3" | "focus_strip4" | "focus_strip5" | "focus_strip6" | "focus_strip7" | "focus_strip8" | "focus_strip9" | "open_files" | "open_terminal" | "clipboard_history" | "notifications" | "toggle_fullscreen" | "toggle_magnifier" | "magnifier_zoom_in" | "magnifier_zoom_out" | "power_menu" | "next_input_source" | "emoji_picker" | "ask_agent" | "say_something" | "report_problem" | "dictation_toggle" | "dictation_finish" | "dictation_cancel" | "toggle_float";

export interface SourceOptions {
  model: string | null;
  compose_key: string | null;
  caps_lock: string | null;
  customised: boolean;
}

/** One of default, left, middle, right, back, forward, shortcut, disabled: an object whose case the app tags it with. */
export type StylusButtonAction = Record<string, unknown>;

export interface TabletConfig {
  output: string | null;
  keep_aspect: boolean;
  left_handed: boolean;
  pressure_curve: number[];
  lower_button: StylusButtonAction;
  upper_button: StylusButtonAction;
  third_button: StylusButtonAction;
  eraser: EraserAction;
  pad_buttons: Record<string, ShortcutAction>;
  pad_map_button: number | null;
  pad_rings: Record<string, PadDialAction[]>;
  pad_strips: Record<string, PadDialAction[]>;
}

export interface Snapshot {
  revision: number;
  words: Word[];
}

export interface Word {
  spelling: string;
  language: string | null;
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

export interface Learned {
  enabled: boolean;
  known: string[];
  shown: Record<string, number>;
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

export interface PlacedDisplay {
  device: string;
  output: string;
  order: number;
  placed: boolean;
  x: number;
  y: number;
}

/** One of "USD", "EUR", "GBP", "JPY", "CHF", "CAD", "AUD", "CNY", "INR". */
export type Currency = "USD" | "EUR" | "GBP" | "JPY" | "CHF" | "CAD" | "AUD" | "CNY" | "INR";

/** One of "Language", "ISO", "US", "EU", "Long". */
export type DateStyle = "Language" | "ISO" | "US" | "EU" | "Long";

/** One of "Monday", "Sunday", "Saturday". */
export type FirstDayOfWeek = "Monday" | "Sunday" | "Saturday";

/** One of "Metric", "Imperial". */
export type Measurement = "Metric" | "Imperial";

/** One of "DecimalPoint", "DecimalComma". */
export type NumberStyle = "DecimalPoint" | "DecimalComma";

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

export interface DisplayConfig {
  outputs: Record<string, OutputConfig>;
}

export interface DisplaySet {
  key: string;
  members: Record<string, string>;
}

export interface DisplayTransaction {
  id: number;
  state: DisplayTransactionState;
  set: DisplaySet;
  previous: DisplayConfig;
  candidate: DisplayConfig;
  deadline_unix_ms: number | null;
  message: string | null;
}

/** One of "requested", "awaiting_confirmation", "confirm_requested", "revert_requested", "confirmed", "reverted", "rejected". */
export type DisplayTransactionState = "requested" | "awaiting_confirmation" | "confirm_requested" | "revert_requested" | "confirmed" | "reverted" | "rejected";

export interface OutputConfig {
  enabled: boolean;
  primary: boolean;
  x: number;
  y: number;
  scale: number;
  transform: OutputTransform;
  mode: OutputMode | null;
  mirror_of: string | null;
  hdr: boolean;
  sdr_brightness_nits: number;
  vrr: boolean;
  color_profile: string | null;
}

export interface OutputMode {
  width: number;
  height: number;
  refresh_mhz: number;
}

/** One of "normal", "rotate90", "rotate180", "rotate270". */
export type OutputTransform = "normal" | "rotate90" | "rotate180" | "rotate270";

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

export interface AgentModelInUse {
  provider: string;
  model: string;
  model_label: string;
}

export interface AgentModelShown {
  id: string;
  label: string;
}

export interface AgentProviderShown {
  id: string;
  label: string;
  enabled: boolean;
  models: AgentModelShown[];
  model: string;
  model_label: string;
}

export interface ActionShortcut {
  label: string;
  chord: string;
}

export interface AppCommand {
  app: string;
  app_name: string;
  action: string;
  label: string;
  usual: string;
  chord: string;
  moved: boolean;
}

export interface ShortcutChord {
  id: string;
  chord: string;
}

export interface Language {
  tag: string;
  name: string;
}

export interface TabletChoiceIds {
  lower_button: string;
  upper_button: string;
  third_button: string;
  eraser: string;
  pad_buttons: Record<string, string>;
  pad_rings: Record<string, string[]>;
  pad_strips: Record<string, string[]>;
}

/** The arguments of `choose-theme`. */
export interface ChooseThemeArgs {
  theme: string;
}

/** The arguments of `set-dark-mode`. */
export interface SetDarkModeArgs {
  on: boolean;
}

/** The arguments of `choose-language`. */
export interface ChooseLanguageArgs {
  language?: string;
}

/** The arguments of `use-theme`. */
export interface UseThemeArgs {
  theme: string;
}

/** The arguments of `new-theme`. */
export interface NewThemeArgs {
  id: string;
  base: string;
}

/** The arguments of `import-theme`. */
export interface ImportThemeArgs {
  id: string;
  document: string;
}

/** The arguments of `reset-theme`. */
export interface ResetThemeArgs {
  theme: string;
}

/** The arguments of `delete-theme`. */
export interface DeleteThemeArgs {
  theme: string;
}

/** The arguments of `switch-polarity`. */
export interface SwitchPolarityArgs {
  theme: string;
  dark: boolean;
}

/** The arguments of `review-theme`. */
export interface ReviewThemeArgs {
  theme: string;
  role: string;
  value?: number;
}

/** The arguments of `step-magnifier`. */
export interface StepMagnifierArgs {
  step: string;
}

/** The arguments of `set-screen-reader`. */
export interface SetScreenReaderArgs {
  on: boolean;
}

/** The arguments of `set-key-repeat`. */
export interface SetKeyRepeatArgs {
  rate: number;
}

/** The arguments of `choose-layout`. */
export interface ChooseLayoutArgs {
  layout: string;
}

/** The arguments of `set-layouts`. */
export interface SetLayoutsArgs {
  layouts: string[];
  active: string;
}

/** The arguments of `set-caps-lock`. */
export interface SetCapsLockArgs {
  behaviour: string;
}

/** The arguments of `set-compose-key`. */
export interface SetComposeKeyArgs {
  key: string;
}

/** The arguments of `set-paste-shortcut`. */
export interface SetPasteShortcutArgs {
  app: string;
  shortcut: string;
}

/** The arguments of `set-keyboard-layouts`. */
export interface SetKeyboardLayoutsArgs {
  keyboard: string;
  profile: KeyboardProfile;
}

/** The arguments of `set-keyboard-sources`. */
export interface SetKeyboardSourcesArgs {
  keyboard: string;
  sources: string[];
  active: string;
}

/** The arguments of `use-shared-layouts`. */
export interface UseSharedLayoutsArgs {
  keyboard: string;
}

/** The arguments of `set-keyboard-model`. */
export interface SetKeyboardModelArgs {
  model: string;
}

/** The arguments of `show-keyboard-layout`. */
export interface ShowKeyboardLayoutArgs {
  layout?: string;
}

/** The arguments of `inspect-keyboard`. */
export interface InspectKeyboardArgs {
  shape: number;
  level: number;
  extra_levels: boolean;
}

/** The arguments of `set-tablet-output`. */
export interface SetTabletOutputArgs {
  device: string;
  output: string;
}

/** The arguments of `set-tablet-keep-aspect`. */
export interface SetTabletKeepAspectArgs {
  device: string;
  on: boolean;
}

/** The arguments of `set-tablet-left-handed`. */
export interface SetTabletLeftHandedArgs {
  device: string;
  on: boolean;
}

/** The arguments of `set-tablet-tip-feel`. */
export interface SetTabletTipFeelArgs {
  device: string;
  feel: number;
}

/** The arguments of `set-tablet-button`. */
export interface SetTabletButtonArgs {
  device: string;
  button: string;
  action: string;
}

/** The arguments of `set-tablet-eraser`. */
export interface SetTabletEraserArgs {
  device: string;
  action: string;
}

/** The arguments of `set-pad-button`. */
export interface SetPadButtonArgs {
  device: string;
  button: number;
  action: string;
}

/** The arguments of `set-pad-dial`. */
export interface SetPadDialArgs {
  device: string;
  dial: string;
  index: number;
  mode: number;
  action: string;
}

/** The arguments of `add`. */
export interface AddArgs {
  spelling: string;
  language?: string;
}

/** The arguments of `clear`. */
export interface ClearArgs {
}

/** The arguments of `set-suspend-plugged-in`. */
export interface SetSuspendPluggedInArgs {
  minutes: number;
}

/** The arguments of `set-suspend-on-battery`. */
export interface SetSuspendOnBatteryArgs {
  minutes: number;
}

/** The arguments of `set-widget-enabled`. */
export interface SetWidgetEnabledArgs {
  widget: string;
  enabled: boolean;
}

/** The arguments of `set-widget-show`. */
export interface SetWidgetShowArgs {
  widget: string;
  show: string;
}

/** The arguments of `place-widget`. */
export interface PlaceWidgetArgs {
  widget: string;
  display: string;
  x: number;
  y: number;
}

/** The arguments of `resize-widget`. */
export interface ResizeWidgetArgs {
  widget: string;
  width: number;
}

/** The arguments of `show-control-hints`. */
export interface ShowControlHintsArgs {
  enabled: boolean;
}

/** The arguments of `learn-control-hint`. */
export interface LearnControlHintArgs {
  hint: string;
}

/** The arguments of `note-control-hint-shown`. */
export interface NoteControlHintShownArgs {
  hint: string;
}

/** The arguments of `withdraw-control-hint-shown`. */
export interface WithdrawControlHintShownArgs {
  hint: string;
}

/** The arguments of `show-control-hints-again`. */
export interface ShowControlHintsAgainArgs {
}

/** The arguments of `record-automatic-updates`. */
export interface RecordAutomaticUpdatesArgs {
  on: boolean;
}

/** The arguments of `record-update-check`. */
export interface RecordUpdateCheckArgs {
  at: string;
}

/** The arguments of `mute-app`. */
export interface MuteAppArgs {
  app: string;
  duration: string;
}

/** The arguments of `set-quiet-hours`. */
export interface SetQuietHoursArgs {
  start_minute: number;
  end_minute: number;
  weekdays: number;
}

/** The arguments of `set-mono-output`. */
export interface SetMonoOutputArgs {
  on: boolean;
}

/** The arguments of `set-echo-cancellation`. */
export interface SetEchoCancellationArgs {
  on: boolean;
}

/** The arguments of `set-system-sound-theme`. */
export interface SetSystemSoundThemeArgs {
  theme?: string;
}

/** The arguments of `remember-output-device`. */
export interface RememberOutputDeviceArgs {
  device: string;
}

/** The arguments of `remember-input-device`. */
export interface RememberInputDeviceArgs {
  device: string;
}

/** The arguments of `set-automatic-brightness`. */
export interface SetAutomaticBrightnessArgs {
  on: boolean;
}

/** The arguments of `set-rotation-lock`. */
export interface SetRotationLockArgs {
  locked: boolean;
}

/** The arguments of `place-display`. */
export interface PlaceDisplayArgs {
  device: string;
  output: string;
  x: number;
  y: number;
  order?: number;
  placed?: boolean;
}

/** The arguments of `allow-continuity`. */
export interface AllowContinuityArgs {
  on: boolean;
}

/** The arguments of `allow-capability`. */
export interface AllowCapabilityArgs {
  capability: string;
  on: boolean;
}

/** The arguments of `set-automatic-display-input`. */
export interface SetAutomaticDisplayInputArgs {
  on: boolean;
}

/** The arguments of `rebind`. */
export interface RebindArgs {
  shortcut: string;
  chord: string;
}

/** The arguments of `reset`. */
export interface ResetArgs {
  shortcuts: string[];
}

/** The arguments of `reset-all`. */
export interface ResetAllArgs {
}

/** The arguments of `rebind-command`. */
export interface RebindCommandArgs {
  app: string;
  action: string;
  chord: string;
}

/** The arguments of `reset-command`. */
export interface ResetCommandArgs {
  app: string;
  action: string;
}

/** The arguments of `set-action-chords`. */
export interface SetActionChordsArgs {
  chords: unknown;
}

/** The arguments of `bind`. */
export interface BindArgs {
  part: string;
  app: string;
  view: string;
  on?: string;
}

/** The arguments of `reset`. */
export interface ShellViewsResetArgs {
  part: string;
  on?: string;
}

/** The arguments of `set-providers`. */
export interface SetProvidersArgs {
  config: unknown;
}

/** The arguments of `follow-local-models`. */
export interface FollowLocalModelsArgs {
  profile: unknown;
}

/** The arguments of `set-providers`. */
export interface HostedAgentProvidersSetProvidersArgs {
  hosted: unknown;
}

/** The arguments of `set-default-app-handler`. */
export interface SetDefaultAppHandlerArgs {
  kind: string;
  app: string;
}

/** The arguments of `request`. */
export interface RequestArgs {
  change: DisplayTransaction;
}

/** The arguments of `set-layouts`. */
export interface OskSourcesSetLayoutsArgs {
  layouts: string[];
  active: string;
}

/** The arguments of `route-through`. */
export interface RouteThroughArgs {
  device: string;
  on: boolean;
}

/** The arguments of `set-kill-switch`. */
export interface SetKillSwitchArgs {
  on: boolean;
}

/** The arguments of `stop`. */
export interface StopArgs {
  device: string;
}

/** The arguments of `set-profile-kill-switch`. */
export interface SetProfileKillSwitchArgs {
  profile: string;
  on: boolean;
  allow_local?: boolean;
}

/** The arguments of `release-profile`. */
export interface ReleaseProfileArgs {
  profile: string;
}

/** The arguments of `count-data-used`. */
export interface CountDataUsedArgs {
  readings: Reading[];
  at: number;
  offset: number;
  boot?: string;
}

/** The arguments of `reset-data-used`. */
export interface ResetDataUsedArgs {
  connection?: string;
}

/** The arguments of `forget-data-used`. */
export interface ForgetDataUsedArgs {
  connection?: string;
}

/** Appearance — `settings.appearance`. */
export interface Appearance extends tilo.Resource {
  mode: string;
  dark_theme: string;
  light_theme: string;
  themes: Array<tilo.Entry<ThemeRecord>>;
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
  }): Promise<void>;
  /** Runs `choose-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  chooseThemeMany(items: ChooseThemeArgs[]): Promise<tilo.ActionAnswer[]>;
  setDarkMode(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set-dark-mode` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setDarkModeMany(items: SetDarkModeArgs[]): Promise<tilo.ActionAnswer[]>;
  chooseLanguage(args?: {
    language?: string;
  }): Promise<void>;
  /** Runs `choose-language` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  chooseLanguageMany(items: ChooseLanguageArgs[]): Promise<tilo.ActionAnswer[]>;
  useTheme(args: {
    theme: string;
  }): Promise<void>;
  /** Runs `use-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  useThemeMany(items: UseThemeArgs[]): Promise<tilo.ActionAnswer[]>;
  newTheme(args: {
    id: string;
    base: string;
  }): Promise<void>;
  /** Runs `new-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  newThemeMany(items: NewThemeArgs[]): Promise<tilo.ActionAnswer[]>;
  importTheme(args: {
    id: string;
    document: string;
  }): Promise<void>;
  /** Runs `import-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  importThemeMany(items: ImportThemeArgs[]): Promise<tilo.ActionAnswer[]>;
  resetTheme(args: {
    theme: string;
  }): Promise<void>;
  /** Runs `reset-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resetThemeMany(items: ResetThemeArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteTheme(args: {
    theme: string;
  }): Promise<void>;
  /** Runs `delete-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteThemeMany(items: DeleteThemeArgs[]): Promise<tilo.ActionAnswer[]>;
  switchPolarity(args: {
    theme: string;
    dark: boolean;
  }): Promise<void>;
  /** Runs `switch-polarity` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  switchPolarityMany(items: SwitchPolarityArgs[]): Promise<tilo.ActionAnswer[]>;
  reviewTheme(args: {
    theme: string;
    role: string;
    value?: number;
  }): Promise<void>;
  /** Runs `review-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reviewThemeMany(items: ReviewThemeArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Appearance` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const APPEARANCE_ACTIONS = ["chooseTheme", "chooseThemeMany", "setDarkMode", "setDarkModeMany", "chooseLanguage", "chooseLanguageMany", "useTheme", "useThemeMany", "newTheme", "newThemeMany", "importTheme", "importThemeMany", "resetTheme", "resetThemeMany", "deleteTheme", "deleteThemeMany", "switchPolarity", "switchPolarityMany", "reviewTheme", "reviewThemeMany"] as const;

/** The `settings.appearance` kind at `path`. */
export function appearance(ws: tilo.Workspace, path: string): Appearance {
  return resourceProxy(ws, path, SURFACE, APPEARANCE_ACTIONS, true) as unknown as Appearance;
}

/** Appearance on this device — `settings.appearance-device`. */
export interface AppearanceDevice extends tilo.Resource {
  readonly distribution_defaults: unknown | null;
}

/** The actions `AppearanceDevice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const APPEARANCEDEVICE_ACTIONS = [] as const;

/** The `settings.appearance-device` kind at `path`. */
export function appearanceDevice(ws: tilo.Workspace, path: string): AppearanceDevice {
  return resourceProxy(ws, path, SURFACE, APPEARANCEDEVICE_ACTIONS, true) as unknown as AppearanceDevice;
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
  dictation_language: string;
  dictation_literal: boolean;
  stepMagnifier(args: {
    step: string;
  }): Promise<void>;
  /** Runs `step-magnifier` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stepMagnifierMany(items: StepMagnifierArgs[]): Promise<tilo.ActionAnswer[]>;
  setScreenReader(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set-screen-reader` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setScreenReaderMany(items: SetScreenReaderArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Accessibility` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ACCESSIBILITY_ACTIONS = ["stepMagnifier", "stepMagnifierMany", "setScreenReader", "setScreenReaderMany"] as const;

/** The `settings.accessibility` kind at `path`. */
export function accessibility(ws: tilo.Workspace, path: string): Accessibility {
  return resourceProxy(ws, path, SURFACE, ACCESSIBILITY_ACTIONS, true) as unknown as Accessibility;
}

/** Accessibility on this device — `settings.accessibility-device`. */
export interface AccessibilityDevice extends tilo.Resource {
  apply_to_sign_in: boolean;
}

/** The actions `AccessibilityDevice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ACCESSIBILITYDEVICE_ACTIONS = [] as const;

/** The `settings.accessibility-device` kind at `path`. */
export function accessibilityDevice(ws: tilo.Workspace, path: string): AccessibilityDevice {
  return resourceProxy(ws, path, SURFACE, ACCESSIBILITYDEVICE_ACTIONS, true) as unknown as AccessibilityDevice;
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
  }): Promise<void>;
  /** Runs `set-key-repeat` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setKeyRepeatMany(items: SetKeyRepeatArgs[]): Promise<tilo.ActionAnswer[]>;
  chooseLayout(args: {
    layout: string;
  }): Promise<void>;
  /** Runs `choose-layout` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  chooseLayoutMany(items: ChooseLayoutArgs[]): Promise<tilo.ActionAnswer[]>;
  setLayouts(args: {
    layouts: string[];
    active: string;
  }): Promise<void>;
  /** Runs `set-layouts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setLayoutsMany(items: SetLayoutsArgs[]): Promise<tilo.ActionAnswer[]>;
  setCapsLock(args: {
    behaviour: string;
  }): Promise<void>;
  /** Runs `set-caps-lock` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setCapsLockMany(items: SetCapsLockArgs[]): Promise<tilo.ActionAnswer[]>;
  setComposeKey(args: {
    key: string;
  }): Promise<void>;
  /** Runs `set-compose-key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setComposeKeyMany(items: SetComposeKeyArgs[]): Promise<tilo.ActionAnswer[]>;
  setPasteShortcut(args: {
    app: string;
    shortcut: string;
  }): Promise<void>;
  /** Runs `set-paste-shortcut` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setPasteShortcutMany(items: SetPasteShortcutArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Input` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const INPUT_ACTIONS = ["setKeyRepeat", "setKeyRepeatMany", "chooseLayout", "chooseLayoutMany", "setLayouts", "setLayoutsMany", "setCapsLock", "setCapsLockMany", "setComposeKey", "setComposeKeyMany", "setPasteShortcut", "setPasteShortcutMany"] as const;

/** The `settings.input` kind at `path`. */
export function input(ws: tilo.Workspace, path: string): Input {
  return resourceProxy(ws, path, SURFACE, INPUT_ACTIONS, true) as unknown as Input;
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
  readonly keyboard_inspected_shape: number;
  readonly tablets: Record<string, TabletConfig>;
  setKeyboardLayouts(args: {
    keyboard: string;
    profile: KeyboardProfile;
  }): Promise<void>;
  /** Runs `set-keyboard-layouts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setKeyboardLayoutsMany(items: SetKeyboardLayoutsArgs[]): Promise<tilo.ActionAnswer[]>;
  setKeyboardSources(args: {
    keyboard: string;
    sources: string[];
    active: string;
  }): Promise<void>;
  /** Runs `set-keyboard-sources` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setKeyboardSourcesMany(items: SetKeyboardSourcesArgs[]): Promise<tilo.ActionAnswer[]>;
  useSharedLayouts(args: {
    keyboard: string;
  }): Promise<void>;
  /** Runs `use-shared-layouts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  useSharedLayoutsMany(items: UseSharedLayoutsArgs[]): Promise<tilo.ActionAnswer[]>;
  setKeyboardModel(args: {
    model: string;
  }): Promise<void>;
  /** Runs `set-keyboard-model` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setKeyboardModelMany(items: SetKeyboardModelArgs[]): Promise<tilo.ActionAnswer[]>;
  showKeyboardLayout(args?: {
    layout?: string;
  }): Promise<void>;
  /** Runs `show-keyboard-layout` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  showKeyboardLayoutMany(items: ShowKeyboardLayoutArgs[]): Promise<tilo.ActionAnswer[]>;
  inspectKeyboard(args: {
    shape: number;
    level: number;
    extra_levels: boolean;
  }): Promise<void>;
  /** Runs `inspect-keyboard` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  inspectKeyboardMany(items: InspectKeyboardArgs[]): Promise<tilo.ActionAnswer[]>;
  setTabletOutput(args: {
    device: string;
    output: string;
  }): Promise<void>;
  /** Runs `set-tablet-output` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setTabletOutputMany(items: SetTabletOutputArgs[]): Promise<tilo.ActionAnswer[]>;
  setTabletKeepAspect(args: {
    device: string;
    on: boolean;
  }): Promise<void>;
  /** Runs `set-tablet-keep-aspect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setTabletKeepAspectMany(items: SetTabletKeepAspectArgs[]): Promise<tilo.ActionAnswer[]>;
  setTabletLeftHanded(args: {
    device: string;
    on: boolean;
  }): Promise<void>;
  /** Runs `set-tablet-left-handed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setTabletLeftHandedMany(items: SetTabletLeftHandedArgs[]): Promise<tilo.ActionAnswer[]>;
  setTabletTipFeel(args: {
    device: string;
    feel: number;
  }): Promise<void>;
  /** Runs `set-tablet-tip-feel` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setTabletTipFeelMany(items: SetTabletTipFeelArgs[]): Promise<tilo.ActionAnswer[]>;
  setTabletButton(args: {
    device: string;
    button: string;
    action: string;
  }): Promise<void>;
  /** Runs `set-tablet-button` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setTabletButtonMany(items: SetTabletButtonArgs[]): Promise<tilo.ActionAnswer[]>;
  setTabletEraser(args: {
    device: string;
    action: string;
  }): Promise<void>;
  /** Runs `set-tablet-eraser` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setTabletEraserMany(items: SetTabletEraserArgs[]): Promise<tilo.ActionAnswer[]>;
  setPadButton(args: {
    device: string;
    button: number;
    action: string;
  }): Promise<void>;
  /** Runs `set-pad-button` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setPadButtonMany(items: SetPadButtonArgs[]): Promise<tilo.ActionAnswer[]>;
  setPadDial(args: {
    device: string;
    dial: string;
    index: number;
    mode: number;
    action: string;
  }): Promise<void>;
  /** Runs `set-pad-dial` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setPadDialMany(items: SetPadDialArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `InputDevice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const INPUTDEVICE_ACTIONS = ["setKeyboardLayouts", "setKeyboardLayoutsMany", "setKeyboardSources", "setKeyboardSourcesMany", "useSharedLayouts", "useSharedLayoutsMany", "setKeyboardModel", "setKeyboardModelMany", "showKeyboardLayout", "showKeyboardLayoutMany", "inspectKeyboard", "inspectKeyboardMany", "setTabletOutput", "setTabletOutputMany", "setTabletKeepAspect", "setTabletKeepAspectMany", "setTabletLeftHanded", "setTabletLeftHandedMany", "setTabletTipFeel", "setTabletTipFeelMany", "setTabletButton", "setTabletButtonMany", "setTabletEraser", "setTabletEraserMany", "setPadButton", "setPadButtonMany", "setPadDial", "setPadDialMany"] as const;

/** The `settings.input-device` kind at `path`. */
export function inputDevice(ws: tilo.Workspace, path: string): InputDevice {
  return resourceProxy(ws, path, SURFACE, INPUTDEVICE_ACTIONS, true) as unknown as InputDevice;
}

/** Spelling and text — `settings.text-assistance`. */
export interface TextAssistance extends tilo.Resource {
  check_spelling: boolean;
  auto_correct: boolean;
  spelling_language: string;
}

/** The actions `TextAssistance` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TEXTASSISTANCE_ACTIONS = [] as const;

/** The `settings.text-assistance` kind at `path`. */
export function textAssistance(ws: tilo.Workspace, path: string): TextAssistance {
  return resourceProxy(ws, path, SURFACE, TEXTASSISTANCE_ACTIONS, true) as unknown as TextAssistance;
}

/** Personal dictionary — `settings.personal-dictionary`. */
export interface PersonalDictionary extends tilo.Resource {
  readonly words: Snapshot;
  add(args: {
    spelling: string;
    language?: string;
  }): Promise<void>;
  /** Runs `add` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addMany(items: AddArgs[]): Promise<tilo.ActionAnswer[]>;
  // the app's `remove` action is the SDK's own: run it with `action("remove")`.
  clear(): Promise<void>;
  /** Runs `clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clearMany(items: ClearArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `PersonalDictionary` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PERSONALDICTIONARY_ACTIONS = ["add", "addMany", "clear", "clearMany"] as const;

/** The `settings.personal-dictionary` kind at `path`. */
export function personalDictionary(ws: tilo.Workspace, path: string): PersonalDictionary {
  return resourceProxy(ws, path, SURFACE, PERSONALDICTIONARY_ACTIONS, true) as unknown as PersonalDictionary;
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
  }): Promise<void>;
  /** Runs `set-suspend-plugged-in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setSuspendPluggedInMany(items: SetSuspendPluggedInArgs[]): Promise<tilo.ActionAnswer[]>;
  setSuspendOnBattery(args: {
    minutes: number;
  }): Promise<void>;
  /** Runs `set-suspend-on-battery` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setSuspendOnBatteryMany(items: SetSuspendOnBatteryArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Power` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const POWER_ACTIONS = ["setSuspendPluggedIn", "setSuspendPluggedInMany", "setSuspendOnBattery", "setSuspendOnBatteryMany"] as const;

/** The `settings.power` kind at `path`. */
export function power(ws: tilo.Workspace, path: string): Power {
  return resourceProxy(ws, path, SURFACE, POWER_ACTIONS, true) as unknown as Power;
}

/** Widgets — `settings.widgets`. */
export interface Widgets extends tilo.Resource {
  music: WidgetPrefs;
  team: WidgetPrefs;
  calendar: WidgetPrefs;
  system_monitor: WidgetPrefs;
  setWidgetEnabled(args: {
    widget: string;
    enabled: boolean;
  }): Promise<void>;
  /** Runs `set-widget-enabled` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setWidgetEnabledMany(items: SetWidgetEnabledArgs[]): Promise<tilo.ActionAnswer[]>;
  setWidgetShow(args: {
    widget: string;
    show: string;
  }): Promise<void>;
  /** Runs `set-widget-show` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setWidgetShowMany(items: SetWidgetShowArgs[]): Promise<tilo.ActionAnswer[]>;
  placeWidget(args: {
    widget: string;
    display: string;
    x: number;
    y: number;
  }): Promise<void>;
  /** Runs `place-widget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  placeWidgetMany(items: PlaceWidgetArgs[]): Promise<tilo.ActionAnswer[]>;
  resizeWidget(args: {
    widget: string;
    width: number;
  }): Promise<void>;
  /** Runs `resize-widget` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resizeWidgetMany(items: ResizeWidgetArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Widgets` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WIDGETS_ACTIONS = ["setWidgetEnabled", "setWidgetEnabledMany", "setWidgetShow", "setWidgetShowMany", "placeWidget", "placeWidgetMany", "resizeWidget", "resizeWidgetMany"] as const;

/** The `settings.widgets` kind at `path`. */
export function widgets(ws: tilo.Workspace, path: string): Widgets {
  return resourceProxy(ws, path, SURFACE, WIDGETS_ACTIONS, true) as unknown as Widgets;
}

/** Control hints — `settings.control-hints`. */
export interface ControlHints extends tilo.Resource {
  learned: Learned;
  showControlHints(args: {
    enabled: boolean;
  }): Promise<void>;
  /** Runs `show-control-hints` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  showControlHintsMany(items: ShowControlHintsArgs[]): Promise<tilo.ActionAnswer[]>;
  learnControlHint(args: {
    hint: string;
  }): Promise<void>;
  /** Runs `learn-control-hint` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  learnControlHintMany(items: LearnControlHintArgs[]): Promise<tilo.ActionAnswer[]>;
  noteControlHintShown(args: {
    hint: string;
  }): Promise<void>;
  /** Runs `note-control-hint-shown` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  noteControlHintShownMany(items: NoteControlHintShownArgs[]): Promise<tilo.ActionAnswer[]>;
  withdrawControlHintShown(args: {
    hint: string;
  }): Promise<void>;
  /** Runs `withdraw-control-hint-shown` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  withdrawControlHintShownMany(items: WithdrawControlHintShownArgs[]): Promise<tilo.ActionAnswer[]>;
  showControlHintsAgain(): Promise<void>;
  /** Runs `show-control-hints-again` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  showControlHintsAgainMany(items: ShowControlHintsAgainArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `ControlHints` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONTROLHINTS_ACTIONS = ["showControlHints", "showControlHintsMany", "learnControlHint", "learnControlHintMany", "noteControlHintShown", "noteControlHintShownMany", "withdrawControlHintShown", "withdrawControlHintShownMany", "showControlHintsAgain", "showControlHintsAgainMany"] as const;

/** The `settings.control-hints` kind at `path`. */
export function controlHints(ws: tilo.Workspace, path: string): ControlHints {
  return resourceProxy(ws, path, SURFACE, CONTROLHINTS_ACTIONS, true) as unknown as ControlHints;
}

/** Search history — `settings.search_history`. */
export interface SearchHistory extends tilo.Resource {
  chosen: string;
  chosen_for_text: string;
  places: unknown;
}

/** The actions `SearchHistory` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SEARCHHISTORY_ACTIONS = [] as const;

/** The `settings.search_history` kind at `path`. */
export function searchHistory(ws: tilo.Workspace, path: string): SearchHistory {
  return resourceProxy(ws, path, SURFACE, SEARCHHISTORY_ACTIONS, true) as unknown as SearchHistory;
}

/** Updates — `settings.updates`. */
export interface Updates extends tilo.Resource {
  readonly auto: boolean;
  readonly last_checked: string;
  recordAutomaticUpdates(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `record-automatic-updates` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordAutomaticUpdatesMany(items: RecordAutomaticUpdatesArgs[]): Promise<tilo.ActionAnswer[]>;
  recordUpdateCheck(args: {
    at: string;
  }): Promise<void>;
  /** Runs `record-update-check` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordUpdateCheckMany(items: RecordUpdateCheckArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Updates` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const UPDATES_ACTIONS = ["recordAutomaticUpdates", "recordAutomaticUpdatesMany", "recordUpdateCheck", "recordUpdateCheckMany"] as const;

/** The `settings.updates` kind at `path`. */
export function updates(ws: tilo.Workspace, path: string): Updates {
  return resourceProxy(ws, path, SURFACE, UPDATES_ACTIONS, true) as unknown as Updates;
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
  muteApp(args: {
    app: string;
    duration: string;
  }): Promise<void>;
  /** Runs `mute-app` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  muteAppMany(items: MuteAppArgs[]): Promise<tilo.ActionAnswer[]>;
  setQuietHours(args: {
    start_minute: number;
    end_minute: number;
    weekdays: number;
  }): Promise<void>;
  /** Runs `set-quiet-hours` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setQuietHoursMany(items: SetQuietHoursArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Notifications` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NOTIFICATIONS_ACTIONS = ["muteApp", "muteAppMany", "setQuietHours", "setQuietHoursMany"] as const;

/** The `settings.notifications` kind at `path`. */
export function notifications(ws: tilo.Workspace, path: string): Notifications {
  return resourceProxy(ws, path, SURFACE, NOTIFICATIONS_ACTIONS, true) as unknown as Notifications;
}

/** Sound — `settings.audio`. */
export interface Audio extends tilo.Resource {
  system_sounds: SystemSoundPrefs;
  balance: number;
  readonly mono_output: boolean;
  readonly echo_cancellation: boolean;
  setMonoOutput(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set-mono-output` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setMonoOutputMany(items: SetMonoOutputArgs[]): Promise<tilo.ActionAnswer[]>;
  setEchoCancellation(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set-echo-cancellation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setEchoCancellationMany(items: SetEchoCancellationArgs[]): Promise<tilo.ActionAnswer[]>;
  setSystemSoundTheme(args?: {
    theme?: string;
  }): Promise<void>;
  /** Runs `set-system-sound-theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setSystemSoundThemeMany(items: SetSystemSoundThemeArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Audio` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const AUDIO_ACTIONS = ["setMonoOutput", "setMonoOutputMany", "setEchoCancellation", "setEchoCancellationMany", "setSystemSoundTheme", "setSystemSoundThemeMany"] as const;

/** The `settings.audio` kind at `path`. */
export function audio(ws: tilo.Workspace, path: string): Audio {
  return resourceProxy(ws, path, SURFACE, AUDIO_ACTIONS, true) as unknown as Audio;
}

/** Sound on this device — `settings.audio-device`. */
export interface AudioDevice extends tilo.Resource {
  readonly output_device: string | null;
  readonly input_device: string | null;
  rememberOutputDevice(args: {
    device: string;
  }): Promise<void>;
  /** Runs `remember-output-device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rememberOutputDeviceMany(items: RememberOutputDeviceArgs[]): Promise<tilo.ActionAnswer[]>;
  rememberInputDevice(args: {
    device: string;
  }): Promise<void>;
  /** Runs `remember-input-device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rememberInputDeviceMany(items: RememberInputDeviceArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `AudioDevice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const AUDIODEVICE_ACTIONS = ["rememberOutputDevice", "rememberOutputDeviceMany", "rememberInputDevice", "rememberInputDeviceMany"] as const;

/** The `settings.audio-device` kind at `path`. */
export function audioDevice(ws: tilo.Workspace, path: string): AudioDevice {
  return resourceProxy(ws, path, SURFACE, AUDIODEVICE_ACTIONS, true) as unknown as AudioDevice;
}

/** Displays — `settings.display`. */
export interface Display extends tilo.Resource {
  readonly auto_brightness: boolean;
  readonly rotation_lock: boolean;
  scale: number;
  setAutomaticBrightness(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set-automatic-brightness` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setAutomaticBrightnessMany(items: SetAutomaticBrightnessArgs[]): Promise<tilo.ActionAnswer[]>;
  setRotationLock(args: {
    locked: boolean;
  }): Promise<void>;
  /** Runs `set-rotation-lock` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setRotationLockMany(items: SetRotationLockArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Display` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DISPLAY_ACTIONS = ["setAutomaticBrightness", "setAutomaticBrightnessMany", "setRotationLock", "setRotationLockMany"] as const;

/** The `settings.display` kind at `path`. */
export function display(ws: tilo.Workspace, path: string): Display {
  return resourceProxy(ws, path, SURFACE, DISPLAY_ACTIONS, true) as unknown as Display;
}

/** Night light — `settings.display-person`. */
export interface DisplayPerson extends tilo.Resource {
  night_light: boolean;
  night_light_temperature: number;
  night_light_schedule: boolean;
  readonly placement: Array<tilo.Entry<PlacedDisplay>>;
  placeDisplay(args: {
    device: string;
    output: string;
    x: number;
    y: number;
    order?: number;
    placed?: boolean;
  }): Promise<void>;
  /** Runs `place-display` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  placeDisplayMany(items: PlaceDisplayArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `DisplayPerson` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DISPLAYPERSON_ACTIONS = ["placeDisplay", "placeDisplayMany"] as const;

/** The `settings.display-person` kind at `path`. */
export function displayPerson(ws: tilo.Workspace, path: string): DisplayPerson {
  return resourceProxy(ws, path, SURFACE, DISPLAYPERSON_ACTIONS, true) as unknown as DisplayPerson;
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

/** The actions `Locale` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LOCALE_ACTIONS = [] as const;

/** The `settings.locale` kind at `path`. */
export function locale(ws: tilo.Workspace, path: string): Locale {
  return resourceProxy(ws, path, SURFACE, LOCALE_ACTIONS, true) as unknown as Locale;
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
  readonly automatic_display_input: boolean;
  allowContinuity(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `allow-continuity` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  allowContinuityMany(items: AllowContinuityArgs[]): Promise<tilo.ActionAnswer[]>;
  allowCapability(args: {
    capability: string;
    on: boolean;
  }): Promise<void>;
  /** Runs `allow-capability` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  allowCapabilityMany(items: AllowCapabilityArgs[]): Promise<tilo.ActionAnswer[]>;
  setAutomaticDisplayInput(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set-automatic-display-input` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setAutomaticDisplayInputMany(items: SetAutomaticDisplayInputArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Continuity` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONTINUITY_ACTIONS = ["allowContinuity", "allowContinuityMany", "allowCapability", "allowCapabilityMany", "setAutomaticDisplayInput", "setAutomaticDisplayInputMany"] as const;

/** The `settings.continuity` kind at `path`. */
export function continuity(ws: tilo.Workspace, path: string): Continuity {
  return resourceProxy(ws, path, SURFACE, CONTINUITY_ACTIONS, true) as unknown as Continuity;
}

/** Keyboard shortcuts — `settings.shortcuts`. */
export interface Shortcuts extends tilo.Resource {
  readonly bindings: unknown;
  readonly actions: unknown;
  readonly commands: unknown;
  rebind(args: {
    shortcut: string;
    chord: string;
  }): Promise<void>;
  /** Runs `rebind` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rebindMany(items: RebindArgs[]): Promise<tilo.ActionAnswer[]>;
  reset(args: {
    shortcuts: string[];
  }): Promise<void>;
  /** Runs `reset` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resetMany(items: ResetArgs[]): Promise<tilo.ActionAnswer[]>;
  resetAll(): Promise<void>;
  /** Runs `reset-all` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resetAllMany(items: ResetAllArgs[]): Promise<tilo.ActionAnswer[]>;
  rebindCommand(args: {
    app: string;
    action: string;
    chord: string;
  }): Promise<void>;
  /** Runs `rebind-command` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rebindCommandMany(items: RebindCommandArgs[]): Promise<tilo.ActionAnswer[]>;
  resetCommand(args: {
    app: string;
    action: string;
  }): Promise<void>;
  /** Runs `reset-command` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resetCommandMany(items: ResetCommandArgs[]): Promise<tilo.ActionAnswer[]>;
  setActionChords(args: {
    chords: unknown;
  }): Promise<void>;
  /** Runs `set-action-chords` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setActionChordsMany(items: SetActionChordsArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Shortcuts` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHORTCUTS_ACTIONS = ["rebind", "rebindMany", "reset", "resetMany", "resetAll", "resetAllMany", "rebindCommand", "rebindCommandMany", "resetCommand", "resetCommandMany", "setActionChords", "setActionChordsMany"] as const;

/** The `settings.shortcuts` kind at `path`. */
export function shortcuts(ws: tilo.Workspace, path: string): Shortcuts {
  return resourceProxy(ws, path, SURFACE, SHORTCUTS_ACTIONS, true) as unknown as Shortcuts;
}

/** Shell views — `shell-views`. */
export interface ShellViews extends tilo.Resource {
  readonly bindings: Record<string, Binding>;
  readonly on_device: Record<string, Record<string, Binding>>;
  bind(args: {
    part: string;
    app: string;
    view: string;
    on?: string;
  }): Promise<void>;
  /** Runs `bind` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  bindMany(items: BindArgs[]): Promise<tilo.ActionAnswer[]>;
  reset(args: {
    part: string;
    on?: string;
  }): Promise<void>;
  /** Runs `reset` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resetMany(items: ShellViewsResetArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `ShellViews` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHELLVIEWS_ACTIONS = ["bind", "bindMany", "reset", "resetMany"] as const;

/** The `shell-views` kind at `path`. */
export function shellViews(ws: tilo.Workspace, path: string): ShellViews {
  return resourceProxy(ws, path, SURFACE, SHELLVIEWS_ACTIONS, true) as unknown as ShellViews;
}

/** Desktop controls — `settings.desktop-controls`. */
export interface DesktopControls extends tilo.Resource {
  vim_mode: boolean;
}

/** The actions `DesktopControls` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DESKTOPCONTROLS_ACTIONS = [] as const;

/** The `settings.desktop-controls` kind at `path`. */
export function desktopControls(ws: tilo.Workspace, path: string): DesktopControls {
  return resourceProxy(ws, path, SURFACE, DESKTOPCONTROLS_ACTIONS, true) as unknown as DesktopControls;
}

/** Desktop controls on this device — `settings.desktop-controls-device`. */
export interface DesktopControlsDevice extends tilo.Resource {
  hide_top_bar: boolean;
}

/** The actions `DesktopControlsDevice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DESKTOPCONTROLSDEVICE_ACTIONS = [] as const;

/** The `settings.desktop-controls-device` kind at `path`. */
export function desktopControlsDevice(ws: tilo.Workspace, path: string): DesktopControlsDevice {
  return resourceProxy(ws, path, SURFACE, DESKTOPCONTROLSDEVICE_ACTIONS, true) as unknown as DesktopControlsDevice;
}

/** Agent model providers — `settings.agent-providers`. */
export interface AgentProviders extends tilo.Resource {
  readonly config: ProviderConfig;
  setProviders(args: {
    config: unknown;
  }): Promise<void>;
  /** Runs `set-providers` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setProvidersMany(items: SetProvidersArgs[]): Promise<tilo.ActionAnswer[]>;
  followLocalModels(args: {
    profile: unknown;
  }): Promise<void>;
  /** Runs `follow-local-models` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  followLocalModelsMany(items: FollowLocalModelsArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `AgentProviders` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const AGENTPROVIDERS_ACTIONS = ["setProviders", "setProvidersMany", "followLocalModels", "followLocalModelsMany"] as const;

/** The `settings.agent-providers` kind at `path`. */
export function agentProviders(ws: tilo.Workspace, path: string): AgentProviders {
  return resourceProxy(ws, path, SURFACE, AGENTPROVIDERS_ACTIONS, true) as unknown as AgentProviders;
}

/** Hosted agent model providers — `settings.hosted-agent-providers`. */
export interface HostedAgentProviders extends tilo.Resource {
  readonly hosted: HostedProfiles;
  setProviders(args: {
    hosted: unknown;
  }): Promise<void>;
  /** Runs `set-providers` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setProvidersMany(items: HostedAgentProvidersSetProvidersArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `HostedAgentProviders` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HOSTEDAGENTPROVIDERS_ACTIONS = ["setProviders", "setProvidersMany"] as const;

/** The `settings.hosted-agent-providers` kind at `path`. */
export function hostedAgentProviders(ws: tilo.Workspace, path: string): HostedAgentProviders {
  return resourceProxy(ws, path, SURFACE, HOSTEDAGENTPROVIDERS_ACTIONS, true) as unknown as HostedAgentProviders;
}

/** Default apps — `settings.default-apps`. */
export interface DefaultApps extends tilo.Resource {
  readonly handlers: Record<string, string>;
  setDefaultAppHandler(args: {
    kind: string;
    app: string;
  }): Promise<void>;
  /** Runs `set-default-app-handler` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setDefaultAppHandlerMany(items: SetDefaultAppHandlerArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `DefaultApps` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DEFAULTAPPS_ACTIONS = ["setDefaultAppHandler", "setDefaultAppHandlerMany"] as const;

/** The `settings.default-apps` kind at `path`. */
export function defaultApps(ws: tilo.Workspace, path: string): DefaultApps {
  return resourceProxy(ws, path, SURFACE, DEFAULTAPPS_ACTIONS, true) as unknown as DefaultApps;
}

/** Clock — `settings.clock`. */
export interface Clock extends tilo.Resource {
  automatic_zone: boolean;
}

/** The actions `Clock` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CLOCK_ACTIONS = [] as const;

/** The `settings.clock` kind at `path`. */
export function clock(ws: tilo.Workspace, path: string): Clock {
  return resourceProxy(ws, path, SURFACE, CLOCK_ACTIONS, true) as unknown as Clock;
}

/** Display change — `settings.display-change`. */
export interface DisplayChange extends tilo.Resource {
  readonly id: number;
  readonly state: unknown;
  readonly deadline_unix_ms: number | null;
  readonly message: string | null;
  request(args: {
    change: DisplayTransaction;
  }): Promise<void>;
  /** Runs `request` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  requestMany(items: RequestArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `DisplayChange` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DISPLAYCHANGE_ACTIONS = ["request", "requestMany"] as const;

/** The `settings.display-change` kind at `path`. */
export function displayChange(ws: tilo.Workspace, path: string): DisplayChange {
  return resourceProxy(ws, path, SURFACE, DISPLAYCHANGE_ACTIONS, true) as unknown as DisplayChange;
}

/** On-screen keyboard layouts — `settings.osk-sources`. */
export interface OskSources extends tilo.Resource {
  readonly layout: string;
  readonly layouts: string[];
  setLayouts(args: {
    layouts: string[];
    active: string;
  }): Promise<void>;
  /** Runs `set-layouts` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setLayoutsMany(items: OskSourcesSetLayoutsArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `OskSources` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OSKSOURCES_ACTIONS = ["setLayouts", "setLayoutsMany"] as const;

/** The `settings.osk-sources` kind at `path`. */
export function oskSources(ws: tilo.Workspace, path: string): OskSources {
  return resourceProxy(ws, path, SURFACE, OSKSOURCES_ACTIONS, true) as unknown as OskSources;
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
  }): Promise<void>;
  /** Runs `route-through` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  routeThroughMany(items: RouteThroughArgs[]): Promise<tilo.ActionAnswer[]>;
  setKillSwitch(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set-kill-switch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setKillSwitchMany(items: SetKillSwitchArgs[]): Promise<tilo.ActionAnswer[]>;
  // the app's `stop` action is named as a field of this kind: run it with
  // `action("stop")`.
  setProfileKillSwitch(args: {
    profile: string;
    on: boolean;
    allow_local?: boolean;
  }): Promise<void>;
  /** Runs `set-profile-kill-switch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setProfileKillSwitchMany(items: SetProfileKillSwitchArgs[]): Promise<tilo.ActionAnswer[]>;
  releaseProfile(args: {
    profile: string;
  }): Promise<void>;
  /** Runs `release-profile` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  releaseProfileMany(items: ReleaseProfileArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `DeviceVpn` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DEVICEVPN_ACTIONS = ["routeThrough", "routeThroughMany", "setKillSwitch", "setKillSwitchMany", "setProfileKillSwitch", "setProfileKillSwitchMany", "releaseProfile", "releaseProfileMany"] as const;

/** The `settings.device-vpn` kind at `path`. */
export function deviceVpn(ws: tilo.Workspace, path: string): DeviceVpn {
  return resourceProxy(ws, path, SURFACE, DEVICEVPN_ACTIONS, true) as unknown as DeviceVpn;
}

/** Your colour — `settings.identity`. */
export interface Identity extends tilo.Resource {
  colour: string | null;
}

/** The actions `Identity` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const IDENTITY_ACTIONS = [] as const;

/** The `settings.identity` kind at `path`. */
export function identity(ws: tilo.Workspace, path: string): Identity {
  return resourceProxy(ws, path, SURFACE, IDENTITY_ACTIONS, true) as unknown as Identity;
}

/** Data usage — `settings.network-usage`. */
export interface NetworkUsage extends tilo.Resource {
  reset_day: number;
  limit_gb: number;
  readonly connections: Record<string, ConnectionUsage>;
  readonly counters: Record<string, Bytes>;
  readonly boot: string | null;
  countDataUsed(args: {
    readings: Reading[];
    at: number;
    offset: number;
    boot?: string;
  }): Promise<void>;
  /** Runs `count-data-used` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  countDataUsedMany(items: CountDataUsedArgs[]): Promise<tilo.ActionAnswer[]>;
  resetDataUsed(args?: {
    connection?: string;
  }): Promise<void>;
  /** Runs `reset-data-used` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resetDataUsedMany(items: ResetDataUsedArgs[]): Promise<tilo.ActionAnswer[]>;
  forgetDataUsed(args?: {
    connection?: string;
  }): Promise<void>;
  /** Runs `forget-data-used` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forgetDataUsedMany(items: ForgetDataUsedArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `NetworkUsage` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NETWORKUSAGE_ACTIONS = ["countDataUsed", "countDataUsedMany", "resetDataUsed", "resetDataUsedMany", "forgetDataUsed", "forgetDataUsedMany"] as const;

/** The `settings.network-usage` kind at `path`. */
export function networkUsage(ws: tilo.Workspace, path: string): NetworkUsage {
  return resourceProxy(ws, path, SURFACE, NETWORKUSAGE_ACTIONS, true) as unknown as NetworkUsage;
}

/** Agent model — `settings.agent-models`. */
export interface AgentModels extends tilo.Resource {
  readonly providers: AgentProviderShown[];
  readonly in_use: AgentModelInUse;
  useModel(args: {
    provider: string;
    model: string;
  }): Promise<void>;
  undoModel(): Promise<void>;
  redoModel(): Promise<void>;
}

/** The actions `AgentModels` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const AGENTMODELS_ACTIONS = ["useModel", "undoModel", "redoModel"] as const;

/** The `settings.agent-models` kind at `path`. */
export function agentModels(ws: tilo.Workspace, path: string): AgentModels {
  return resourceProxy(ws, path, SURFACE, AGENTMODELS_ACTIONS) as unknown as AgentModels;
}

/** Keyboard shortcuts — `settings.shortcuts-table`. */
export interface ShortcutsTable extends tilo.Resource {
  readonly bindings: ShortcutChord[];
  readonly actions: ActionShortcut[];
  readonly commands: AppCommand[];
}

/** The actions `ShortcutsTable` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHORTCUTSTABLE_ACTIONS = [] as const;

/** The `settings.shortcuts-table` kind at `path`. */
export function shortcutsTable(ws: tilo.Workspace, path: string): ShortcutsTable {
  return resourceProxy(ws, path, SURFACE, SHORTCUTSTABLE_ACTIONS) as unknown as ShortcutsTable;
}

/** Languages — `settings.languages`. */
export interface Languages extends tilo.Resource {
  readonly languages: Language[];
}

/** The actions `Languages` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LANGUAGES_ACTIONS = [] as const;

/** The `settings.languages` kind at `path`. */
export function languages(ws: tilo.Workspace, path: string): Languages {
  return resourceProxy(ws, path, SURFACE, LANGUAGES_ACTIONS) as unknown as Languages;
}

/** Tablet controls — `settings.tablet-choices`. */
export interface TabletChoices extends tilo.Resource {
  readonly tablets: Record<string, TabletChoiceIds>;
}

/** The actions `TabletChoices` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TABLETCHOICES_ACTIONS = [] as const;

/** The `settings.tablet-choices` kind at `path`. */
export function tabletChoices(ws: tilo.Workspace, path: string): TabletChoices {
  return resourceProxy(ws, path, SURFACE, TABLETCHOICES_ACTIONS) as unknown as TabletChoices;
}

