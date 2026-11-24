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
export const SURFACE = "so.tilo.settings@2f47e1185fbaba03";

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
  /** The theme as a file: what exporting it saves, and importing it on another workspace reads. */
  export_theme(args: {
    t: unknown;
  }): Promise<unknown>;
}

/** The actions `TiloSettings` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOSETTINGS_ACTIONS = ["open_pane", "open_paneMany", "show_setting", "show_settingMany", "set_processors_room", "set_memory_room", "export_theme"] as const;

/** The `tl.so.tilo.settings` kind at `path`. */
export function tiloSettings(ws: tilo.Workspace, path: string): TiloSettings {
  return resourceProxy(ws, path, SURFACE, TILOSETTINGS_ACTIONS) as unknown as TiloSettings;
}

