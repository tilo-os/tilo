/**
 * Types for Settings (`so.tilo.settings`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Settings` is a `Resource` of the app's `tl.so.tilo.settings.Settings`, made by its factory with the
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
export const SURFACE = "so.tilo.settings@b91ff8fb98358e8b";

/** The arguments of `import_theme`. */
export interface ImportThemeArgs {
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

/** Settings — `tl.so.tilo.settings.Settings`. */
export interface Settings extends tilo.Resource {
  vfi4twig: string;  // pane
  /** Takes in a theme saved with Export, from this workspace or another: every role, tuned value and glass shader, once it is admitted and keeps every contrast floor. Importing names it by its own name, so what a person exported under a name comes back under one. */
  import_theme(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `import_theme` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_themeMany(items: ImportThemeArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Settings` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SETTINGS_ACTIONS = ["import_theme", "import_themeMany"] as const;

/** The `tl.so.tilo.settings.Settings` kind at `path`. */
export function settings(ws: tilo.Workspace, path: string): Settings {
  return resourceProxy(ws, path, SURFACE, SETTINGS_ACTIONS) as unknown as Settings;
}

/** Settings — `tl.so.tilo.settings`. */
export interface TiloSettings extends tilo.Resource {
  /** Move the screen picked to where the pointer was let go, in the desktop's own pixels, snapped to a small grid: a drag too small to mean anything is not a change. */
  move_screen(args: {
    screens: unknown;
    picked: number;
    start: number[];
    let_go: number[];
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
  /** The processors this device keeps free for the people at it: the slider's share, made whole. */
  set_processors_room(args: {
    percent: number;
  }): Promise<void>;
  /** The memory this device keeps free for the people at it. */
  set_memory_room(args: {
    percent: number;
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
export const TILOSETTINGS_ACTIONS = ["move_screen", "open_pane", "open_paneMany", "show_setting", "show_settingMany", "set_processors_room", "set_memory_room", "review_contrast", "review_contrastMany", "review_contrast_on", "review_contrast_onMany", "review_contrast_now", "review_contrast_nowMany", "export_theme"] as const;

/** The `tl.so.tilo.settings` kind at `path`. */
export function tiloSettings(ws: tilo.Workspace, path: string): TiloSettings {
  return resourceProxy(ws, path, SURFACE, TILOSETTINGS_ACTIONS) as unknown as TiloSettings;
}

