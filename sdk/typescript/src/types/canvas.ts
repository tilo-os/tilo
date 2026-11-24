/**
 * Types for Canvas (`so.tilo.canvas`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Artwork` is a `Resource` of the app's `tl.so.tilo.canvas.Artwork`, made by its factory with the
 * resource's path (`canvas/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.canvas@34cab672aa7431c3";

/** The arguments of `rename_layer`. */
export interface RenameLayerArgs {
  layer: Layer;
  name: string;
}

/** The arguments of `lock_layer`. */
export interface LockLayerArgs {
  layer: Layer;
  locked?: boolean;
}

/** The arguments of `move_layer_up`. */
export interface MoveLayerUpArgs {
  layer: Layer;
}

/** The arguments of `move_layer_down`. */
export interface MoveLayerDownArgs {
  layer: Layer;
}

/** The arguments of `merge_layer_down`. */
export interface MergeLayerDownArgs {
  layer: Layer;
}

/** The arguments of `show_layer`. */
export interface ShowLayerArgs {
  layer: Layer;
  visible: boolean;
}

/** The arguments of `remove_layer`. */
export interface RemoveLayerArgs {
  layer: Layer;
}

/** The arguments of `set_artwork`. */
export interface SetArtworkArgs {
  title?: string;
  width?: number;
  height?: number;
}

/** The arguments of `extend_stroke`. */
export interface ExtendStrokeArgs {
  m: Mark;
  point: number[];
}

/** The arguments of `remove_mark`. */
export interface RemoveMarkArgs {
  m: Mark;
}

/** The arguments of `recolour`. */
export interface RecolourArgs {
  m: Mark;
  colour: string;
}

/** The arguments of `resize_mark`. */
export interface ResizeMarkArgs {
  m: Mark;
  size: number;
}

/** The arguments of `select_area`. */
export interface SelectAreaArgs {
  d: string;
  feather?: number;
}

/** The arguments of `clear_selection`. */
export interface ClearSelectionArgs {
}

/** The arguments of `draw_look`. */
export interface DrawLookArgs {
  size?: number;
}

/** The arguments of `clear_notice`. */
export interface ClearNoticeArgs {
}

/** The arguments of `start_paint`. */
export interface StartPaintArgs {
  tool: string;
  brush: string;
  point: number[];
  size: number;
  colour: string;
  layer?: Layer;
}

/** Artwork — `tl.so.tilo.canvas.Artwork`. */
export interface Artwork extends tilo.Resource {
  zp2ztcwq: string;  // title
  izpefh4x: number;  // width
  "5ui5uh6r": number;  // height
  fh35sews: string;  // ground_texture
  zdv3lz52: string;  // ground_colour
  xu4h6ew2: Array<tilo.Entry<Layer>>;  // layers
  btwv4xq5: Array<tilo.Entry<Mark>>;  // marks
  "4wmc4l7b": Array<tilo.Entry<Picture>>;  // pictures
  ob6rlf7s: Area | null;  // selection
  k2slfiuy: string;  // look
  meooqdog: string;  // notice
  /** Adds a layer on top of the artwork. */
  add_layer(args?: {
    name?: string;
  }): Promise<Layer>;
  /** Renames a layer. Two layers never share a name: a mark names its layer by it. */
  rename_layer(args: {
    layer: Layer;
    name: string;
  }): Promise<void>;
  /** Runs `rename_layer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_layerMany(items: RenameLayerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Holds a layer still, so the pointer passes through its marks, or lets its marks be picked again. */
  lock_layer(args: {
    layer: Layer;
    locked?: boolean;
  }): Promise<void>;
  /** Runs `lock_layer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  lock_layerMany(items: LockLayerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a layer one place up in the stack. */
  move_layer_up(args: {
    layer: Layer;
  }): Promise<void>;
  /** Runs `move_layer_up` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_layer_upMany(items: MoveLayerUpArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a layer one place down in the stack. */
  move_layer_down(args: {
    layer: Layer;
  }): Promise<void>;
  /** Runs `move_layer_down` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_layer_downMany(items: MoveLayerDownArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies a layer and everything on it, above the original. */
  duplicate_layer(args: {
    layer: Layer;
  }): Promise<Layer>;
  /** Folds a layer into the one under it: its marks join that layer, and it is gone. */
  merge_layer_down(args: {
    layer: Layer;
  }): Promise<void>;
  /** Runs `merge_layer_down` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  merge_layer_downMany(items: MergeLayerDownArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows or hides a layer. */
  show_layer(args: {
    layer: Layer;
    visible: boolean;
  }): Promise<void>;
  /** Runs `show_layer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_layerMany(items: ShowLayerArgs[]): Promise<tilo.ActionAnswer[]>;
  remove_layer(args: {
    layer: Layer;
  }): Promise<void>;
  /** Runs `remove_layer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_layerMany(items: RemoveLayerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets what the artwork is called and how big it is. */
  set_artwork(args?: {
    title?: string;
    width?: number;
    height?: number;
  }): Promise<void>;
  /** Runs `set_artwork` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_artworkMany(items: SetArtworkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Lays one brush stroke where the hand went, its point `[x, y, pressure]` in artwork units. */
  add_stroke(args: {
    point: number[];
    size: number;
    colour: string;
    brush: string;
    layer?: Layer;
  }): Promise<Mark>;
  /** Adds the next point to the stroke being painted. */
  extend_stroke(args: {
    m: Mark;
    point: number[];
  }): Promise<void>;
  /** Runs `extend_stroke` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  extend_strokeMany(items: ExtendStrokeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The layer a stroke lands on when none is chosen: the top one that shows and is not held still. An artwork whose layers are all hidden or locked gains one to paint on. */
  top_layer(): Promise<Layer>;
  /** Draws a shape, its SVG path data in artwork units. */
  add_shape(args: {
    d: string;
    colour: string;
    layer?: Layer;
  }): Promise<Mark>;
  remove_mark(args: {
    m: Mark;
  }): Promise<void>;
  /** Runs `remove_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_markMany(items: RemoveMarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Recolours a mark. */
  recolour(args: {
    m: Mark;
    colour: string;
  }): Promise<void>;
  /** Runs `recolour` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recolourMany(items: RecolourArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes a mark bigger or smaller. */
  resize_mark(args: {
    m: Mark;
    size: number;
  }): Promise<void>;
  /** Runs `resize_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resize_markMany(items: ResizeMarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Places a picture the person chose on the top layer, fitted inside the artwork and centred: the picture stays its own object, so it moves, crops and syncs like paint. */
  place_picture(args: {
    file: unknown;
  }): Promise<Mark>;
  /** Selects an area, its outline as SVG path data. */
  select_area(args: {
    d: string;
    feather?: number;
  }): Promise<void>;
  /** Runs `select_area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  select_areaMany(items: SelectAreaArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Lets go of the selection. */
  clear_selection(): Promise<void>;
  /** Runs `clear_selection` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_selectionMany(items: ClearSelectionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Fills the selection as a path mark on a layer. */
  fill_selection(args: {
    colour: string;
    layer?: Layer;
  }): Promise<Mark | null>;
  /** The picture of the artwork as it stands, kept on it, so the surface shows it under the hand. */
  draw_look(args?: {
    size?: number;
  }): Promise<void>;
  /** Runs `draw_look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  draw_lookMany(items: DrawLookArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Writes the artwork at full size as a PNG, JPEG or WebP picture. */
  export_picture(args?: {
    format?: string;
    scale?: number;
  }): Promise<unknown | null>;
  /** Writes the artwork as an SVG document. */
  export_svg(): Promise<string>;
  /** Saves the artwork as a `.artwork` package carrying its pictures. */
  save_artwork(): Promise<unknown | null>;
  /** Imports an SVG document as marks on a layer: each shape the device's drawing engine read from it becomes a mark, so the import is drawn, saved and shown like paint the person laid. */
  import_svg(args: {
    markup: string;
  }): Promise<string>;
  /** Clears the notice a person has read. */
  clear_notice(): Promise<void>;
  /** Runs `clear_notice` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_noticeMany(items: ClearNoticeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts what the hand lays down at `point`: a stroke with the paint loaded, or nothing when the tool draws a shape, whose outline only exists once the hand lifts. It lands on `layer`, the one the person chose, or the top one that takes paint. */
  start_paint(args: {
    tool: string;
    brush: string;
    point: number[];
    size: number;
    colour: string;
    layer?: Layer;
  }): Promise<void>;
  /** Runs `start_paint` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  start_paintMany(items: StartPaintArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the next point of the stroke being painted. A shape tool paints nothing while the hand moves: its outline is drawn when the hand lifts. */
  continue_paint(args: {
    tool: string;
    brush: string;
    point: number[];
    size: number;
    colour: string;
  }): Promise<Mark | null>;
  /** Ends what the hand laid down at `point`: the stroke takes its last point and closes, or a shape is drawn across the drag that made it. */
  finish_paint(args: {
    tool: string;
    brush: string;
    from: number[];
    point: number[];
    size: number;
    colour: string;
    layer?: Layer;
  }): Promise<Mark | null>;
}

/** The actions `Artwork` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ARTWORK_ACTIONS = ["add_layer", "rename_layer", "rename_layerMany", "lock_layer", "lock_layerMany", "move_layer_up", "move_layer_upMany", "move_layer_down", "move_layer_downMany", "duplicate_layer", "merge_layer_down", "merge_layer_downMany", "show_layer", "show_layerMany", "remove_layer", "remove_layerMany", "set_artwork", "set_artworkMany", "add_stroke", "extend_stroke", "extend_strokeMany", "top_layer", "add_shape", "remove_mark", "remove_markMany", "recolour", "recolourMany", "resize_mark", "resize_markMany", "place_picture", "select_area", "select_areaMany", "clear_selection", "clear_selectionMany", "fill_selection", "draw_look", "draw_lookMany", "export_picture", "export_svg", "save_artwork", "import_svg", "clear_notice", "clear_noticeMany", "start_paint", "start_paintMany", "continue_paint", "finish_paint"] as const;

/** The `tl.so.tilo.canvas.Artwork` kind at `path`. */
export function artwork(ws: tilo.Workspace, path: string): Artwork {
  return resourceProxy(ws, path, SURFACE, ARTWORK_ACTIONS) as unknown as Artwork;
}

/** Layer — `tl.so.tilo.canvas.Layer`. */
export interface Layer extends tilo.Resource {
  bplq2q5j: string;  // name
  ilxjjxxz: string;  // medium
  smoxtxmk: number;  // opacity
  evne54yw: boolean;  // visible
  nagaw3zw: boolean;  // locked
  yzvcnjuy: number;  // texture
}

/** The actions `Layer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LAYER_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.Layer` kind at `path`. */
export function layer(ws: tilo.Workspace, path: string): Layer {
  return resourceProxy(ws, path, SURFACE, LAYER_ACTIONS) as unknown as Layer;
}

/** Mark — `tl.so.tilo.canvas.Mark`. */
export interface Mark extends tilo.Resource {
  "3mgqcu4c": string;  // name
  xeahkwbd: Layer;  // layer
  pwxdvgoe: string;  // kind
  "4eq3fpao": number[];  // points
  qxx46dqp: string;  // d
  vmqyvhnc: number;  // size
  fnl5j4dd: string;  // colour
  omem6tkf: string;  // brush
  kytiqfia: number;  // opacity
  y5teghkv: boolean;  // open
  cdvrgjan: string;  // src
  vniyw6t3: number;  // x
  v3wlgndz: number;  // y
  "4nv5sha3": number;  // width
  hlvljkif: number;  // height
}

/** The actions `Mark` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MARK_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.Mark` kind at `path`. */
export function mark(ws: tilo.Workspace, path: string): Mark {
  return resourceProxy(ws, path, SURFACE, MARK_ACTIONS) as unknown as Mark;
}

/** Area — `tl.so.tilo.canvas.Area`. */
export interface Area extends tilo.Resource {
  k26xq4t7: string;  // d
  enongrrr: number;  // feather
}

/** The actions `Area` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const AREA_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.Area` kind at `path`. */
export function area(ws: tilo.Workspace, path: string): Area {
  return resourceProxy(ws, path, SURFACE, AREA_ACTIONS) as unknown as Area;
}

/** Picture — `tl.so.tilo.canvas.Picture`. */
export interface Picture extends tilo.Resource {
  di5h6qir: string;  // name
  "4be6ojwm": string;  // url
  rp5ikqj7: number;  // width
  "4r7lfx7t": number;  // height
}

/** The actions `Picture` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PICTURE_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.Picture` kind at `path`. */
export function picture(ws: tilo.Workspace, path: string): Picture {
  return resourceProxy(ws, path, SURFACE, PICTURE_ACTIONS) as unknown as Picture;
}

/** Library — `tl.so.tilo.canvas.Library`. */
export interface Library extends tilo.Resource {
  "5gxbqk6v": Array<tilo.Entry<Artwork>>;  // artworks
  /** Opens a saved artwork from the file a person, Files or another app hands over. */
  open_artwork(args: {
    file: unknown;
  }): Promise<Artwork>;
  /** A blank artwork begun with a problem to show, so a file that cannot be opened still lands where a person can read why. */
  blank_with(args: {
    why: string;
  }): Promise<Artwork>;
  /** Starts a blank artwork with one plain layer. */
  new_artwork(args: {
    title: string;
    width: number;
    height: number;
  }): Promise<Artwork>;
}

/** The actions `Library` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LIBRARY_ACTIONS = ["open_artwork", "blank_with", "new_artwork"] as const;

/** The `tl.so.tilo.canvas.Library` kind at `path`. */
export function library(ws: tilo.Workspace, path: string): Library {
  return resourceProxy(ws, path, SURFACE, LIBRARY_ACTIONS) as unknown as Library;
}

/** LookRequest — `tl.so.tilo.canvas.LookRequest`. */
export interface LookRequest extends tilo.Resource {
  hhu6bz5c: number;  // size
  ubid4c7s: string;  // format
}

/** The actions `LookRequest` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LOOKREQUEST_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.LookRequest` kind at `path`. */
export function lookRequest(ws: tilo.Workspace, path: string): LookRequest {
  return resourceProxy(ws, path, SURFACE, LOOKREQUEST_ACTIONS) as unknown as LookRequest;
}

/** ExportRequest — `tl.so.tilo.canvas.ExportRequest`. */
export interface ExportRequest extends tilo.Resource {
  ottkj6jd: string;  // format
  "5mutx32x": number;  // scale
}

/** The actions `ExportRequest` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EXPORTREQUEST_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.ExportRequest` kind at `path`. */
export function exportRequest(ws: tilo.Workspace, path: string): ExportRequest {
  return resourceProxy(ws, path, SURFACE, EXPORTREQUEST_ACTIONS) as unknown as ExportRequest;
}

/** SvgRequest — `tl.so.tilo.canvas.SvgRequest`. */
export interface SvgRequest extends tilo.Resource {
  c35hmrf6: string[] | null;  // ids
  gqfdwswn: boolean | null;  // outline_text
}

/** The actions `SvgRequest` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SVGREQUEST_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.SvgRequest` kind at `path`. */
export function svgRequest(ws: tilo.Workspace, path: string): SvgRequest {
  return resourceProxy(ws, path, SURFACE, SVGREQUEST_ACTIONS) as unknown as SvgRequest;
}

/** PlaceRequest — `tl.so.tilo.canvas.PlaceRequest`. */
export interface PlaceRequest extends tilo.Resource {
  "2w4n3dr2": string | null;  // layer
  zxu35ztu: string | null;  // group
  ulngw3pf: number[] | null;  // fit
}

/** The actions `PlaceRequest` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLACEREQUEST_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.PlaceRequest` kind at `path`. */
export function placeRequest(ws: tilo.Workspace, path: string): PlaceRequest {
  return resourceProxy(ws, path, SURFACE, PLACEREQUEST_ACTIONS) as unknown as PlaceRequest;
}

/** ArtworkJson — `tl.so.tilo.canvas.ArtworkJson`. */
export interface ArtworkJson extends tilo.Resource {
  j3bzgr3f: string;  // title
  dowdfhi2: number;  // width
  vlrgzghe: number;  // height
  lotrow4q: string;  // ground_texture
  mpyvrw7z: string;  // ground_colour
  xubd3xon: LayerJson[];  // layers
  lowvdtkg: MarkJson[];  // marks
  "5h6vjvk2": AreaJson | null;  // selection
}

/** The actions `ArtworkJson` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ARTWORKJSON_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.ArtworkJson` kind at `path`. */
export function artworkJson(ws: tilo.Workspace, path: string): ArtworkJson {
  return resourceProxy(ws, path, SURFACE, ARTWORKJSON_ACTIONS) as unknown as ArtworkJson;
}

/** LayerJson — `tl.so.tilo.canvas.LayerJson`. */
export interface LayerJson extends tilo.Resource {
  xunw6qmv: string;  // name
  sxka2uxh: string;  // medium
  aka7ql3b: number;  // opacity
  "4h7cxhog": boolean;  // visible
  "2r4nberb": boolean;  // locked
  xlvw3rkz: number;  // texture
}

/** The actions `LayerJson` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LAYERJSON_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.LayerJson` kind at `path`. */
export function layerJson(ws: tilo.Workspace, path: string): LayerJson {
  return resourceProxy(ws, path, SURFACE, LAYERJSON_ACTIONS) as unknown as LayerJson;
}

/** MarkJson — `tl.so.tilo.canvas.MarkJson`. */
export interface MarkJson extends tilo.Resource {
  "67wvmkkq": string;  // name
  jk7lnwfw: LayerJson;  // layer
  xy2goji2: string;  // kind
  ysn6onwt: number[];  // points
  vmvvarxw: string;  // d
  hajhmux3: number;  // size
  gtn2xdwh: string;  // colour
  pqrpx7k3: string;  // brush
  kolk77nz: number;  // opacity
  "7hgp7nbk": string;  // src
  vtafbavc: number;  // x
  el3izuuy: number;  // y
  jzpg25ot: number;  // width
  k7jjvary: number;  // height
}

/** The actions `MarkJson` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MARKJSON_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.MarkJson` kind at `path`. */
export function markJson(ws: tilo.Workspace, path: string): MarkJson {
  return resourceProxy(ws, path, SURFACE, MARKJSON_ACTIONS) as unknown as MarkJson;
}

/** AreaJson — `tl.so.tilo.canvas.AreaJson`. */
export interface AreaJson extends tilo.Resource {
  nxcluiyf: string;  // d
  jxpwvxya: number;  // feather
}

/** The actions `AreaJson` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const AREAJSON_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.AreaJson` kind at `path`. */
export function areaJson(ws: tilo.Workspace, path: string): AreaJson {
  return resourceProxy(ws, path, SURFACE, AREAJSON_ACTIONS) as unknown as AreaJson;
}

/** Canvas — `tl.so.tilo.canvas`. */
export interface Canvas extends tilo.Resource {
}

/** The actions `Canvas` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CANVAS_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas` kind at `path`. */
export function canvas(ws: tilo.Workspace, path: string): Canvas {
  return resourceProxy(ws, path, SURFACE, CANVAS_ACTIONS) as unknown as Canvas;
}

