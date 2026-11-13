/**
 * Types for Canvas (`so.tilo.canvas`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Artwork` is a `Resource` of the app's `canvas.artwork`, made by its factory with the
 * resource's path (`canvas/<resource>`): its attributes are the app's fields
 * (read-only ones `readonly`) and its methods are the app's actions, named in camel
 * case and taking the arguments the wire takes them under. An action that takes
 * longer answers a `Run`. A record is the object the wire carries, and an enum whose
 * cases carry fields is an object whose tag the app's schema names. An action whose
 * name is one the interface already has is run with `action("…")`, which the file
 * says where it is.
 */

import { resourceProxy } from "tilo-sdk";
import type * as tilo from "tilo-sdk";

/** The version of the surface these types were made from: a workspace whose app
 * publishes another refuses their actions rather than run what they no longer
 * describe. */
export const SURFACE = "so.tilo.canvas@3af12d4a81695ba4";

export interface Adjust {
  exposure: number | null;
  contrast: number | null;
  highlights: number | null;
  shadows: number | null;
  saturation: number | null;
  vibrance: number | null;
  hue: number | null;
  temperature: number | null;
  tint: number | null;
  curve: number[][] | null;
  grayscale: boolean | null;
  invert: boolean | null;
  blur: number | null;
  sharpen: number | null;
  vignette: number | null;
}

export interface Ground {
  texture: string;
  colour: string;
}

export interface Layer {
  id: string;
  name: string;
  group: string;
  order: string;
  medium: string;
  opacity: number;
  visible: boolean;
  texture: number;
  locked: boolean;
  adjust: Adjust | null;
  mask_of: string;
  base: number;
  invert: boolean;
  clip: boolean;
  unlinked: boolean;
}

export interface Mark {
  id: string;
  layer: string;
  order: string;
  name: string;
  shape: Shape;
  style: Style;
  transform: number[] | null;
  group: string;
  clip: string;
  area: string;
}

/** One of rect, ellipse, polygon, line, path, text, stroke, image: an object whose case the app tags it with. */
export type Shape = Record<string, unknown>;

export interface Style {
  pixelated: boolean;
  fill: string | null;
  gradient: unknown | null;
  stroke: string | null;
  stroke_width: number;
  opacity: number | null;
  brush: string | null;
  brush_size: number | null;
  angle: number | null;
  wet: boolean;
  params: Record<string, number>;
  dash: number[];
  cap: string | null;
  join: string | null;
  blend: string | null;
  shadow: unknown | null;
  inner_shadow: unknown | null;
  grain: number | null;
  feather: number | null;
  adjust: Adjust | null;
}

export interface Recent {
  path: string;
  title: string;
  width: number;
  height: number;
  at_ms: number;
  thumbnail: string | null;
}

/** Artwork — `canvas.artwork`. */
export interface Artwork extends tilo.Resource {
  title: string;
  width: number;
  height: number;
  ground: Ground;
  layers: Layer[];
  marks: Mark[];
  media: Record<string, unknown>;
  reference: unknown | null;
  selection: unknown | null;
  paint(args: {
    strokes: unknown[];
    layer?: string;
    group?: string;
  }): Promise<unknown | tilo.Run>;
  draw(args: {
    marks: unknown[];
    layer?: string;
  }): Promise<unknown | tilo.Run>;
  chart(args: {
    labels: string[];
    values: number[];
    kind?: string;
    box?: number[];
    max?: number;
    bar_width?: number;
    colour?: string;
    label_size?: number;
    values_shown?: boolean;
    title?: string;
    id?: string;
    layer?: string;
  }): Promise<unknown | tilo.Run>;
  editMarks(args: {
    ids: string[];
    pixelated?: boolean;
    name?: string;
    shape?: unknown;
    dash?: number[];
    cap?: string;
    join?: string;
    blend?: string;
    shadow?: unknown;
    inner_shadow?: unknown;
    grain?: number;
    feather?: number;
    group?: string;
    clip?: string;
    fill?: string;
    gradient?: unknown;
    stroke?: string;
    stroke_width?: number;
    opacity?: number;
    brush?: string;
    size?: number;
    brush_size?: number;
    angle?: number;
    wet?: boolean;
    params?: unknown;
    move_by?: number[];
    scale?: number;
    rotate?: number;
    layer?: string;
    transform?: number[];
    flip?: string;
    crop?: number[];
    straighten?: number;
    adjust?: unknown;
    smooth?: number;
    simplify?: number;
    extend?: number[][];
    split?: number[];
    merge?: boolean;
  }): Promise<tilo.Run | void>;
  removeMarks(args?: {
    ids?: string[];
    layer?: string;
    along?: number[][];
  }): Promise<tilo.Run | void>;
  arrange(args: {
    ids: string[];
    to: string;
    of?: string;
  }): Promise<tilo.Run | void>;
  addLayer(args: {
    name: string;
    id?: string;
    medium?: string;
    opacity?: number;
    above?: string;
    texture?: number;
  }): Promise<tilo.Run | void>;
  editLayer(args: {
    id: string;
    name?: string;
    group?: string;
    medium?: string;
    opacity?: number;
    visible?: boolean;
    locked?: boolean;
    to?: string;
    texture?: number;
    adjust?: unknown;
    mask?: string;
    invert?: boolean;
    clip?: boolean;
  }): Promise<tilo.Run | void>;
  removeLayer(args: {
    id: string;
  }): Promise<tilo.Run | void>;
  setArtwork(args?: {
    title?: string;
    width?: number;
    height?: number;
    texture?: string;
    ground_colour?: string;
    crop?: number[];
    fit?: string;
  }): Promise<tilo.Run | void>;
  saveMedium(args: {
    medium: unknown;
  }): Promise<tilo.Run | void>;
  removeMedium(args: {
    id: string;
  }): Promise<tilo.Run | void>;
  align(args: {
    ids: string[];
    edge?: string;
    distribute?: string;
    to?: string;
  }): Promise<tilo.Run | void>;
  setReference(args?: {
    image?: string;
    frame?: number[];
    opacity?: number;
    visible?: boolean;
    remove?: boolean;
  }): Promise<tilo.Run | void>;
  importSvg(args: {
    svg: string;
    layer?: string;
    group?: string;
    fit?: number[];
  }): Promise<unknown | tilo.Run>;
  openPicture(args: {
    path: string;
    title?: string;
  }): Promise<tilo.Run | void>;
  newArtwork(args?: {
    title?: string;
    size?: string;
    landscape?: boolean;
    width?: number;
    height?: number;
    start?: string;
    texture?: string;
    ground_colour?: string;
    medium?: string;
  }): Promise<tilo.Run | void>;
  openArtwork(args: {
    file: string;
  }): Promise<tilo.Run | void>;
  select(args?: {
    rect?: number[];
    ellipse?: number[];
    points?: number[][];
    d?: string;
    marks?: string[];
    matte?: unknown;
    mode?: string;
    feather?: number;
    grow?: number;
    invert?: boolean;
  }): Promise<unknown | tilo.Run>;
  useSelection(args: {
    do: string;
    layer?: string;
    colour?: string;
    width?: number;
    id?: string;
    defringe?: boolean;
    cleaned?: string[][];
  }): Promise<unknown | tilo.Run>;
  combine(args: {
    ids: string[];
    op: string;
  }): Promise<tilo.Run | void>;
  look(args?: {
    region?: number[];
    size?: number;
    format?: string;
    grid?: boolean;
    layers?: string[];
    version?: string;
    reference?: boolean;
  }): Promise<void>;
  measure(args: {
    ids: string[];
    version?: string;
  }): Promise<void>;
  export(args?: {
    transparent?: boolean;
    scale?: number;
    format?: string;
    quality?: number;
    path?: string;
  }): Promise<void>;
  compareVersions(args: {
    from: string;
    to?: string;
    look?: boolean;
  }): Promise<void>;
  exportSvg(args?: {
    ids?: string[];
    outline_text?: boolean;
    path?: string;
  }): Promise<void>;
  compare(args?: {
    region?: number[];
    mode?: string;
    size?: number;
  }): Promise<void>;
  saveArtwork(args?: {
    path?: string;
  }): Promise<void>;
}

/** The `canvas.artwork` kind at `path`. */
export function artwork(ws: tilo.Workspace, path: string): Artwork {
  return resourceProxy(ws, path, SURFACE) as unknown as Artwork;
}

/** Recent artworks — `canvas.recent-artworks`. */
export interface RecentArtworks extends tilo.Resource {
  devices: Record<string, unknown>;
  remember(args: {
    device: string;
    recent: Recent;
  }): Promise<tilo.Run | void>;
  forget(args: {
    device: string;
    path: string;
  }): Promise<tilo.Run | void>;
}

/** The `canvas.recent-artworks` kind at `path`. */
export function recentArtworks(ws: tilo.Workspace, path: string): RecentArtworks {
  return resourceProxy(ws, path, SURFACE) as unknown as RecentArtworks;
}

