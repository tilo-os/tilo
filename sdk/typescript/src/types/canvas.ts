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
export const SURFACE = "so.tilo.canvas@c1cf5232af16e7da";

export interface AddLayer {
  id: string | null;
  name: string;
  medium: string | null;
  opacity: number | null;
  above: string | null;
  texture: number | null;
}

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

export interface Align {
  ids: string[];
  edge: string | null;
  distribute: string | null;
  to: string | null;
}

export interface Arrange {
  ids: string[];
  to: string;
  of: string | null;
}

export interface Chart {
  labels: string[];
  values: number[];
  kind: string | null;
  box: number[] | null;
  max: number | null;
  bar_width: number | null;
  colour: string | null;
  label_size: number | null;
  values_shown: boolean | null;
  title: string | null;
  id: string | null;
  layer: string | null;
}

export interface Combine {
  ids: string[];
  op: string;
}

export interface Draw {
  layer: string | null;
  marks: NewMark[];
}

export interface EditLayer {
  id: string;
  name: string | null;
  group: string | null;
  medium: string | null;
  opacity: number | null;
  visible: boolean | null;
  locked: boolean | null;
  to: string | null;
  texture: number | null;
  adjust: unknown | null;
  mask: string | null;
  invert: boolean | null;
  clip: boolean | null;
}

export interface EditMarks {
  pixelated: boolean | null;
  ids: string[];
  name: string | null;
  shape: unknown | null;
  dash: number[] | null;
  cap: string | null;
  join: string | null;
  blend: string | null;
  shadow: unknown | null;
  inner_shadow: unknown | null;
  grain: number | null;
  feather: number | null;
  group: string | null;
  clip: string | null;
  fill: string | null;
  gradient: unknown | null;
  stroke: string | null;
  stroke_width: number | null;
  opacity: number | null;
  brush: string | null;
  size: number | null;
  brush_size: number | null;
  angle: number | null;
  wet: boolean | null;
  params: Record<string, number> | null;
  move_by: number[] | null;
  scale: number | null;
  rotate: number | null;
  layer: string | null;
  transform: number[] | null;
  flip: string | null;
  crop: number[] | null;
  straighten: number | null;
  adjust: Adjust | null;
  smooth: number | null;
  simplify: number | null;
  extend: number[][] | null;
  split: number[] | null;
  merge: boolean | null;
}

export interface Gradient {
  kind: string;
  from: number[];
  to: number[];
  stops: unknown[];
  focal: number[] | null;
}

export interface Ground {
  texture: string;
  colour: string;
}

export interface ImportSvg {
  svg: string;
  layer: string | null;
  group: string | null;
  fit: number[] | null;
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

export interface Matte {
  src: string;
  rect: number[];
}

export interface MediumSpec {
  id: string;
  label: string;
  engine: string;
  description: string;
  params: Record<string, number>;
  ground: string | null;
  palette: PigmentSpec[];
}

export interface NewArtwork {
  title: string | null;
  size: string | null;
  landscape: boolean | null;
  width: number | null;
  height: number | null;
  start: string | null;
  texture: string | null;
  ground_colour: string | null;
  medium: string | null;
}

export interface NewMark {
  pixelated: boolean | null;
  id: string | null;
  name: string | null;
  shape: Shape;
  fill: string | null;
  gradient: Gradient | null;
  stroke: string | null;
  stroke_width: number | null;
  opacity: number | null;
  brush_size: number | null;
  brush: string | null;
  angle: number | null;
  wet: boolean | null;
  params: Record<string, number> | null;
  dash: number[] | null;
  cap: string | null;
  join: string | null;
  blend: string | null;
  shadow: Shadow | null;
  inner_shadow: unknown | null;
  grain: number | null;
  feather: number | null;
  group: string | null;
  clip: string | null;
  transform: number[] | null;
}

export interface NewStroke {
  id: string | null;
  name: string | null;
  points: number[][];
  size: number;
  colour: string;
  brush: string | null;
  opacity: number | null;
  wet: boolean | null;
  params: Record<string, number> | null;
  forks: number | null;
  spread: number | null;
  dabs: number | null;
  scatter: number | null;
  dab_length: number | null;
  hatch: number | null;
  angle: number | null;
  clip: string | null;
}

export interface Open {
  path: string;
  title: string | null;
}

export interface OpenArtwork {
  file: string;
}

export interface Paint {
  layer: string | null;
  strokes: NewStroke[];
  group: string | null;
}

export interface PigmentSpec {
  name: string;
  colour: string;
  granulation: number;
  strength: number;
}

export interface RemoveLayer {
  id: string;
}

export interface RemoveMarks {
  ids: string[];
  layer: string | null;
  along: number[][] | null;
}

export interface RemoveMedium {
  id: string;
}

export interface SaveMedium {
  medium: MediumSpec;
}

export interface Select {
  rect: number[] | null;
  ellipse: number[] | null;
  points: number[][] | null;
  d: string | null;
  marks: string[] | null;
  matte: Matte | null;
  mode: string | null;
  feather: number | null;
  grow: number | null;
  invert: boolean | null;
}

export interface SetArtwork {
  title: string | null;
  width: number | null;
  height: number | null;
  texture: string | null;
  ground_colour: string | null;
  crop: number[] | null;
  fit: string | null;
}

export interface SetReference {
  image: string | null;
  frame: number[] | null;
  opacity: number | null;
  visible: boolean | null;
  remove: boolean | null;
}

export interface Shadow {
  offset: number[];
  blur: number;
  colour: string | null;
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

export interface UseSelection {
  do: string;
  layer: string | null;
  colour: string | null;
  width: number | null;
  id: string | null;
  defringe: boolean | null;
  cleaned: string[][] | null;
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
  params?: Record<string, number>;
  move_by?: number[];
  scale?: number;
  rotate?: number;
  layer?: string;
  transform?: number[];
  flip?: string;
  crop?: number[];
  straighten?: number;
  adjust?: Adjust;
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
  medium: MediumSpec;
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
  layers: Array<tilo.Entry<Layer>>;
  marks: Array<tilo.Entry<Mark>>;
  media: Record<string, unknown>;
  reference: unknown | null;
  selection: unknown | null;
  paint(args: {
    strokes: NewStroke[];
    layer?: string;
    group?: string;
  }): Promise<unknown>;
  draw(args: {
    marks: NewMark[];
    layer?: string;
  }): Promise<unknown>;
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
  }): Promise<unknown>;
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
    params?: Record<string, number>;
    move_by?: number[];
    scale?: number;
    rotate?: number;
    layer?: string;
    transform?: number[];
    flip?: string;
    crop?: number[];
    straighten?: number;
    adjust?: Adjust;
    smooth?: number;
    simplify?: number;
    extend?: number[][];
    split?: number[];
    merge?: boolean;
  }): Promise<void>;
  /** Runs `edit-marks` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  editMarksMany(items: EditMarksArgs[]): Promise<tilo.ActionAnswer[]>;
  removeMarks(args?: {
    ids?: string[];
    layer?: string;
    along?: number[][];
  }): Promise<void>;
  /** Runs `remove-marks` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeMarksMany(items: RemoveMarksArgs[]): Promise<tilo.ActionAnswer[]>;
  arrange(args: {
    ids: string[];
    to: string;
    of?: string;
  }): Promise<void>;
  /** Runs `arrange` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  arrangeMany(items: ArrangeArgs[]): Promise<tilo.ActionAnswer[]>;
  addLayer(args: {
    name: string;
    id?: string;
    medium?: string;
    opacity?: number;
    above?: string;
    texture?: number;
  }): Promise<void>;
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
  }): Promise<void>;
  /** Runs `edit-layer` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  editLayerMany(items: EditLayerArgs[]): Promise<tilo.ActionAnswer[]>;
  removeLayer(args: {
    id: string;
  }): Promise<void>;
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
  }): Promise<void>;
  /** Runs `set-artwork` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setArtworkMany(items: SetArtworkArgs[]): Promise<tilo.ActionAnswer[]>;
  saveMedium(args: {
    medium: MediumSpec;
  }): Promise<void>;
  /** Runs `save-medium` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  saveMediumMany(items: SaveMediumArgs[]): Promise<tilo.ActionAnswer[]>;
  removeMedium(args: {
    id: string;
  }): Promise<void>;
  /** Runs `remove-medium` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeMediumMany(items: RemoveMediumArgs[]): Promise<tilo.ActionAnswer[]>;
  align(args: {
    ids: string[];
    edge?: string;
    distribute?: string;
    to?: string;
  }): Promise<void>;
  /** Runs `align` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  alignMany(items: AlignArgs[]): Promise<tilo.ActionAnswer[]>;
  setReference(args?: {
    image?: string;
    frame?: number[];
    opacity?: number;
    visible?: boolean;
    remove?: boolean;
  }): Promise<void>;
  /** Runs `set-reference` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setReferenceMany(items: SetReferenceArgs[]): Promise<tilo.ActionAnswer[]>;
  importSvg(args: {
    svg: string;
    layer?: string;
    group?: string;
    fit?: number[];
  }): Promise<unknown>;
  openPicture(args: {
    path: string;
    title?: string;
  }): Promise<void>;
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
  }): Promise<void>;
  /** Runs `new-artwork` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  newArtworkMany(items: NewArtworkArgs[]): Promise<tilo.ActionAnswer[]>;
  openArtwork(args: {
    file: string;
  }): Promise<void>;
  /** Runs `open-artwork` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  openArtworkMany(items: OpenArtworkArgs[]): Promise<tilo.ActionAnswer[]>;
  select(args?: {
    rect?: number[];
    ellipse?: number[];
    points?: number[][];
    d?: string;
    marks?: string[];
    matte?: Matte;
    mode?: string;
    feather?: number;
    grow?: number;
    invert?: boolean;
  }): Promise<unknown>;
  useSelection(args: {
    do: string;
    layer?: string;
    colour?: string;
    width?: number;
    id?: string;
    defringe?: boolean;
    cleaned?: string[][];
  }): Promise<unknown>;
  combine(args: {
    ids: string[];
    op: string;
  }): Promise<void>;
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

/** The actions `Artwork` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ARTWORK_ACTIONS = ["paint", "draw", "chart", "editMarks", "editMarksMany", "removeMarks", "removeMarksMany", "arrange", "arrangeMany", "addLayer", "addLayerMany", "editLayer", "editLayerMany", "removeLayer", "removeLayerMany", "setArtwork", "setArtworkMany", "saveMedium", "saveMediumMany", "removeMedium", "removeMediumMany", "align", "alignMany", "setReference", "setReferenceMany", "importSvg", "openPicture", "openPictureMany", "newArtwork", "newArtworkMany", "openArtwork", "openArtworkMany", "select", "useSelection", "combine", "combineMany", "look", "measure", "export", "compareVersions", "exportSvg", "compare", "saveArtwork"] as const;

/** The `canvas.artwork` kind at `path`. */
export function artwork(ws: tilo.Workspace, path: string): Artwork {
  return resourceProxy(ws, path, SURFACE, ARTWORK_ACTIONS) as unknown as Artwork;
}

/** Recent artworks — `canvas.recent-artworks`. */
export interface RecentArtworks extends tilo.Resource {
  devices: Record<string, unknown>;
  remember(args: {
    device: string;
    recent: Recent;
  }): Promise<void>;
  /** Runs `remember` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  rememberMany(items: RememberArgs[]): Promise<tilo.ActionAnswer[]>;
  forget(args: {
    device: string;
    path: string;
  }): Promise<void>;
  /** Runs `forget` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  forgetMany(items: ForgetArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `RecentArtworks` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RECENTARTWORKS_ACTIONS = ["remember", "rememberMany", "forget", "forgetMany"] as const;

/** The `canvas.recent-artworks` kind at `path`. */
export function recentArtworks(ws: tilo.Workspace, path: string): RecentArtworks {
  return resourceProxy(ws, path, SURFACE, RECENTARTWORKS_ACTIONS) as unknown as RecentArtworks;
}

