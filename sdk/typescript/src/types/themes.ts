/**
 * Types for Theme Studio (`so.tilo.themes`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Studio` is a `Resource` of the app's `themes.studio`, made by its factory with the
 * resource's path (`themes/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.themes@aece4838e9a21ddc";

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

export interface Theme {
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
  origin: string | null;
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

/** The arguments of `reset`. */
export interface ResetArgs {
  theme: string;
}

/** The arguments of `delete`. */
export interface DeleteArgs {
  theme: string;
}

/** The arguments of `switch-polarity`. */
export interface SwitchPolarityArgs {
  theme: string;
  dark: boolean;
}

/** Themes — `themes.studio`. */
export interface Studio extends tilo.Resource {
  themes: Array<tilo.Entry<Theme>>;
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
  reset(args: {
    theme: string;
  }): Promise<void>;
  /** Runs `reset` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resetMany(items: ResetArgs[]): Promise<tilo.ActionAnswer[]>;
  delete(args: {
    theme: string;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  switchPolarity(args: {
    theme: string;
    dark: boolean;
  }): Promise<void>;
  /** Runs `switch-polarity` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  switchPolarityMany(items: SwitchPolarityArgs[]): Promise<tilo.ActionAnswer[]>;
  export(args: {
    theme: string;
  }): Promise<void>;
}

/** The actions `Studio` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STUDIO_ACTIONS = ["newTheme", "newThemeMany", "importTheme", "importThemeMany", "reset", "resetMany", "delete", "deleteMany", "switchPolarity", "switchPolarityMany", "export"] as const;

/** The `themes.studio` kind at `path`. */
export function studio(ws: tilo.Workspace, path: string): Studio {
  return resourceProxy(ws, path, SURFACE, STUDIO_ACTIONS) as unknown as Studio;
}

