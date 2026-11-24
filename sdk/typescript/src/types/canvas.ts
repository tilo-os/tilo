/**
 * Types for Canvas (`so.tilo.canvas`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
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
export const SURFACE = "so.tilo.canvas@59ab3d6f91f4513c";

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

/** The arguments of `clip_layer`. */
export interface ClipLayerArgs {
  layer: Layer;
  clip?: boolean;
}

/** The arguments of `set_mask`. */
export interface SetMaskArgs {
  layer: Layer;
  base?: number;
  invert?: boolean;
  unlinked?: boolean;
}

/** The arguments of `set_medium`. */
export interface SetMediumArgs {
  layer: Layer;
  medium: string;
}

/** The arguments of `remove_medium`. */
export interface RemoveMediumArgs {
  m: Medium;
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

/** The arguments of `set_reference`. */
export interface SetReferenceArgs {
  file: unknown;
  opacity?: number;
}

/** The arguments of `clear_reference`. */
export interface ClearReferenceArgs {
}

/** The arguments of `clear_comparison`. */
export interface ClearComparisonArgs {
}

/** The arguments of `pick_mark`. */
export interface PickMarkArgs {
  m: Mark;
  on?: boolean;
}

/** The arguments of `pick_marks`. */
export interface PickMarksArgs {
  marks: Mark[];
}

/** The arguments of `clear_picked`. */
export interface ClearPickedArgs {
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

/** The arguments of `set_layer_group`. */
export interface SetLayerGroupArgs {
  group: string;
}

/** The arguments of `adjust_layer`. */
export interface AdjustLayerArgs {
  exposure?: number;
  contrast?: number;
  highlights?: number;
  shadows?: number;
  saturation?: number;
  vibrance?: number;
  hue?: number;
  temperature?: number;
  tint?: number;
  grayscale?: boolean;
  invert?: boolean;
  blur?: number;
  sharpen?: number;
  vignette?: number;
}

/** The arguments of `group_mark`. */
export interface GroupMarkArgs {
  group: string;
}

/** The arguments of `fill_gradient`. */
export interface FillGradientArgs {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
  offsets: number[];
  colours: string[];
  kind?: string;
}

/** The arguments of `wet_mark`. */
export interface WetMarkArgs {
  wet?: boolean;
}

/** The arguments of `clear_gradient`. */
export interface ClearGradientArgs {
}

/** The arguments of `set_text`. */
export interface SetTextArgs {
  text?: string;
  size?: number;
  colour?: string;
  font?: string;
  weight?: number;
  italic?: boolean;
  align?: string;
  line_height?: number;
  tracking?: number;
  path?: string;
}

/** The arguments of `crop_picture`. */
export interface CropPictureArgs {
  x?: number;
  y?: number;
  width?: number;
  height?: number;
}

/** The arguments of `straighten_picture`. */
export interface StraightenPictureArgs {
  degrees: number;
}

/** The arguments of `flip_picture`. */
export interface FlipPictureArgs {
  horizontal?: boolean;
  vertical?: boolean;
  flip_h?: boolean;
  flip_v?: boolean;
}

/** The arguments of `blend_mark`. */
export interface BlendMarkArgs {
  blend: string;
}

/** The arguments of `adjust_mark`. */
export interface AdjustMarkArgs {
  exposure?: number;
  contrast?: number;
  highlights?: number;
  shadows?: number;
  saturation?: number;
  vibrance?: number;
  hue?: number;
  temperature?: number;
  tint?: number;
  grayscale?: boolean;
  invert?: boolean;
  blur?: number;
  sharpen?: number;
  vignette?: number;
}

/** The arguments of `move_mark`. */
export interface MoveMarkArgs {
  dx: number;
  dy: number;
}

/** The arguments of `scale_mark`. */
export interface ScaleMarkArgs {
  cx: number;
  cy: number;
  factor: number;
}

/** The arguments of `rotate_mark`. */
export interface RotateMarkArgs {
  cx: number;
  cy: number;
  degrees: number;
}

/** The arguments of `clip_mark`. */
export interface ClipMarkArgs {
  by: string;
}

/** The arguments of `finish_mark`. */
export interface FinishMarkArgs {
  pixelated?: boolean;
  grain?: number;
  feather?: number;
  shadow_x?: number;
  shadow_y?: number;
  shadow_blur?: number;
  shadow_colour?: string;
  inner_shadow_x?: number;
  inner_shadow_y?: number;
  inner_shadow_blur?: number;
  inner_shadow_colour?: string;
}

/** The arguments of `set_curve`. */
export interface SetCurveArgs {
  curve: number[][];
}

/** The arguments of `set_dynamics`. */
export interface SetDynamicsArgs {
  flow?: number;
  hardness?: number;
  spacing?: number;
  smoothing?: number;
  taper?: number;
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

/** Artwork — `tl.so.tilo.canvas.Artwork`. */
export interface Artwork extends tilo.Resource {
  zp2ztcwq: string;  // title
  izpefh4x: number;  // width
  "5ui5uh6r": number;  // height
  fh35sews: string;  // ground_texture
  zdv3lz52: string;  // ground_colour
  xu4h6ew2: Array<tilo.Entry<Layer>>;  // layers
  btwv4xq5: Array<tilo.Entry<Mark>>;  // marks
  "4bht3tzh": Array<tilo.Entry<Medium>>;  // media
  "4wmc4l7b": Array<tilo.Entry<Picture>>;  // pictures
  ob6rlf7s: Area | null;  // selection
  hrqejear: Mark[];  // picked
  exp6h3eo: Reference | null;  // reference
  k2slfiuy: string;  // look
  mksntvfu: string;  // comparison
  jmoqryks: string;  // version_note
  fydl4rsk: string;  // version_overlay
  i5yildir: number[];  // version_region
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
  /** Holds a layer to the paint of the one under it, or lets it stand free. */
  clip_layer(args: {
    layer: Layer;
    clip?: boolean;
  }): Promise<void>;
  /** Runs `clip_layer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clip_layerMany(items: ClipLayerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a mask to a layer: a layer of greys the layer is seen through. Its marks are made with the brushes as any layer's are, light showing the layer and dark hiding it. */
  add_mask(args: {
    layer: Layer;
  }): Promise<Layer>;
  /** Sets a mask's grey before any mark, whether it hides where it is light, and whether it stays put when its layer's marks move. */
  set_mask(args: {
    layer: Layer;
    base?: number;
    invert?: boolean;
    unlinked?: boolean;
  }): Promise<void>;
  /** Runs `set_mask` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_maskMany(items: SetMaskArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a text mark: words set in the artwork's own fonts, at `x`, `y` (its first baseline) in artwork units. */
  add_text(args: {
    text: string;
    layer?: Layer;
    x?: number;
    y?: number;
    size?: number;
    colour?: string;
    font?: string;
    weight?: number;
    italic?: boolean;
    align?: string;
    line_height?: number;
    tracking?: number;
    path?: string;
  }): Promise<Mark>;
  /** Paints a layer with a medium: `oil`, `watercolour`, a preset saved in the artwork by its id, or empty for plain vector paint. */
  set_medium(args: {
    layer: Layer;
    medium: string;
  }): Promise<void>;
  /** Runs `set_medium` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_mediumMany(items: SetMediumArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Saves a medium in the artwork, so its layers name it and every device paints it the same. A medium of the same id is replaced. */
  save_medium(args: {
    id: string;
    label: string;
    engine?: string;
    description?: string;
    ground?: string;
    params?: Record<string, number>;
    palette?: Pigment[];
  }): Promise<Medium>;
  remove_medium(args: {
    m: Medium;
  }): Promise<void>;
  /** Runs `remove_medium` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_mediumMany(items: RemoveMediumArgs[]): Promise<tilo.ActionAnswer[]>;
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
  /** Selects what the colour at `at` touches in the artwork's picture, or that colour everywhere: the magic wand. The area comes back as a selection the fill, mask and outline use. */
  select_like(args?: {
    at?: number[];
    colour?: string;
    tolerance?: number;
    everywhere?: boolean;
  }): Promise<string | null>;
  /** Selects the picture's subject, found by the on-device model the person set up, with its edge fitted to the picture's own edges over `refine` units where that is asked. */
  select_subject(args?: {
    refine?: number;
  }): Promise<string | null>;
  /** Refines the selection's edge to the picture's own edges over `refine` units, keeping fur and hair soft. */
  refine_selection(args?: {
    refine?: number;
  }): Promise<string | null>;
  /** Combines the marks into one path, in the bottom one's place and colour: shapes united, subtracted, intersected or excluded. The engine works out the outline; the artwork keeps one mark where there were several. */
  combine_marks(args: {
    marks: Mark[];
    op?: string;
  }): Promise<Mark | null>;
  /** Weighs the marks' ink as the eye sees it: each one's ink box, ink area, centre of mass and its offset from the box's middle, so two icons balance when their areas and offsets match. The answer is the engine's JSON; the window says it in words. */
  measure_marks(args: {
    marks: Mark[];
  }): Promise<string | null>;
  /** Sets the picture to work from, fitted inside the artwork and centred. It is not part of the work: comparing shows the artwork beside, over or against it, and export never includes it. */
  set_reference(args: {
    file: unknown;
    opacity?: number;
  }): Promise<void>;
  /** Runs `set_reference` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_referenceMany(items: SetReferenceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the reference picture away. */
  clear_reference(): Promise<void>;
  /** Runs `clear_reference` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_referenceMany(items: ClearReferenceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the artwork beside (`side-by-side`), over (`overlay`) or against (`difference`) the reference picture, as a new file, with how far apart they are. */
  compare_reference(args?: {
    mode?: string;
    size?: number;
  }): Promise<unknown | null>;
  /** Compares the artwork with the version history names, overlaid on the surface: what changed between the two, and the changed region as it now stands where it stands. */
  compare_versions(args: {
    change: string;
  }): Promise<string | null>;
  /** Takes the version comparison away from the surface. */
  clear_comparison(): Promise<void>;
  /** Runs `clear_comparison` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_comparisonMany(items: ClearComparisonArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the engine for a selection, and keeps the area it answers. */
  ask_selection(args: {
    ask: string;
  }): Promise<string | null>;
  /** Selects a rectangle or an ellipse from one corner of a drag to the other, meeting the area there is by `mode` (`replace`, `add`, `subtract` or `intersect`). */
  select_shape(args: {
    kind: string;
    from: number[];
    to: number[];
    mode?: string;
    feather?: number;
    grow?: number;
    invert?: boolean;
  }): Promise<string | null>;
  /** Selects the area a lasso traces, closed, meeting the area there is by `mode`. */
  select_lasso(args: {
    points: number[][];
    mode?: string;
    feather?: number;
    grow?: number;
    invert?: boolean;
  }): Promise<string | null>;
  /** Selects the area an outline names, meeting the area there is by `mode`. */
  select_outline(args: {
    d: string;
    mode?: string;
    feather?: number;
    grow?: number;
    invert?: boolean;
  }): Promise<string | null>;
  /** Grows (or shrinks, when negative) the selection's edge by `by` units. */
  grow_selection(args: {
    by: number;
  }): Promise<string | null>;
  /** Turns the selection inside out: everything in the artwork but what it held. */
  invert_selection(): Promise<string | null>;
  /** Selects the shape of the marks or groups `ids` name, meeting the area there is by `mode`. */
  select_marks(args: {
    ids: string[];
    mode?: string;
  }): Promise<string | null>;
  /** Picks a mark, or lets it go: combine and measure act on the picked marks. */
  pick_mark(args: {
    m: Mark;
    on?: boolean;
  }): Promise<void>;
  /** Runs `pick_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pick_markMany(items: PickMarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Picks the marks, adding any that were not picked. */
  pick_marks(args: {
    marks: Mark[];
  }): Promise<void>;
  /** Runs `pick_marks` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pick_marksMany(items: PickMarksArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Lets every picked mark go. */
  clear_picked(): Promise<void>;
  /** Runs `clear_picked` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_pickedMany(items: ClearPickedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Removes the brush strokes a path crosses, on `layer` or every layer: what a stroke eraser does. Answers how many went. */
  remove_along(args: {
    points: number[][];
    layer?: Layer;
  }): Promise<number>;
  /** Lists what the artwork holds: its layers and newest marks, with ids, bounds and colours, and with `palette` the colours it paints with. The answer is the engine's JSON. */
  inspect_artwork(args?: {
    palette?: boolean;
    limit?: number;
    layer?: string;
    region?: number[];
  }): Promise<string | null>;
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
    lasso?: number[][];
  }): Promise<Mark | null>;
}

/** The actions `Artwork` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ARTWORK_ACTIONS = ["add_layer", "rename_layer", "rename_layerMany", "lock_layer", "lock_layerMany", "clip_layer", "clip_layerMany", "add_mask", "set_mask", "set_maskMany", "add_text", "set_medium", "set_mediumMany", "save_medium", "remove_medium", "remove_mediumMany", "move_layer_up", "move_layer_upMany", "move_layer_down", "move_layer_downMany", "duplicate_layer", "merge_layer_down", "merge_layer_downMany", "show_layer", "show_layerMany", "remove_layer", "remove_layerMany", "set_artwork", "set_artworkMany", "add_stroke", "extend_stroke", "extend_strokeMany", "top_layer", "add_shape", "remove_mark", "remove_markMany", "recolour", "recolourMany", "resize_mark", "resize_markMany", "place_picture", "select_like", "select_subject", "refine_selection", "combine_marks", "measure_marks", "set_reference", "set_referenceMany", "clear_reference", "clear_referenceMany", "compare_reference", "compare_versions", "clear_comparison", "clear_comparisonMany", "ask_selection", "select_shape", "select_lasso", "select_outline", "grow_selection", "invert_selection", "select_marks", "pick_mark", "pick_markMany", "pick_marks", "pick_marksMany", "clear_picked", "clear_pickedMany", "remove_along", "inspect_artwork", "select_area", "select_areaMany", "clear_selection", "clear_selectionMany", "fill_selection", "draw_look", "draw_lookMany", "export_picture", "export_svg", "save_artwork", "import_svg", "clear_notice", "clear_noticeMany", "start_paint", "start_paintMany", "continue_paint", "finish_paint"] as const;

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
  cwenklqw: string;  // group
  rxz73i7f: string;  // mask_of
  umkqodij: number;  // base
  owktdb7d: boolean;  // invert
  n43pvuyc: boolean;  // clip
  "2rtbmdqf": boolean;  // unlinked
  zrxehlis: number;  // exposure
  erzbebsl: number;  // contrast
  qcpp23hg: number;  // highlights
  xr5c7gly: number;  // shadows
  cxp7piot: number;  // saturation
  frr4f7qk: number;  // vibrance
  "7iz5iokq": number;  // hue
  lqesc6ol: number;  // temperature
  "4hpqnjer": number;  // tint
  "2zsbapjy": boolean;  // adjust_grayscale
  giajnlvh: boolean;  // adjust_invert
  "76aq4pc3": number;  // blur
  nraansa6: number;  // sharpen
  yxn6dvna: number;  // vignette
  mvgiivbx: number[][];  // curve
  /** Puts a layer in the folder `group`, or takes it out when that is empty. */
  set_layer_group(args: {
    group: string;
  }): Promise<void>;
  /** Runs `set_layer_group` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_layer_groupMany(items: SetLayerGroupArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adjusts everything on a layer, after each mark's own adjustments. */
  adjust_layer(args?: {
    exposure?: number;
    contrast?: number;
    highlights?: number;
    shadows?: number;
    saturation?: number;
    vibrance?: number;
    hue?: number;
    temperature?: number;
    tint?: number;
    grayscale?: boolean;
    invert?: boolean;
    blur?: number;
    sharpen?: number;
    vignette?: number;
  }): Promise<void>;
  /** Runs `adjust_layer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  adjust_layerMany(items: AdjustLayerArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Layer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LAYER_ACTIONS = ["set_layer_group", "set_layer_groupMany", "adjust_layer", "adjust_layerMany"] as const;

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
  hyumfmpg: string;  // group
  lu2gjrfu: boolean;  // wet
  s2tjjp2p: Record<string, number>;  // params
  cdvrgjan: string;  // src
  vniyw6t3: number;  // x
  v3wlgndz: number;  // y
  "4nv5sha3": number;  // width
  hlvljkif: number;  // height
  o4zghm7q: string;  // blend
  wpv66rxy: number;  // crop_x
  sz6fzruq: number;  // crop_y
  ax7jjgo5: number;  // crop_width
  "7jtq2b7o": number;  // crop_height
  mzyxo2mo: number;  // straighten
  xndatwwg: boolean;  // flip_h
  vyaxfcb2: boolean;  // flip_v
  "37ailkzg": number;  // exposure
  g3ifa5op: number;  // contrast
  "3taebyjo": number;  // highlights
  "5d6dfhop": number;  // shadows
  nebzyt3a: number;  // saturation
  "6ib7oqy4": number;  // vibrance
  vzagvyr3: number;  // hue
  bdxzjdby: number;  // temperature
  skvl4xui: number;  // tint
  "3tuoc3tu": boolean;  // grayscale
  tr7kuqc5: boolean;  // invert
  qe7z2w5c: number;  // blur
  h6rqzqu5: number;  // sharpen
  glowmwgx: number;  // vignette
  jzqxuy4d: string;  // gradient
  "2afylrq3": number;  // gradient_x0
  skgor2yj: number;  // gradient_y0
  ctpi3sbl: number;  // gradient_x1
  nosksv3s: number;  // gradient_y1
  n4zfbtzn: number[] | null;  // gradient_offsets
  "7vai53br": string[] | null;  // gradient_colours
  "63yhsfnf": string;  // text
  xbmipp4j: string;  // font
  ymnuknae: number;  // weight
  u62kvwpy: boolean;  // italic
  mj3rz5s2: string;  // align
  i7zjtca3: number;  // line_height
  sc445r7q: number;  // tracking
  e7l24fze: string;  // text_path
  "5lpu3cr5": number[];  // transform
  "7kaylfaa": string;  // clip
  adangnyz: boolean;  // pixelated
  p45w3ec2: number;  // grain
  gdv4nrn7: number;  // feather
  dvxs4mkf: number;  // shadow_x
  ebyye73f: number;  // shadow_y
  lpv4a2ao: number;  // shadow_blur
  j3x5co4v: string;  // shadow_colour
  "4d36wjcg": number;  // inner_shadow_x
  aukevlpf: number;  // inner_shadow_y
  wz2pco6l: number;  // inner_shadow_blur
  "3hhduagr": string;  // inner_shadow_colour
  "66bccfgi": number[][];  // curve
  /** Puts a mark in the folder `group`, or takes it out when that is empty. Marks of a group are addressed, exported and shown together. */
  group_mark(args: {
    group: string;
  }): Promise<void>;
  /** Runs `group_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  group_markMany(items: GroupMarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Fills a mark with a gradient from `from` to `to`, its stops given as offsets 0 to 1 beside their colours. */
  fill_gradient(args: {
    x0: number;
    y0: number;
    x1: number;
    y1: number;
    offsets: number[];
    colours: string[];
    kind?: string;
  }): Promise<void>;
  /** Runs `fill_gradient` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  fill_gradientMany(items: FillGradientArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Paints a mark into what is still wet, or over dry paint. */
  wet_mark(args?: {
    wet?: boolean;
  }): Promise<void>;
  /** Runs `wet_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  wet_markMany(items: WetMarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Clears a mark's gradient, leaving the plain colour. */
  clear_gradient(): Promise<void>;
  /** Runs `clear_gradient` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_gradientMany(items: ClearGradientArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets a text mark's words, its type and its setting. A parameter left out stays as it is. */
  set_text(args?: {
    text?: string;
    size?: number;
    colour?: string;
    font?: string;
    weight?: number;
    italic?: boolean;
    align?: string;
    line_height?: number;
    tracking?: number;
    path?: string;
  }): Promise<void>;
  /** Runs `set_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_textMany(items: SetTextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows part of the picture: the frame `x`, `y`, `width`, `height` of its own pixels, each a fraction of them. The picture stays put and the frame follows, so a crop is an edit of the mark and never of the pixels. */
  crop_picture(args?: {
    x?: number;
    y?: number;
    width?: number;
    height?: number;
  }): Promise<void>;
  /** Runs `crop_picture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  crop_pictureMany(items: CropPictureArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns the picture inside its frame, in degrees, enlarging it so no corner shows. */
  straighten_picture(args: {
    degrees: number;
  }): Promise<void>;
  /** Runs `straighten_picture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  straighten_pictureMany(items: StraightenPictureArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Mirrors the picture across its middle. */
  flip_picture(args?: {
    horizontal?: boolean;
    vertical?: boolean;
    flip_h?: boolean;
    flip_v?: boolean;
  }): Promise<void>;
  /** Runs `flip_picture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  flip_pictureMany(items: FlipPictureArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets how a mark mixes with what is under it. */
  blend_mark(args: {
    blend: string;
  }): Promise<void>;
  /** Runs `blend_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  blend_markMany(items: BlendMarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Grades a mark without touching its pixels: exposure, contrast and the rest are kept on the mark and applied wherever the artwork is shown or exported. A parameter left out stays as it is, so an agent changes one step at a time. */
  adjust_mark(args?: {
    exposure?: number;
    contrast?: number;
    highlights?: number;
    shadows?: number;
    saturation?: number;
    vibrance?: number;
    hue?: number;
    temperature?: number;
    tint?: number;
    grayscale?: boolean;
    invert?: boolean;
    blur?: number;
    sharpen?: number;
    vignette?: number;
  }): Promise<void>;
  /** Runs `adjust_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  adjust_markMany(items: AdjustMarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a mark by `dx`, `dy` artwork units. */
  move_mark(args: {
    dx: number;
    dy: number;
  }): Promise<void>;
  /** Runs `move_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_markMany(items: MoveMarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Scales a mark about `cx`, `cy` by `factor`. */
  scale_mark(args: {
    cx: number;
    cy: number;
    factor: number;
  }): Promise<void>;
  /** Runs `scale_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  scale_markMany(items: ScaleMarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns a mark about `cx`, `cy` by `degrees`. */
  rotate_mark(args: {
    cx: number;
    cy: number;
    degrees: number;
  }): Promise<void>;
  /** Runs `rotate_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rotate_markMany(items: RotateMarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Cuts a mark to the shape of another mark or a group, or lets it stand whole. */
  clip_mark(args: {
    by: string;
  }): Promise<void>;
  /** Runs `clip_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clip_markMany(items: ClipMarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets how a mark is finished: nearest-neighbour pixels, grain, a feathered edge, and its shadows. A parameter left out stays as it is. */
  finish_mark(args?: {
    pixelated?: boolean;
    grain?: number;
    feather?: number;
    shadow_x?: number;
    shadow_y?: number;
    shadow_blur?: number;
    shadow_colour?: string;
    inner_shadow_x?: number;
    inner_shadow_y?: number;
    inner_shadow_blur?: number;
    inner_shadow_colour?: string;
  }): Promise<void>;
  /** Runs `finish_mark` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  finish_markMany(items: FinishMarkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets a mark's tone curve, `[[in, out], …]` from 0 to 1. */
  set_curve(args: {
    curve: number[][];
  }): Promise<void>;
  /** Runs `set_curve` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_curveMany(items: SetCurveArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets how the brush lays a mark: its flow, hardness, spacing, smoothing and taper, each 0 to 1. A parameter left out stays as it is. */
  set_dynamics(args?: {
    flow?: number;
    hardness?: number;
    spacing?: number;
    smoothing?: number;
    taper?: number;
  }): Promise<void>;
  /** Runs `set_dynamics` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_dynamicsMany(items: SetDynamicsArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Mark` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MARK_ACTIONS = ["group_mark", "group_markMany", "fill_gradient", "fill_gradientMany", "wet_mark", "wet_markMany", "clear_gradient", "clear_gradientMany", "set_text", "set_textMany", "crop_picture", "crop_pictureMany", "straighten_picture", "straighten_pictureMany", "flip_picture", "flip_pictureMany", "blend_mark", "blend_markMany", "adjust_mark", "adjust_markMany", "move_mark", "move_markMany", "scale_mark", "scale_markMany", "rotate_mark", "rotate_markMany", "clip_mark", "clip_markMany", "finish_mark", "finish_markMany", "set_curve", "set_curveMany", "set_dynamics", "set_dynamicsMany"] as const;

/** The `tl.so.tilo.canvas.Mark` kind at `path`. */
export function mark(ws: tilo.Workspace, path: string): Mark {
  return resourceProxy(ws, path, SURFACE, MARK_ACTIONS) as unknown as Mark;
}

/** Area — `tl.so.tilo.canvas.Area`. */
export interface Area extends tilo.Resource {
  k26xq4t7: string;  // d
  enongrrr: number;  // feather
  j7jomf5b: string;  // matte_src
  oeihhduj: number;  // matte_x
  iqzt5i5r: number;  // matte_y
  l6zpym3q: number;  // matte_width
  to7qczkv: number;  // matte_height
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

/** Medium — `tl.so.tilo.canvas.Medium`. */
export interface Medium extends tilo.Resource {
  bevph2qk: string;  // key
  cgytyo4j: string;  // label
  "2opuzdzr": string;  // engine
  mgi6schv: string;  // description
  "6r6opsjz": string;  // ground
  hids323q: Record<string, number>;  // params
  rdqoi6b4: Pigment[];  // palette
}

/** The actions `Medium` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MEDIUM_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.Medium` kind at `path`. */
export function medium(ws: tilo.Workspace, path: string): Medium {
  return resourceProxy(ws, path, SURFACE, MEDIUM_ACTIONS) as unknown as Medium;
}

/** Pigment — `tl.so.tilo.canvas.Pigment`. */
export interface Pigment extends tilo.Resource {
  ph4qfvlv: string;  // name
  "6heumgwe": string;  // colour
  tm7hljog: number;  // granulation
  t2xyhkxn: number;  // strength
}

/** The actions `Pigment` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PIGMENT_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.Pigment` kind at `path`. */
export function pigment(ws: tilo.Workspace, path: string): Pigment {
  return resourceProxy(ws, path, SURFACE, PIGMENT_ACTIONS) as unknown as Pigment;
}

/** Reference — `tl.so.tilo.canvas.Reference`. */
export interface Reference extends tilo.Resource {
  "3ze3qk7e": string;  // image
  ky6t7zk4: number;  // x
  k5uohe26: number;  // y
  vyzji3vv: number;  // width
  peuuxvxb: number;  // height
  decytxpt: number;  // opacity
  j3jxn7s3: boolean;  // visible
}

/** The actions `Reference` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REFERENCE_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.Reference` kind at `path`. */
export function reference(ws: tilo.Workspace, path: string): Reference {
  return resourceProxy(ws, path, SURFACE, REFERENCE_ACTIONS) as unknown as Reference;
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
  hoix2o4s: MediumJson[];  // media
  "5h6vjvk2": AreaJson | null;  // selection
  n77pojfm: ReferenceJson | null;  // reference
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
  km2u7cyx: string;  // group
  qhmnxju4: string;  // mask_of
  "6oqanygz": number;  // base
  y6iyulut: boolean;  // invert
  k2r2z54p: boolean;  // clip
  ppv32nn6: boolean;  // unlinked
  z56o2ag2: number;  // exposure
  ui3khk6w: number;  // contrast
  yynr6bsh: number;  // highlights
  "3kml5egf": number;  // shadows
  "5c4xwrcg": number;  // saturation
  gi73p5cu: number;  // vibrance
  "7r6f5gkz": number;  // hue
  tfdtaklm: number;  // temperature
  oanzkyda: number;  // tint
  aj3mmig4: boolean;  // grayscale
  "2cktvie6": boolean;  // invert_adjust
  zkir2g3c: number;  // blur
  frk7zwuc: number;  // sharpen
  pftddo22: number;  // vignette
  eenyv7e2: number[][];  // curve
}

/** The actions `LayerJson` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LAYERJSON_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.LayerJson` kind at `path`. */
export function layerJson(ws: tilo.Workspace, path: string): LayerJson {
  return resourceProxy(ws, path, SURFACE, LAYERJSON_ACTIONS) as unknown as LayerJson;
}

/** MediumJson — `tl.so.tilo.canvas.MediumJson`. */
export interface MediumJson extends tilo.Resource {
  iztu5hbq: string;  // id
  shen6puc: string;  // label
  pcwg6twm: string;  // engine
  tucyloxp: string;  // description
  "3g5vtp7l": string;  // ground
  athamtu3: Record<string, number>;  // params
  pq2ipocw: PigmentJson[];  // palette
}

/** The actions `MediumJson` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MEDIUMJSON_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.MediumJson` kind at `path`. */
export function mediumJson(ws: tilo.Workspace, path: string): MediumJson {
  return resourceProxy(ws, path, SURFACE, MEDIUMJSON_ACTIONS) as unknown as MediumJson;
}

/** PigmentJson — `tl.so.tilo.canvas.PigmentJson`. */
export interface PigmentJson extends tilo.Resource {
  dgwlsywk: string;  // name
  b74p3dso: string;  // colour
  rfus746j: number;  // granulation
  veeu7fpc: number;  // strength
}

/** The actions `PigmentJson` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PIGMENTJSON_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.PigmentJson` kind at `path`. */
export function pigmentJson(ws: tilo.Workspace, path: string): PigmentJson {
  return resourceProxy(ws, path, SURFACE, PIGMENTJSON_ACTIONS) as unknown as PigmentJson;
}

/** ReferenceJson — `tl.so.tilo.canvas.ReferenceJson`. */
export interface ReferenceJson extends tilo.Resource {
  go76fdwt: string;  // image
  ax3jntcc: number[] | null;  // frame
  urv4gnep: number;  // opacity
  zbqd3iad: boolean;  // visible
}

/** The actions `ReferenceJson` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REFERENCEJSON_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.ReferenceJson` kind at `path`. */
export function referenceJson(ws: tilo.Workspace, path: string): ReferenceJson {
  return resourceProxy(ws, path, SURFACE, REFERENCEJSON_ACTIONS) as unknown as ReferenceJson;
}

/** MarkJson — `tl.so.tilo.canvas.MarkJson`. */
export interface MarkJson extends tilo.Resource {
  "67wvmkkq": string;  // name
  jk7lnwfw: LayerJson;  // layer
  xy2goji2: string;  // kind
  lrfhvryb: string;  // group
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
  f2yfdpsw: string;  // blend
  gbswtfkm: number;  // crop_x
  cnv33zk4: number;  // crop_y
  yzjgjp5q: number;  // crop_width
  d2tduo5n: number;  // crop_height
  j7tormbq: number;  // straighten
  vmes3q7g: boolean;  // flip_h
  "4tv2wnsi": boolean;  // flip_v
  tnsniv2d: number;  // exposure
  xsuhepir: number;  // contrast
  rhmkylpp: number;  // highlights
  qqdl7hij: number;  // shadows
  asuq3lgp: number;  // saturation
  fxjyqy3l: number;  // vibrance
  rf2rek22: number;  // hue
  oqo6d26l: number;  // temperature
  suw3mw5t: number;  // tint
  prqekzin: boolean;  // grayscale
  ulnewmko: boolean;  // invert
  hhwbutye: number;  // blur
  aooc5sn5: number;  // sharpen
  y32qfhyc: number;  // vignette
  r77f22g6: string;  // gradient_kind
  nuba72na: number;  // gradient_x0
  yvaatile: number;  // gradient_y0
  jdbhzols: number;  // gradient_x1
  tv6gyxgx: number;  // gradient_y1
  ih74f5za: number[];  // gradient_offsets
  zvcp7y2l: string[];  // gradient_colours
  xub3tsun: string;  // text
  fkp53ei3: string;  // font
  "7nusoc27": number;  // weight
  n3izluzh: boolean;  // italic
  "5dk4ceey": string;  // align
  gnfltu6g: number;  // line_height
  u2tmalez: number;  // tracking
  hyqcnmz5: string;  // text_path
  q73msd2q: boolean;  // wet
  gfq7akbn: Record<string, number>;  // params
  uk5tcnwm: number[];  // transform
  w4ew2p2r: string;  // clip
  "3bhfidei": boolean;  // pixelated
  "6emj7eax": number;  // grain
  tcpy73ad: number;  // feather
  "3oq5rjv3": number;  // shadow_x
  ho5hmfdi: number;  // shadow_y
  "7v73mzgx": number;  // shadow_blur
  aktlep6h: string;  // shadow_colour
  aayx2zu4: number;  // inner_shadow_x
  "52usozk2": number;  // inner_shadow_y
  "2h5mxl46": number;  // inner_shadow_blur
  f7cd75z6: string;  // inner_shadow_colour
  zalrvie7: number[][];  // curve
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
  algnlk7t: MatteJson | null;  // matte
}

/** The actions `AreaJson` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const AREAJSON_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.AreaJson` kind at `path`. */
export function areaJson(ws: tilo.Workspace, path: string): AreaJson {
  return resourceProxy(ws, path, SURFACE, AREAJSON_ACTIONS) as unknown as AreaJson;
}

/** MatteJson — `tl.so.tilo.canvas.MatteJson`. */
export interface MatteJson extends tilo.Resource {
  "7s7g6rha": string;  // src
  erbjejvt: number[];  // rect
}

/** The actions `MatteJson` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MATTEJSON_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.MatteJson` kind at `path`. */
export function matteJson(ws: tilo.Workspace, path: string): MatteJson {
  return resourceProxy(ws, path, SURFACE, MATTEJSON_ACTIONS) as unknown as MatteJson;
}

/** GradeAsk — `tl.so.tilo.canvas.GradeAsk`. */
export interface GradeAsk extends tilo.Resource {
  fy7u7ntb: number;  // exposure
  h7ykiuc5: number;  // contrast
  akjbo2e5: number;  // highlights
  gx2o2imz: number;  // shadows
  gwfjuivz: number;  // saturation
  y7wzyyqq: number;  // vibrance
  pkwjhrnw: number;  // hue
  gvwo4klz: number;  // temperature
  "6bogm2iw": number;  // tint
  mz4xapep: boolean;  // grayscale
  "7pnhizf2": boolean;  // invert
  vzbhth5d: number;  // blur
  ayg5wxga: number;  // sharpen
  bporvvfd: number;  // vignette
}

/** The actions `GradeAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const GRADEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.GradeAsk` kind at `path`. */
export function gradeAsk(ws: tilo.Workspace, path: string): GradeAsk {
  return resourceProxy(ws, path, SURFACE, GRADEASK_ACTIONS) as unknown as GradeAsk;
}

/** WandAsk — `tl.so.tilo.canvas.WandAsk`. */
export interface WandAsk extends tilo.Resource {
  yyqmyjfc: number[] | null;  // at
  etjvwbve: string | null;  // colour
  giesycja: number | null;  // tolerance
  vry7txyu: boolean | null;  // everywhere
  e3v56jsz: number[] | null;  // frame
}

/** The actions `WandAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WANDASK_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.WandAsk` kind at `path`. */
export function wandAsk(ws: tilo.Workspace, path: string): WandAsk {
  return resourceProxy(ws, path, SURFACE, WANDASK_ACTIONS) as unknown as WandAsk;
}

/** FindAsk — `tl.so.tilo.canvas.FindAsk`. */
export interface FindAsk extends tilo.Resource {
  "3ug7cc6p": boolean | null;  // subject
  mgf542kb: number | null;  // refine
  oehqnmnq: number[] | null;  // at
  t76lt6eq: string | null;  // colour
  rk2wxm5k: number | null;  // tolerance
  ipz3kxni: string | null;  // layer
  "5ijkh76y": number | null;  // feather
}

/** The actions `FindAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FINDASK_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.FindAsk` kind at `path`. */
export function findAsk(ws: tilo.Workspace, path: string): FindAsk {
  return resourceProxy(ws, path, SURFACE, FINDASK_ACTIONS) as unknown as FindAsk;
}

/** CombineAsk — `tl.so.tilo.canvas.CombineAsk`. */
export interface CombineAsk extends tilo.Resource {
  bp3zj5os: string[];  // ids
  "4kf2vu7l": string;  // op
}

/** The actions `CombineAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMBINEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.CombineAsk` kind at `path`. */
export function combineAsk(ws: tilo.Workspace, path: string): CombineAsk {
  return resourceProxy(ws, path, SURFACE, COMBINEASK_ACTIONS) as unknown as CombineAsk;
}

/** MeasureAsk — `tl.so.tilo.canvas.MeasureAsk`. */
export interface MeasureAsk extends tilo.Resource {
  "2tk6phpq": string[];  // ids
}

/** The actions `MeasureAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MEASUREASK_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.MeasureAsk` kind at `path`. */
export function measureAsk(ws: tilo.Workspace, path: string): MeasureAsk {
  return resourceProxy(ws, path, SURFACE, MEASUREASK_ACTIONS) as unknown as MeasureAsk;
}

/** SelectAsk — `tl.so.tilo.canvas.SelectAsk`. */
export interface SelectAsk extends tilo.Resource {
  "7omk45ql": number[] | null;  // rect
  krhe2i56: number[] | null;  // ellipse
  "5eu7ff7r": number[][] | null;  // points
  mgxsjhfz: string | null;  // d
  iqzzaywp: string[] | null;  // marks
  l4imaa4l: string | null;  // mode
  "7zgzz7h2": number | null;  // feather
  ohfhq5pm: number | null;  // grow
  ojp4xz4a: boolean | null;  // invert
}

/** The actions `SelectAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SELECTASK_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.SelectAsk` kind at `path`. */
export function selectAsk(ws: tilo.Workspace, path: string): SelectAsk {
  return resourceProxy(ws, path, SURFACE, SELECTASK_ACTIONS) as unknown as SelectAsk;
}

/** InspectAsk — `tl.so.tilo.canvas.InspectAsk`. */
export interface InspectAsk extends tilo.Resource {
  "3uypu22a": boolean | null;  // palette
  veyz6j2q: number[] | null;  // region
  ylddsw2e: string | null;  // layer
  "6nfqt3pd": number | null;  // limit
}

/** The actions `InspectAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const INSPECTASK_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.InspectAsk` kind at `path`. */
export function inspectAsk(ws: tilo.Workspace, path: string): InspectAsk {
  return resourceProxy(ws, path, SURFACE, INSPECTASK_ACTIONS) as unknown as InspectAsk;
}

/** RemoveAsk — `tl.so.tilo.canvas.RemoveAsk`. */
export interface RemoveAsk extends tilo.Resource {
  js4zslbp: number[][];  // along
  besel4qy: string | null;  // layer
}

/** The actions `RemoveAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REMOVEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.RemoveAsk` kind at `path`. */
export function removeAsk(ws: tilo.Workspace, path: string): RemoveAsk {
  return resourceProxy(ws, path, SURFACE, REMOVEASK_ACTIONS) as unknown as RemoveAsk;
}

/** DiffAsk — `tl.so.tilo.canvas.DiffAsk`. */
export interface DiffAsk extends tilo.Resource {
  eubkyqqd: string;  // from
  mit27xah: string | null;  // to
  eiocv26o: boolean | null;  // look
  rgii66ho: boolean | null;  // overlay
}

/** The actions `DiffAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DIFFASK_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.DiffAsk` kind at `path`. */
export function diffAsk(ws: tilo.Workspace, path: string): DiffAsk {
  return resourceProxy(ws, path, SURFACE, DIFFASK_ACTIONS) as unknown as DiffAsk;
}

/** CompareAsk — `tl.so.tilo.canvas.CompareAsk`. */
export interface CompareAsk extends tilo.Resource {
  mztro4wb: number[] | null;  // region
  "67462gfa": string | null;  // mode
  hbu4rn3a: number | null;  // size
}

/** The actions `CompareAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COMPAREASK_ACTIONS = [] as const;

/** The `tl.so.tilo.canvas.CompareAsk` kind at `path`. */
export function compareAsk(ws: tilo.Workspace, path: string): CompareAsk {
  return resourceProxy(ws, path, SURFACE, COMPAREASK_ACTIONS) as unknown as CompareAsk;
}

/** Canvas — `tl.so.tilo.canvas`. */
export interface Canvas extends tilo.Resource {
  /** A picture with those steps applied to its pixels, as a new file: what a person or an agent asks for when the graded picture itself is wanted, rather than the artwork that shows it. */
  grade_picture(args: {
    file: unknown;
    exposure?: number;
    contrast?: number;
    highlights?: number;
    shadows?: number;
    saturation?: number;
    vibrance?: number;
    hue?: number;
    temperature?: number;
    tint?: number;
    grayscale?: boolean;
    invert?: boolean;
    blur?: number;
    sharpen?: number;
    vignette?: number;
  }): Promise<unknown | null>;
}

/** The actions `Canvas` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CANVAS_ACTIONS = ["grade_picture"] as const;

/** The `tl.so.tilo.canvas` kind at `path`. */
export function canvas(ws: tilo.Workspace, path: string): Canvas {
  return resourceProxy(ws, path, SURFACE, CANVAS_ACTIONS) as unknown as Canvas;
}

