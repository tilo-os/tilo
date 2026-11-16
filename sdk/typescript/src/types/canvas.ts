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
 * says where it is. An undoable action that answers nothing also has a `<action>Many`,
which applies it to every argument object of a list in a few all-or-nothing steps,
all of them one run.
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

/** The arguments of `edit-marks`. */
export interface EditMarksArgs {
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
}

/** The arguments of `remove-marks`. */
export interface RemoveMarksArgs {
  ids?: string[];
  layer?: string;
  along?: number[][];
}

/** The arguments of `arrange`. */
export interface ArrangeArgs {
  ids: string[];
  to: string;
  of?: string;
}

/** The arguments of `add-layer`. */
export interface AddLayerArgs {
  name: string;
  id?: string;
  medium?: string;
  opacity?: number;
  above?: string;
  texture?: number;
}

/** The arguments of `edit-layer`. */
export interface EditLayerArgs {
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
}

/** The arguments of `remove-layer`. */
export interface RemoveLayerArgs {
  id: string;
}

/** The arguments of `set-artwork`. */
export interface SetArtworkArgs {
  title?: string;
  width?: number;
  height?: number;
  texture?: string;
  ground_colour?: string;
  crop?: number[];
  fit?: string;
}

/** The arguments of `save-medium`. */
export interface SaveMediumArgs {
  medium: unknown;
}

/** The arguments of `remove-medium`. */
export interface RemoveMediumArgs {
  id: string;
}

/** The arguments of `align`. */
export interface AlignArgs {
  ids: string[];
  edge?: string;
  distribute?: string;
  to?: string;
}

/** The arguments of `set-reference`. */
export interface SetReferenceArgs {
  image?: string;
  frame?: number[];
  opacity?: number;
  visible?: boolean;
  remove?: boolean;
}

/** The arguments of `open-picture`. */
export interface OpenPictureArgs {
  path: string;
  title?: string;
}

/** The arguments of `new-artwork`. */
export interface NewArtworkArgs {
  title?: string;
  size?: string;
  landscape?: boolean;
  width?: number;
  height?: number;
  start?: string;
  texture?: string;
  ground_colour?: string;
  medium?: string;
}

/** The arguments of `open-artwork`. */
export interface OpenArtworkArgs {
  file: string;
}

/** The arguments of `combine`. */
export interface CombineArgs {
  ids: string[];
  op: string;
}

/** The arguments of `remember`. */
export interface RememberArgs {
  device: string;
  recent: Recent;
}

/** The arguments of `forget`. */
export interface ForgetArgs {
  device: string;
  path: string;
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
  /** Runs `edit-marks` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  editMarksMany(items: EditMarksArgs[]): Promise<tilo.ActionAnswer[]>;
  removeMarks(args?: {
    ids?: string[];
    layer?: string;
    along?: number[][];
  }): Promise<tilo.Run | void>;
  /** Runs `remove-marks` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeMarksMany(items: RemoveMarksArgs[]): Promise<tilo.ActionAnswer[]>;
  arrange(args: {
    ids: string[];
    to: string;
    of?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `arrange` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  arrangeMany(items: ArrangeArgs[]): Promise<tilo.ActionAnswer[]>;
  addLayer(args: {
    name: string;
    id?: string;
    medium?: string;
    opacity?: number;
    above?: string;
    texture?: number;
  }): Promise<tilo.Run | void>;
  /** Runs `add-layer` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addLayerMany(items: AddLayerArgs[]): Promise<tilo.ActionAnswer[]>;
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
  /** Runs `edit-layer` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  editLayerMany(items: EditLayerArgs[]): Promise<tilo.ActionAnswer[]>;
  removeLayer(args: {
    id: string;
  }): Promise<tilo.Run | void>;
  /** Runs `remove-layer` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeLayerMany(items: RemoveLayerArgs[]): Promise<tilo.ActionAnswer[]>;
  setArtwork(args?: {
    title?: string;
    width?: number;
    height?: number;
    texture?: string;
    ground_colour?: string;
    crop?: number[];
    fit?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `set-artwork` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setArtworkMany(items: SetArtworkArgs[]): Promise<tilo.ActionAnswer[]>;
  saveMedium(args: {
    medium: unknown;
  }): Promise<tilo.Run | void>;
  /** Runs `save-medium` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  saveMediumMany(items: SaveMediumArgs[]): Promise<tilo.ActionAnswer[]>;
  removeMedium(args: {
    id: string;
  }): Promise<tilo.Run | void>;
  /** Runs `remove-medium` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeMediumMany(items: RemoveMediumArgs[]): Promise<tilo.ActionAnswer[]>;
  align(args: {
    ids: string[];
    edge?: string;
    distribute?: string;
    to?: string;
  }): Promise<tilo.Run | void>;
  /** Runs `align` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  alignMany(items: AlignArgs[]): Promise<tilo.ActionAnswer[]>;
  setReference(args?: {
    image?: string;
    frame?: number[];
    opacity?: number;
    visible?: boolean;
    remove?: boolean;
  }): Promise<tilo.Run | void>;
  /** Runs `set-reference` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setReferenceMany(items: SetReferenceArgs[]): Promise<tilo.ActionAnswer[]>;
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
  /** Runs `open-picture` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  openPictureMany(items: OpenPictureArgs[]): Promise<tilo.ActionAnswer[]>;
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
  /** Runs `new-artwork` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  newArtworkMany(items: NewArtworkArgs[]): Promise<tilo.ActionAnswer[]>;
  openArtwork(args: {
    file: string;
  }): Promise<tilo.Run | void>;
  /** Runs `open-artwork` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  openArtworkMany(items: OpenArtworkArgs[]): Promise<tilo.ActionAnswer[]>;
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
  /** Runs `combine` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  combineMany(items: CombineArgs[]): Promise<tilo.ActionAnswer[]>;
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
  /** Runs `remember` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  rememberMany(items: RememberArgs[]): Promise<tilo.ActionAnswer[]>;
  forget(args: {
    device: string;
    path: string;
  }): Promise<tilo.Run | void>;
  /** Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  forgetMany(items: ForgetArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The `canvas.recent-artworks` kind at `path`. */
export function recentArtworks(ws: tilo.Workspace, path: string): RecentArtworks {
  return resourceProxy(ws, path, SURFACE) as unknown as RecentArtworks;
}

