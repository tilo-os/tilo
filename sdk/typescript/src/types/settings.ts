/**
 * Types for Settings (`so.tilo.settings`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`settings/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.settings@7966432df163e3c6";

/** The arguments of `import_theme`. */
export interface ImportThemeArgs {
  file: unknown;
}

/** The arguments of `set_own_picture`. */
export interface SetOwnPictureArgs {
  file: unknown;
}

/** The arguments of `open_pane`. */
export interface OpenPaneArgs {
  page: string;
}

/** The arguments of `show_setting`. */
export interface ShowSettingArgs {
  setting: string;
}

/** The arguments of `review_contrast`. */
export interface ReviewContrastArgs {
  theme: string;
  role: string;
  value: number;
}

/** The arguments of `review_contrast_on`. */
export interface ReviewContrastOnArgs {
  theme: string;
  role: string;
  on: boolean;
}

/** The arguments of `review_contrast_now`. */
export interface ReviewContrastNowArgs {
  theme: string;
}

/** Notice — `tl.tilo.host.Notice`. */
export interface Notice extends tilo.Resource {
  notice_to: string;  // to
  notice_title: string;  // title
  notice_body: string;  // body
  notice_opens: unknown;  // opens
  notice_seen: boolean;  // seen
  notice_device: string;  // device
  notice_actions: string[];  // actions
}

/** The actions `Notice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const NOTICE_ACTIONS = [] as const;

/** The `tl.tilo.host.Notice` kind at `path`. */
export function notice(ws: tilo.Workspace, path: string): Notice {
  return resourceProxy(ws, path, SURFACE, NOTICE_ACTIONS) as unknown as Notice;
}

/** Settings — `tl.so.tilo.settings.Settings`. */
export interface Settings extends tilo.Resource {
  vfi4twig: string;  // pane
  /** Takes in a theme saved with Export, from this workspace or another: every role, tuned value and glass shader, once it is admitted and keeps every contrast floor. Importing names it by its own name, so what a person exported under a name comes back under one. */
  import_theme(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `import_theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_themeMany(items: ImportThemeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Give this person the picture `file` holds. The session keeps it in their identity record, where every device and every app that shows them reads it. */
  set_own_picture(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `set_own_picture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_own_pictureMany(items: SetOwnPictureArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Settings` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SETTINGS_ACTIONS = ["import_theme", "import_themeMany", "set_own_picture", "set_own_pictureMany"] as const;

/** The `tl.so.tilo.settings.Settings` kind at `path`. */
export function settings(ws: tilo.Workspace, path: string): Settings {
  return resourceProxy(ws, path, SURFACE, SETTINGS_ACTIONS) as unknown as Settings;
}

/** Screen — `tl.so.tilo.settings.Screen`. */
export interface Screen extends tilo.Resource {
  jv7udxtv: string;  // device
  yffzojmk: string;  // device_label
  ygrxnhgi: string;  // output
  pyvmb3xc: string;  // connector
  qkogq64e: string;  // name
  wl63r7vc: number;  // x
  ifqfxckm: number;  // y
  ckht7p7n: number;  // own_x
  mw6cqvcw: number;  // own_y
  yfbxh7yj: number;  // width
  dhxa4orb: number;  // height
  pokqvk7i: boolean;  // local
  jqmgcbem: boolean;  // online
}

/** The actions `Screen` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCREEN_ACTIONS = [] as const;

/** The `tl.so.tilo.settings.Screen` kind at `path`. */
export function settingsScreen(ws: tilo.Workspace, path: string): Screen {
  return resourceProxy(ws, path, SURFACE, SCREEN_ACTIONS) as unknown as Screen;
}

/** ProblemReport — `tl.so.tilo.settings.ProblemReport`. */
export interface ProblemReport extends tilo.Resource {
  kb2c7b7e: string;  // body
}

/** The actions `ProblemReport` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROBLEMREPORT_ACTIONS = [] as const;

/** The `tl.so.tilo.settings.ProblemReport` kind at `path`. */
export function problemReport(ws: tilo.Workspace, path: string): ProblemReport {
  return resourceProxy(ws, path, SURFACE, PROBLEMREPORT_ACTIONS) as unknown as ProblemReport;
}

/** Settings — `tl.so.tilo.settings`. */
export interface TiloSettings extends tilo.Resource {
  /** Move the screen picked to where the pointer let it go. This device's own screens keep the arrangement their desktop gives them; another device's screens move together by the person's shared layout, this device's too when it has only one. A drag too small to mean anything is not a change. */
  move_screen(args: {
    screens: Screen[];
    picked: number;
    start: number[];
    let_go: number[];
  }): Promise<void>;
  /** Set how bright ordinary content looks in HDR, from the slider's value in cd/m². */
  set_sdr_brightness(args: {
    displays: unknown;
    display: string;
    nits: number;
  }): Promise<void>;
  /** Show the pane a search page names, as the search's own row runs it from outside the window. */
  open_pane(args: {
    page: string;
  }): Promise<void>;
  /** Runs `open_pane` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_paneMany(items: OpenPaneArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Show the pane a setting is on, as search's own row for a setting runs it. */
  show_setting(args: {
    setting: string;
  }): Promise<void>;
  /** Runs `show_setting` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_settingMany(items: ShowSettingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the provider's page where a card, a plan and the invoices change, in the app that opens web addresses: the provider's own page, never a form Tilo draws. The page is the one the account names, so nothing an app or an agent passes can open another, and an account that names none opens nothing. */
  open_manage_page(args: {
    account: unknown;
  }): Promise<void>;
  /** Opens one of the account's invoices on the provider's page. The invoice is the account's own, looked up by its id, so only a page the account names opens and an id it does not hold opens nothing. */
  open_invoice(args: {
    account: unknown;
    invoice: string;
  }): Promise<void>;
  /** The processors this device keeps free for the people at it: the slider's share, made whole. */
  set_processors_room(args: {
    percent: number;
  }): Promise<void>;
  /** The memory this device keeps free for the people at it. */
  set_memory_room(args: {
    percent: number;
  }): Promise<void>;
  /** Files this computer's record of a problem as a problem report in the workspace's feedback channel, where the people who make Tilo read what went wrong, with the record's own words. */
  send_problem_report(args: {
    body: string;
  }): Promise<void>;
  /** Lists every contrast floor the theme would miss with `role` at the control's value, so its editor shows each pairing under its floor while the theme is tuned. The theme keeps the roles it admits; a value that would break a floor is refused, and this says which pairs it would break. */
  review_contrast(args: {
    theme: string;
    role: string;
    value: number;
  }): Promise<void>;
  /** Runs `review_contrast` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  review_contrastMany(items: ReviewContrastArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The same after a switch was tried: its on is `1` and its off `0`. */
  review_contrast_on(args: {
    theme: string;
    role: string;
    on: boolean;
  }): Promise<void>;
  /** Runs `review_contrast_on` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  review_contrast_onMany(items: ReviewContrastOnArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Records what the theme misses as it stands, when its editor opens or its polarity changes, before any control has carried a value. */
  review_contrast_now(args: {
    theme: string;
  }): Promise<void>;
  /** Runs `review_contrast_now` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  review_contrast_nowMany(items: ReviewContrastNowArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The theme as a file: what exporting it saves, and importing it on another workspace reads. */
  export_theme(args: {
    t: unknown;
  }): Promise<unknown>;
}

/** The actions `TiloSettings` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOSETTINGS_ACTIONS = ["move_screen", "set_sdr_brightness", "open_pane", "open_paneMany", "show_setting", "show_settingMany", "open_manage_page", "open_invoice", "set_processors_room", "set_memory_room", "send_problem_report", "review_contrast", "review_contrastMany", "review_contrast_on", "review_contrast_onMany", "review_contrast_now", "review_contrast_nowMany", "export_theme"] as const;

/** The `tl.so.tilo.settings` kind at `path`. */
export function tiloSettings(ws: tilo.Workspace, path: string): TiloSettings {
  return resourceProxy(ws, path, SURFACE, TILOSETTINGS_ACTIONS) as unknown as TiloSettings;
}

