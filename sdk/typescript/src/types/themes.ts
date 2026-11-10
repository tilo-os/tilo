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
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "@tilo/sdk";
import type * as tilo from "@tilo/sdk";

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

/** Themes — `themes.studio`. */
export interface Studio extends tilo.Resource {
  themes: Theme[];
  newTheme(args: {
    id: string;
    base: string;
  }): Promise<tilo.Run | void>;
  importTheme(args: {
    id: string;
    document: string;
  }): Promise<tilo.Run | void>;
  reset(args: {
    theme: string;
  }): Promise<tilo.Run | void>;
  delete(args: {
    theme: string;
  }): Promise<tilo.Run | void>;
  switchPolarity(args: {
    theme: string;
    dark: boolean;
  }): Promise<tilo.Run | void>;
  export(args: {
    theme: string;
  }): Promise<void>;
}

/** The `themes.studio` kind at `path`. */
export function studio(ws: tilo.Workspace, path: string): Studio {
  return resourceProxy(ws, path) as unknown as Studio;
}

