/**
 * Types for Video Editor (`so.tilo.video-editor`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Media` is a `Resource` of the app's `tl.so.tilo.video-editor.Media`, made by its factory with the
 * resource's path (`video-editor/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.video-editor@c1036ba6df1f5196";

/** How a title's lines sit in its box. One of "left", "centre", "right". */
export type Align = "left" | "centre" | "right";

/** What a source is: a picture that moves, a sound, a still, or words this app draws itself. One of "video", "audio", "image", "title". */
export type Form = "video" | "audio" | "image" | "title";

/** One look of a title the inspector sets with a slider, drawn again as it changes. One of "size", "red", "green", "blue", "backdrop". */
export type TitleLook = "size" | "red" | "green" | "blue" | "backdrop";

/** One look of a clip that the inspector sets with a slider: the clip's own picture and sound, which the preview draws again as the person changes it. One of "opacity", "volume", "speed", "fade_in", "fade_out", "brightness", "contrast", "saturation", "scale_x", "scale_y", "pos_x", "pos_y", "rotation", "crop_left", "crop_right", "crop_top", "crop_bottom", "key_tolerance", "key_softness", "key_spill", "mask_x", "mask_y", "mask_w", "mask_h", "mask_rotation", "mask_feather", "transition_duration". */
export type Look = "opacity" | "volume" | "speed" | "fade_in" | "fade_out" | "brightness" | "contrast" | "saturation" | "scale_x" | "scale_y" | "pos_x" | "pos_y" | "rotation" | "crop_left" | "crop_right" | "crop_top" | "crop_bottom" | "key_tolerance" | "key_softness" | "key_spill" | "mask_x" | "mask_y" | "mask_w" | "mask_h" | "mask_rotation" | "mask_feather" | "transition_duration";

/** The arguments of `draw_swatch`. */
export interface DrawSwatchArgs {
  width: number;
}

/** The arguments of `hear`. */
export interface HearArgs {
}

/** The arguments of `set_title_style`. */
export interface SetTitleStyleArgs {
  bold: boolean;
  italic: boolean;
}

/** The arguments of `set_title_look`. */
export interface SetTitleLookArgs {
  look: TitleLook;
  value: number;
}

/** The arguments of `redraw_media`. */
export interface RedrawMediaArgs {
}

/** The arguments of `set_align`. */
export interface SetAlignArgs {
  align: Align;
}

/** The arguments of `set_words`. */
export interface SetWordsArgs {
  words: string;
}

/** The arguments of `set_track_gain`. */
export interface SetTrackGainArgs {
  db: number;
}

/** The arguments of `set_track_pan`. */
export interface SetTrackPanArgs {
  pan: number;
}

/** The arguments of `set_track_muted`. */
export interface SetTrackMutedArgs {
  muted: boolean;
}

/** The arguments of `set_track_solo`. */
export interface SetTrackSoloArgs {
  solo: boolean;
}

/** The arguments of `set_track_locked`. */
export interface SetTrackLockedArgs {
  locked: boolean;
}

/** The arguments of `reset_mix`. */
export interface ResetMixArgs {
}

/** The arguments of `set_interp`. */
export interface SetInterpArgs {
  interp: string;
}

/** The arguments of `set_speed`. */
export interface SetSpeedArgs {
  speed: number;
}

/** The arguments of `normal_speed`. */
export interface NormalSpeedArgs {
}

/** The arguments of `set_look`. */
export interface SetLookArgs {
  look: Look;
  value: number;
}

/** The arguments of `set_mask`. */
export interface SetMaskArgs {
  shape: string;
}

/** The arguments of `invert_mask`. */
export interface InvertMaskArgs {
  invert: boolean;
}

/** The arguments of `set_transition`. */
export interface SetTransitionArgs {
  transition: string;
  seconds: number;
}

/** The arguments of `set_reverse`. */
export interface SetReverseArgs {
  reverse: boolean;
}

/** The arguments of `redraw_clip`. */
export interface RedrawClipArgs {
}

/** The arguments of `key_at`. */
export interface KeyAtArgs {
  at: number;
  param: string;
  interp: string;
}

/** The arguments of `unkey`. */
export interface UnkeyArgs {
  k: Key;
}

/** The arguments of `key_colour`. */
export interface KeyColourArgs {
  red: number;
  green: number;
  blue: number;
}

/** The arguments of `unlink_clip`. */
export interface UnlinkClipArgs {
}

/** The arguments of `unplace_side_by_side`. */
export interface UnplaceSideBySideArgs {
}

/** The arguments of `add_effect`. */
export interface AddEffectArgs {
  form: string;
}

/** The arguments of `remove_effect`. */
export interface RemoveEffectArgs {
  e: Effect;
}

/** The arguments of `set_effect`. */
export interface SetEffectArgs {
  e: Effect;
  amount: number;
}

/** The arguments of `move_effect`. */
export interface MoveEffectArgs {
  e: Effect;
  up: boolean;
}

/** The arguments of `clear_effects`. */
export interface ClearEffectsArgs {
}

/** The arguments of `note_marker`. */
export interface NoteMarkerArgs {
  note: string;
}

/** The arguments of `split_at`. */
export interface SplitAtArgs {
  c: Clip;
  at: number;
}

/** The arguments of `trouble`. */
export interface TroubleArgs {
  why: string;
}

/** The arguments of `adopt_probe`. */
export interface AdoptProbeArgs {
  m: Media;
  said: string;
}

/** The arguments of `refresh_source`. */
export interface RefreshSourceArgs {
  m: Media;
}

/** The arguments of `relink`. */
export interface RelinkArgs {
  m: Media;
  file: unknown;
}

/** The arguments of `mark_missing`. */
export interface MarkMissingArgs {
  m: Media;
  why: string;
}

/** The arguments of `draw_still`. */
export interface DrawStillArgs {
  at: number;
}

/** The arguments of `begin_render`. */
export interface BeginRenderArgs {
  render_kind: string;
  resolution: string;
  container: string;
  fps: number;
}

/** The arguments of `rename_project`. */
export interface RenameProjectArgs {
  name: string;
}

/** The arguments of `import_files`. */
export interface ImportFilesArgs {
  files: unknown[];
}

/** The arguments of `import_one`. */
export interface ImportOneArgs {
  file: unknown;
}

/** The arguments of `commit_drag`. */
export interface CommitDragArgs {
  grab: string;
  clip: string;
  from: number;
  at: number;
}

/** The arguments of `split_clip`. */
export interface SplitClipArgs {
  c: Clip;
  at: number;
}

/** The arguments of `cut_at`. */
export interface CutAtArgs {
  at: number;
}

/** The arguments of `remove_clip`. */
export interface RemoveClipArgs {
  c: Clip;
}

/** The arguments of `ripple_delete`. */
export interface RippleDeleteArgs {
  c: Clip;
}

/** The arguments of `nudge`. */
export interface NudgeArgs {
  c: Clip;
  frames: number;
}

/** The arguments of `redraw`. */
export interface RedrawArgs {
}

/** The arguments of `sample_key`. */
export interface SampleKeyArgs {
  c: Clip;
  at: number;
  x: number;
  y: number;
}

/** The arguments of `filmstrip`. */
export interface FilmstripArgs {
  c: Clip;
}

/** The arguments of `draw_next_strip`. */
export interface DrawNextStripArgs {
}

/** The arguments of `remove_marker`. */
export interface RemoveMarkerArgs {
  k: Marker;
}

/** The arguments of `mark_in`. */
export interface MarkInArgs {
  at: number;
}

/** The arguments of `mark_out`. */
export interface MarkOutArgs {
  at: number;
}

/** The arguments of `clear_marks`. */
export interface ClearMarksArgs {
}

/** The arguments of `set_frame_rate`. */
export interface SetFrameRateArgs {
  fps: number;
}

/** The arguments of `set_frame_size`. */
export interface SetFrameSizeArgs {
  width: number;
  height: number;
}

/** The arguments of `adopt_format`. */
export interface AdoptFormatArgs {
  m: Media;
}

/** The arguments of `delete_track`. */
export interface DeleteTrackArgs {
  t: Track;
}

/** The arguments of `link_clips`. */
export interface LinkClipsArgs {
  a: Clip;
  b: Clip;
}

/** The arguments of `split_audio`. */
export interface SplitAudioArgs {
  c: Clip;
}

/** The arguments of `place_side_by_side`. */
export interface PlaceSideBySideArgs {
  left: Clip;
  right: Clip;
}

/** The arguments of `delete_media`. */
export interface DeleteMediaArgs {
  m: Media;
}

/** The arguments of `preview`. */
export interface PreviewArgs {
  at: number;
}

/** The arguments of `seek_at`. */
export interface SeekAtArgs {
  x: number;
  span: number;
  scroll: number;
}

/** The arguments of `preview_cut`. */
export interface PreviewCutArgs {
}

/** The arguments of `export_cut`. */
export interface ExportCutArgs {
  resolution: string;
  container: string;
  fps: number;
}

/** The arguments of `poll_render`. */
export interface PollRenderArgs {
}

/** The arguments of `cancel_render`. */
export interface CancelRenderArgs {
}

/** The arguments of `open_project`. */
export interface OpenProjectArgs {
  p: Project;
}

/** The arguments of `forget_project`. */
export interface ForgetProjectArgs {
  p: Project;
}

/** Media — `tl.so.tilo.video-editor.Media`. */
export interface Media extends tilo.Resource {
  efejoszz: string;  // name
  pcamqw4n: Form;  // form
  wj2aohfd: number;  // duration
  hpkuwe3q: number;  // width
  krrrznuy: number;  // height
  vmxn3cwg: number;  // fps
  foxa6zt7: number;  // palette
  zltur6gc: string;  // path
  ap457hnd: boolean;  // has_audio
  "3vgrlwgv": string;  // still
  vddvspwk: number[];  // peaks
  v4yfudyw: boolean;  // missing
  e7atusb5: string;  // words
  qrihyxs3: number;  // size
  x2yorlvh: number;  // red
  "5nv4umhz": number;  // green
  ktanswey: number;  // blue
  lqpu2kuo: number;  // alpha
  "3j5mkeir": Align;  // align
  u37npdvi: boolean;  // bold
  wzbpbjsf: boolean;  // italic
  hlbhw6tg: number;  // backdrop
  /** The source's picture as the bin shows it. */
  draw_swatch(args: {
    width: number;
  }): Promise<void>;
  /** Runs `draw_swatch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  draw_swatchMany(items: DrawSwatchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The source's loudness across the file, as the waveform draws it. */
  hear(): Promise<void>;
  /** Runs `hear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  hearMany(items: HearArgs[]): Promise<tilo.ActionAnswer[]>;
  /** One style of a title's lines, drawn again as it changes. */
  set_title_style(args: {
    bold: boolean;
    italic: boolean;
  }): Promise<void>;
  /** Runs `set_title_style` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_title_styleMany(items: SetTitleStyleArgs[]): Promise<tilo.ActionAnswer[]>;
  set_title_look(args: {
    look: TitleLook;
    value: number;
  }): Promise<void>;
  /** Runs `set_title_look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_title_lookMany(items: SetTitleLookArgs[]): Promise<tilo.ActionAnswer[]>;
  redraw_media(): Promise<void>;
  /** Runs `redraw_media` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  redraw_mediaMany(items: RedrawMediaArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets where a title's lines sit in its box. */
  set_align(args: {
    align: Align;
  }): Promise<void>;
  /** Runs `set_align` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_alignMany(items: SetAlignArgs[]): Promise<tilo.ActionAnswer[]>;
  /** What the title's lines say. */
  set_words(args: {
    words: string;
  }): Promise<void>;
  /** Runs `set_words` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_wordsMany(items: SetWordsArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Media` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MEDIA_ACTIONS = ["draw_swatch", "draw_swatchMany", "hear", "hearMany", "set_title_style", "set_title_styleMany", "set_title_look", "set_title_lookMany", "redraw_media", "redraw_mediaMany", "set_align", "set_alignMany", "set_words", "set_wordsMany"] as const;

/** The `tl.so.tilo.video-editor.Media` kind at `path`. */
export function media(ws: tilo.Workspace, path: string): Media {
  return resourceProxy(ws, path, SURFACE, MEDIA_ACTIONS) as unknown as Media;
}

/** Effect — `tl.so.tilo.video-editor.Effect`. */
export interface Effect extends tilo.Resource {
  ktghxc36: string;  // form
  lj3ggezn: number;  // amount
}

/** The actions `Effect` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EFFECT_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.Effect` kind at `path`. */
export function effect(ws: tilo.Workspace, path: string): Effect {
  return resourceProxy(ws, path, SURFACE, EFFECT_ACTIONS) as unknown as Effect;
}

/** Track — `tl.so.tilo.video-editor.Track`. */
export interface Track extends tilo.Resource {
  hyiqyr2d: string;  // name
  voeh7uw2: Form;  // form
  h5vj7qoc: number;  // index
  o7i5df56: boolean;  // muted
  azpfbjsn: boolean;  // solo
  wowizwqg: boolean;  // locked
  rofejlx6: number;  // gain_db
  ovon2nm7: number;  // pan
  /** One track's level, where the sound of everything on it sits in the mix. */
  set_track_gain(args: {
    db: number;
  }): Promise<void>;
  /** Runs `set_track_gain` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_track_gainMany(items: SetTrackGainArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Where a track's sound sits across the stereo field: -1 hard left, 1 hard right. */
  set_track_pan(args: {
    pan: number;
  }): Promise<void>;
  /** Runs `set_track_pan` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_track_panMany(items: SetTrackPanArgs[]): Promise<tilo.ActionAnswer[]>;
  set_track_muted(args: {
    muted: boolean;
  }): Promise<void>;
  /** Runs `set_track_muted` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_track_mutedMany(items: SetTrackMutedArgs[]): Promise<tilo.ActionAnswer[]>;
  set_track_solo(args: {
    solo: boolean;
  }): Promise<void>;
  /** Runs `set_track_solo` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_track_soloMany(items: SetTrackSoloArgs[]): Promise<tilo.ActionAnswer[]>;
  set_track_locked(args: {
    locked: boolean;
  }): Promise<void>;
  /** Runs `set_track_locked` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_track_lockedMany(items: SetTrackLockedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts a track back at unity and the centre. */
  reset_mix(): Promise<void>;
  /** Runs `reset_mix` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reset_mixMany(items: ResetMixArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Track` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TRACK_ACTIONS = ["set_track_gain", "set_track_gainMany", "set_track_pan", "set_track_panMany", "set_track_muted", "set_track_mutedMany", "set_track_solo", "set_track_soloMany", "set_track_locked", "set_track_lockedMany", "reset_mix", "reset_mixMany"] as const;

/** The `tl.so.tilo.video-editor.Track` kind at `path`. */
export function track(ws: tilo.Workspace, path: string): Track {
  return resourceProxy(ws, path, SURFACE, TRACK_ACTIONS) as unknown as Track;
}

/** Key — `tl.so.tilo.video-editor.Key`. */
export interface Key extends tilo.Resource {
  "362ioruy": string;  // param
  ksxcsp7k: number;  // t
  al7ic2wh: number;  // v
  "4rmokhsj": string;  // interp
  /** How a key's curve leaves it. */
  set_interp(args: {
    interp: string;
  }): Promise<void>;
  /** Runs `set_interp` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_interpMany(items: SetInterpArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Key` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const KEY_ACTIONS = ["set_interp", "set_interpMany"] as const;

/** The `tl.so.tilo.video-editor.Key` kind at `path`. */
export function key(ws: tilo.Workspace, path: string): Key {
  return resourceProxy(ws, path, SURFACE, KEY_ACTIONS) as unknown as Key;
}

/** Clip — `tl.so.tilo.video-editor.Clip`. */
export interface Clip extends tilo.Resource {
  azbufhus: string;  // name
  tpxg6k4q: Track;  // track
  y4qdvmxb: Media;  // media
  vle43xl6: number;  // start
  tmrpa5lj: number;  // in_point
  ijg7zj64: number;  // out_point
  mzmglpkg: number;  // opacity
  hb6kydcz: number;  // volume
  "5lwntoqr": string;  // transition
  ihl7lqbw: number;  // transition_duration
  fmhqhubf: number;  // fade_in
  bygbi2tw: number;  // fade_out
  jrsfcsej: number;  // palette
  "55seasjt": number;  // speed_rate
  dmthewcr: boolean;  // reverse
  ttu42344: number;  // brightness
  sosquagj: number;  // contrast
  t6mekjo2: number;  // saturation
  kikxhud5: number;  // scale_x
  "67q6ewpx": number;  // scale_y
  nu7xhxvx: number;  // pos_x
  vrqmvalw: number;  // pos_y
  "3ru7tn4m": number;  // rotation
  kx3c765z: number;  // crop_left
  "4rl2tdsz": number;  // crop_right
  d4ryk5ir: number;  // crop_top
  r27hvfe7: number;  // crop_bottom
  "7uz3gyat": Array<tilo.Entry<Effect>>;  // effects
  tskiyma5: boolean;  // key_enabled
  nflhw3wz: number;  // key_tolerance
  gk3fgv4a: number;  // key_softness
  zxs2bxu6: number;  // key_spill
  ag6tnt5w: Array<tilo.Entry<Key>>;  // keys
  "54ikwabs": string[];  // strips
  bvukpc5x: string;  // strips_key
  dhh754gx: number;  // key_red
  podots7u: number;  // key_green
  tgpgd3hb: number;  // key_blue
  hfnhir7p: string;  // mask_shape
  hclucc6k: number;  // mask_x
  g2n4egth: number;  // mask_y
  lwnm5qmy: number;  // mask_w
  casg7gsr: number;  // mask_h
  u3oht37u: number;  // mask_rotation
  iraobc5s: number;  // mask_feather
  "6njmwsei": boolean;  // mask_invert
  vmc7izdi: string;  // link
  wovh5j2b: boolean;  // link_follows
  poy7f6dc: boolean;  // sound_split
  /** Sets the rate a clip plays at: 2 twice as fast and half as long, 0.5 in slow motion. */
  set_speed(args: {
    speed: number;
  }): Promise<void>;
  /** Runs `set_speed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_speedMany(items: SetSpeedArgs[]): Promise<tilo.ActionAnswer[]>;
  normal_speed(): Promise<void>;
  /** Runs `normal_speed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  normal_speedMany(items: NormalSpeedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets one look of the clip, and draws the frame the preview shows again, so an edit is seen as it is made rather than at the next move of the playhead. */
  set_look(args: {
    look: Look;
    value: number;
  }): Promise<void>;
  /** Runs `set_look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_lookMany(items: SetLookArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets which shape limits where a clip shows: `off`, `rect` or `ellipse`. */
  set_mask(args: {
    shape: string;
  }): Promise<void>;
  /** Runs `set_mask` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_maskMany(items: SetMaskArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps the inside of the clip's mask, or the outside of it. */
  invert_mask(args: {
    invert: boolean;
  }): Promise<void>;
  /** Runs `invert_mask` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  invert_maskMany(items: InvertMaskArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the incoming transition and how long it runs. */
  set_transition(args: {
    transition: string;
    seconds: number;
  }): Promise<void>;
  /** Runs `set_transition` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_transitionMany(items: SetTransitionArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Plays the clip's source backwards, or forwards again, and draws the frame it holds now. */
  set_reverse(args: {
    reverse: boolean;
  }): Promise<void>;
  /** Runs `set_reverse` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_reverseMany(items: SetReverseArgs[]): Promise<tilo.ActionAnswer[]>;
  redraw_clip(): Promise<void>;
  /** Runs `redraw_clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  redraw_clipMany(items: RedrawClipArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets a key of `param` at `at` (timeline seconds), at the value the field holds now: the value a curve changes from, as the inspection shows it. */
  key_at(args: {
    at: number;
    param: string;
    interp: string;
  }): Promise<void>;
  /** Runs `key_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  key_atMany(items: KeyAtArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Removes a key. */
  unkey(args: {
    k: Key;
  }): Promise<void>;
  /** Runs `unkey` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unkeyMany(items: UnkeyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets the colour the key removes to one of the usual screens. */
  key_colour(args: {
    red: number;
    green: number;
    blue: number;
  }): Promise<void>;
  /** Runs `key_colour` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  key_colourMany(items: KeyColourArgs[]): Promise<tilo.ActionAnswer[]>;
  /** One frame of a source at `at` seconds in it, kept as a filmstrip picture. */
  thumb_frame(args: {
    at: number;
    width: number;
    height: number;
  }): Promise<string>;
  unlink_clip(): Promise<void>;
  /** Runs `unlink_clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unlink_clipMany(items: UnlinkClipArgs[]): Promise<tilo.ActionAnswer[]>;
  unplace_side_by_side(): Promise<void>;
  /** Runs `unplace_side_by_side` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unplace_side_by_sideMany(items: UnplaceSideBySideArgs[]): Promise<tilo.ActionAnswer[]>;
  add_effect(args: {
    form: string;
  }): Promise<void>;
  /** Runs `add_effect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_effectMany(items: AddEffectArgs[]): Promise<tilo.ActionAnswer[]>;
  remove_effect(args: {
    e: Effect;
  }): Promise<void>;
  /** Runs `remove_effect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_effectMany(items: RemoveEffectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** How strongly one effect of the clip applies, applied to the picture at once. */
  set_effect(args: {
    e: Effect;
    amount: number;
  }): Promise<void>;
  /** Runs `set_effect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_effectMany(items: SetEffectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves an effect up or down the clip's stack: the order decides what the picture goes through first. */
  move_effect(args: {
    e: Effect;
    up: boolean;
  }): Promise<void>;
  /** Runs `move_effect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_effectMany(items: MoveEffectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes every effect off a clip. */
  clear_effects(): Promise<void>;
  /** Runs `clear_effects` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_effectsMany(items: ClearEffectsArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Clip` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CLIP_ACTIONS = ["set_speed", "set_speedMany", "normal_speed", "normal_speedMany", "set_look", "set_lookMany", "set_mask", "set_maskMany", "invert_mask", "invert_maskMany", "set_transition", "set_transitionMany", "set_reverse", "set_reverseMany", "redraw_clip", "redraw_clipMany", "key_at", "key_atMany", "unkey", "unkeyMany", "key_colour", "key_colourMany", "thumb_frame", "unlink_clip", "unlink_clipMany", "unplace_side_by_side", "unplace_side_by_sideMany", "add_effect", "add_effectMany", "remove_effect", "remove_effectMany", "set_effect", "set_effectMany", "move_effect", "move_effectMany", "clear_effects", "clear_effectsMany"] as const;

/** The `tl.so.tilo.video-editor.Clip` kind at `path`. */
export function clip(ws: tilo.Workspace, path: string): Clip {
  return resourceProxy(ws, path, SURFACE, CLIP_ACTIONS) as unknown as Clip;
}

/** Marker — `tl.so.tilo.video-editor.Marker`. */
export interface Marker extends tilo.Resource {
  "6gy3r5jn": string;  // name
  "43eckzwd": number;  // t
  jvjfto7y: number;  // palette
  h2qmhyqy: string;  // note
  /** Writes what a mark says. */
  note_marker(args: {
    note: string;
  }): Promise<void>;
  /** Runs `note_marker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  note_markerMany(items: NoteMarkerArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Marker` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MARKER_ACTIONS = ["note_marker", "note_markerMany"] as const;

/** The `tl.so.tilo.video-editor.Marker` kind at `path`. */
export function marker(ws: tilo.Workspace, path: string): Marker {
  return resourceProxy(ws, path, SURFACE, MARKER_ACTIONS) as unknown as Marker;
}

/** Project — `tl.so.tilo.video-editor.Project`. */
export interface Project extends tilo.Resource {
  hdhd7ufr: string;  // name
  sggyvq5o: number;  // fps
  rv7tdvl6: number;  // width
  z7cmqzxo: number;  // height
  m7e2vebr: boolean;  // explicit_format
  enmbeevc: number;  // mark_in
  vfq5phqm: number;  // mark_out
  f7xzwsye: string;  // format
  qm4juanr: Array<tilo.Entry<Media>>;  // media
  "4z3qlwar": Array<tilo.Entry<Track>>;  // tracks
  "74vaoosx": Array<tilo.Entry<Clip>>;  // clips
  qr6qe3cp: Array<tilo.Entry<Marker>>;  // markers
  "47vsdgaf": string;  // still
  dppbe5en: number;  // still_at
  gkp4va33: string;  // proxy
  xa5fpldf: string;  // job
  dkhcppi4: string;  // job_kind
  myjdbgx2: number;  // job_done
  "63bjrnm4": number;  // job_total
  rhjs5vir: string;  // exported
  "4jky56y7": string;  // trouble
  /** Cuts a clip in two at `at`, each half keeping the source its own part shows. */
  split_at(args: {
    c: Clip;
    at: number;
  }): Promise<void>;
  /** Runs `split_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  split_atMany(items: SplitAtArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves `c` (and its linked sound with it) to `start`, refusing an overlap. */
  shifted(args: {
    c: Clip;
    start: number;
  }): Promise<boolean>;
  trouble(args: {
    why: string;
  }): Promise<void>;
  /** Runs `trouble` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  troubleMany(items: TroubleArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks the device what a file is, adds it as a source and draws its swatch and waveform. What the device said when it could not be added, or empty when it was: a batch names each file that did not arrive and why, so a person knows what to do about it. */
  absorb(args: {
    file: unknown;
  }): Promise<string>;
  /** Writes what the device read of a source onto it, as the engine said it: what it is, how long it runs, its size and rate, and whether it carries sound. An action is handed the engine's words rather than the decoded `Probed`, which is a value no resource holds. */
  adopt_probe(args: {
    m: Media;
    said: string;
  }): Promise<void>;
  /** Runs `adopt_probe` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  adopt_probeMany(items: AdoptProbeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Checks that a source's bytes are here: reads what they are again, draws its swatch and waveform, and marks it not on this device when the engine cannot reach them. */
  refresh_source(args: {
    m: Media;
  }): Promise<void>;
  /** Runs `refresh_source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  refresh_sourceMany(items: RefreshSourceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Points a source at a file the person chose: the clips are cuts of the source, so they keep it, and the source reads what its new file says. */
  relink(args: {
    m: Media;
    file: unknown;
  }): Promise<void>;
  /** Runs `relink` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  relinkMany(items: RelinkArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Says the source's bytes are not on this device, and why the engine could not read them. */
  mark_missing(args: {
    m: Media;
    why: string;
  }): Promise<void>;
  /** Runs `mark_missing` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  mark_missingMany(items: MarkMissingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The frame the preview shows at `at`, drawn by the engine. */
  draw_still(args: {
    at: number;
  }): Promise<void>;
  /** Runs `draw_still` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  draw_stillMany(items: DrawStillArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a render: `preview` keeps the cut small for the window to play, `export` writes the file a person saves. */
  begin_render(args: {
    render_kind: string;
    resolution: string;
    container: string;
    fps: number;
  }): Promise<void>;
  /** Runs `begin_render` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  begin_renderMany(items: BeginRenderArgs[]): Promise<tilo.ActionAnswer[]>;
  rename_project(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_projectMany(items: RenameProjectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds every file a person chose or dropped as a source, and says which files did not arrive and what the device said of each. */
  import_files(args: {
    files: unknown[];
  }): Promise<void>;
  /** Runs `import_files` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_filesMany(items: ImportFilesArgs[]): Promise<tilo.ActionAnswer[]>;
  import_one(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `import_one` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_oneMany(items: ImportOneArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The cut as a file a person keeps: its sources and their places, where they stand on their own device. */
  save_project(): Promise<unknown>;
  /** The cut as a film: what an export made. */
  export_file(): Promise<unknown>;
  /** Places `m` on `track` at `at`, in the first place it fits. */
  place(args: {
    m: Media;
    track: Track;
    at: number;
  }): Promise<Clip>;
  /** Places the bin's source on the first track of its form, at `at`. */
  place_media(args: {
    m: Media;
    at: number;
  }): Promise<Clip | null>;
  /** What a drag of the timeline committed: a clip moved or trimmed. */
  commit_drag(args: {
    grab: string;
    clip: string;
    from: number;
    at: number;
  }): Promise<void>;
  /** Runs `commit_drag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  commit_dragMany(items: CommitDragArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Cuts the clip at the playhead in two. */
  split_clip(args: {
    c: Clip;
    at: number;
  }): Promise<void>;
  /** Runs `split_clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  split_clipMany(items: SplitClipArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Cuts every clip the playhead stands on. */
  cut_at(args: {
    at: number;
  }): Promise<void>;
  /** Runs `cut_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cut_atMany(items: CutAtArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a clip, and the sound linked to it with it. */
  remove_clip(args: {
    c: Clip;
  }): Promise<void>;
  /** Runs `remove_clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_clipMany(items: RemoveClipArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Deletes a clip and closes the gap it leaves behind it. */
  ripple_delete(args: {
    c: Clip;
  }): Promise<void>;
  /** Runs `ripple_delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  ripple_deleteMany(items: RippleDeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Slips the clip along the timeline by frames. */
  nudge(args: {
    c: Clip;
    frames: number;
  }): Promise<void>;
  /** Runs `nudge` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  nudgeMany(items: NudgeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Draws the frame the preview last showed again, so an edit made after it is seen at once. A cut whose frame the window never showed draws nothing: there is no place to draw it at. */
  redraw(): Promise<void>;
  /** Runs `redraw` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  redrawMany(items: RedrawArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the colour of the cut's frame where the person pointed, as the key that removes it. */
  sample_key(args: {
    c: Clip;
    at: number;
    x: number;
    y: number;
  }): Promise<void>;
  /** Runs `sample_key` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  sample_keyMany(items: SampleKeyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Draws eight frames of the clip across its own part, kept until a trim asks for them again. */
  filmstrip(args: {
    c: Clip;
  }): Promise<void>;
  /** Runs `filmstrip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  filmstripMany(items: FilmstripArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Draws the filmstrip of the first clip whose frames are missing or stale. */
  draw_next_strip(): Promise<void>;
  /** Runs `draw_next_strip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  draw_next_stripMany(items: DrawNextStripArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a title to the bin and places it at `at`. */
  add_title(args: {
    words: string;
    at: number;
  }): Promise<Media>;
  /** Adds a marker at `at`. */
  add_marker(args: {
    at: number;
  }): Promise<Marker>;
  remove_marker(args: {
    k: Marker;
  }): Promise<void>;
  /** Runs `remove_marker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_markerMany(items: RemoveMarkerArgs[]): Promise<tilo.ActionAnswer[]>;
  mark_in(args: {
    at: number;
  }): Promise<void>;
  /** Runs `mark_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  mark_inMany(items: MarkInArgs[]): Promise<tilo.ActionAnswer[]>;
  mark_out(args: {
    at: number;
  }): Promise<void>;
  /** Runs `mark_out` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  mark_outMany(items: MarkOutArgs[]): Promise<tilo.ActionAnswer[]>;
  clear_marks(): Promise<void>;
  /** Runs `clear_marks` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_marksMany(items: ClearMarksArgs[]): Promise<tilo.ActionAnswer[]>;
  set_frame_rate(args: {
    fps: number;
  }): Promise<void>;
  /** Runs `set_frame_rate` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_frame_rateMany(items: SetFrameRateArgs[]): Promise<tilo.ActionAnswer[]>;
  set_frame_size(args: {
    width: number;
    height: number;
  }): Promise<void>;
  /** Runs `set_frame_size` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_frame_sizeMany(items: SetFrameSizeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes its shape from a source: what 'Use its format' does. */
  adopt_format(args: {
    m: Media;
  }): Promise<void>;
  /** Runs `adopt_format` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  adopt_formatMany(items: AdoptFormatArgs[]): Promise<tilo.ActionAnswer[]>;
  add_track(args: {
    form: Form;
  }): Promise<Track>;
  delete_track(args: {
    t: Track;
  }): Promise<void>;
  /** Runs `delete_track` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_trackMany(items: DeleteTrackArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Makes two clips one shot's picture and sound: they move and cut together. */
  link_clips(args: {
    a: Clip;
    b: Clip;
  }): Promise<void>;
  /** Runs `link_clips` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  link_clipsMany(items: LinkClipsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a picture clip's own sound onto an audio clip of its own, linked to it. */
  split_audio(args: {
    c: Clip;
  }): Promise<void>;
  /** Runs `split_audio` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  split_audioMany(items: SplitAudioArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Puts two clips side by side: each half the frame, one left and one right. */
  place_side_by_side(args: {
    left: Clip;
    right: Clip;
  }): Promise<void>;
  /** Runs `place_side_by_side` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  place_side_by_sideMany(items: PlaceSideBySideArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a source out of the bin, and every clip that showed it with it. */
  delete_media(args: {
    m: Media;
  }): Promise<void>;
  /** Runs `delete_media` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_mediaMany(items: DeleteMediaArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Draws the frame the preview shows at `at`. */
  preview(args: {
    at: number;
  }): Promise<void>;
  /** Runs `preview` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  previewMany(items: PreviewArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Draws the frame at the ruler's place `x`, as a drag along it asks for. */
  seek_at(args: {
    x: number;
    span: number;
    scroll: number;
  }): Promise<void>;
  /** Runs `seek_at` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  seek_atMany(items: SeekAtArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renders the cut small, so the window plays it. */
  preview_cut(): Promise<void>;
  /** Runs `preview_cut` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  preview_cutMany(items: PreviewCutArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renders the cut to a file. */
  export_cut(args: {
    resolution: string;
    container: string;
    fps: number;
  }): Promise<void>;
  /** Runs `export_cut` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  export_cutMany(items: ExportCutArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks how far the running render has got. */
  poll_render(): Promise<void>;
  /** Runs `poll_render` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  poll_renderMany(items: PollRenderArgs[]): Promise<tilo.ActionAnswer[]>;
  cancel_render(): Promise<void>;
  /** Runs `cancel_render` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cancel_renderMany(items: CancelRenderArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Project` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROJECT_ACTIONS = ["split_at", "split_atMany", "shifted", "trouble", "troubleMany", "absorb", "adopt_probe", "adopt_probeMany", "refresh_source", "refresh_sourceMany", "relink", "relinkMany", "mark_missing", "mark_missingMany", "draw_still", "draw_stillMany", "begin_render", "begin_renderMany", "rename_project", "rename_projectMany", "import_files", "import_filesMany", "import_one", "import_oneMany", "save_project", "export_file", "place", "place_media", "commit_drag", "commit_dragMany", "split_clip", "split_clipMany", "cut_at", "cut_atMany", "remove_clip", "remove_clipMany", "ripple_delete", "ripple_deleteMany", "nudge", "nudgeMany", "redraw", "redrawMany", "sample_key", "sample_keyMany", "filmstrip", "filmstripMany", "draw_next_strip", "draw_next_stripMany", "add_title", "add_marker", "remove_marker", "remove_markerMany", "mark_in", "mark_inMany", "mark_out", "mark_outMany", "clear_marks", "clear_marksMany", "set_frame_rate", "set_frame_rateMany", "set_frame_size", "set_frame_sizeMany", "adopt_format", "adopt_formatMany", "add_track", "delete_track", "delete_trackMany", "link_clips", "link_clipsMany", "split_audio", "split_audioMany", "place_side_by_side", "place_side_by_sideMany", "delete_media", "delete_mediaMany", "preview", "previewMany", "seek_at", "seek_atMany", "preview_cut", "preview_cutMany", "export_cut", "export_cutMany", "poll_render", "poll_renderMany", "cancel_render", "cancel_renderMany"] as const;

/** The `tl.so.tilo.video-editor.Project` kind at `path`. */
export function project(ws: tilo.Workspace, path: string): Project {
  return resourceProxy(ws, path, SURFACE, PROJECT_ACTIONS) as unknown as Project;
}

/** Editor — `tl.so.tilo.video-editor.Editor`. */
export interface Editor extends tilo.Resource {
  ws67tq4i: Array<tilo.Entry<Project>>;  // projects
  i3vrs6nt: Project | null;  // open
  qyx2uds6: Array<tilo.Entry<unknown>>;  // sessions
  "3kenkvdb": unknown | null;  // playing
  /** Starts a cut: its shape and its first two tracks. */
  new_project(): Promise<Project>;
  /** Opens the cut the window shows, and draws its first frame where the window has not shown one yet: a cut opens on its picture, not on an empty preview. */
  open_project(args: {
    p: Project;
  }): Promise<void>;
  /** Runs `open_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_projectMany(items: OpenProjectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The session this window plays through: the one it has, or one made now. */
  session(): Promise<unknown>;
  forget_project(args: {
    p: Project;
  }): Promise<void>;
  /** Runs `forget_project` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forget_projectMany(items: ForgetProjectArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens a video, a sound or a picture in a cut of its own: what Files and the person's own system offer the editor for. */
  open_media(args: {
    file: unknown;
  }): Promise<Project>;
  /** Opens a project file a person chose: one undoable edit that makes the cut it holds. */
  open_project_file(args: {
    file: unknown;
  }): Promise<Project>;
}

/** The actions `Editor` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EDITOR_ACTIONS = ["new_project", "open_project", "open_projectMany", "session", "forget_project", "forget_projectMany", "open_media", "open_project_file"] as const;

/** The `tl.so.tilo.video-editor.Editor` kind at `path`. */
export function editor(ws: tilo.Workspace, path: string): Editor {
  return resourceProxy(ws, path, SURFACE, EDITOR_ACTIONS) as unknown as Editor;
}

/** Probed — `tl.so.tilo.video-editor.Probed`. */
export interface Probed extends tilo.Resource {
  bc2o5yk6: string;  // kind
  jb7opba6: number;  // duration
  duh5etpe: number;  // width
  ibouz53w: number;  // height
  eci2btbm: number;  // fps
  cg3roaa5: boolean;  // has_audio
  r72d7i4q: string;  // codec
}

/** The actions `Probed` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROBED_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.Probed` kind at `path`. */
export function probed(ws: tilo.Workspace, path: string): Probed {
  return resourceProxy(ws, path, SURFACE, PROBED_ACTIONS) as unknown as Probed;
}

/** ProbeAsk — `tl.so.tilo.video-editor.ProbeAsk`. */
export interface ProbeAsk extends tilo.Resource {
  dx4zjyh5: string;  // url
}

/** The actions `ProbeAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROBEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.ProbeAsk` kind at `path`. */
export function probeAsk(ws: tilo.Workspace, path: string): ProbeAsk {
  return resourceProxy(ws, path, SURFACE, PROBEASK_ACTIONS) as unknown as ProbeAsk;
}

/** StillAsk — `tl.so.tilo.video-editor.StillAsk`. */
export interface StillAsk extends tilo.Resource {
  itbf6wf4: string;  // project
  "5klklgrm": number;  // at
  zrjp2ayo: number;  // width
  c7hx2nde: number;  // height
}

/** The actions `StillAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STILLASK_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.StillAsk` kind at `path`. */
export function stillAsk(ws: tilo.Workspace, path: string): StillAsk {
  return resourceProxy(ws, path, SURFACE, STILLASK_ACTIONS) as unknown as StillAsk;
}

/** StillAnswer — `tl.so.tilo.video-editor.StillAnswer`. */
export interface StillAnswer extends tilo.Resource {
  fqmupsg5: string;  // url
}

/** The actions `StillAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STILLANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.StillAnswer` kind at `path`. */
export function stillAnswer(ws: tilo.Workspace, path: string): StillAnswer {
  return resourceProxy(ws, path, SURFACE, STILLANSWER_ACTIONS) as unknown as StillAnswer;
}

/** WaveAsk — `tl.so.tilo.video-editor.WaveAsk`. */
export interface WaveAsk extends tilo.Resource {
  hv2hn45e: string;  // url
  u2gsuh7h: number;  // buckets
}

/** The actions `WaveAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WAVEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.WaveAsk` kind at `path`. */
export function waveAsk(ws: tilo.Workspace, path: string): WaveAsk {
  return resourceProxy(ws, path, SURFACE, WAVEASK_ACTIONS) as unknown as WaveAsk;
}

/** Peaks — `tl.so.tilo.video-editor.Peaks`. */
export interface Peaks extends tilo.Resource {
  xatqduw2: number[];  // peaks
}

/** The actions `Peaks` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PEAKS_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.Peaks` kind at `path`. */
export function peaks(ws: tilo.Workspace, path: string): Peaks {
  return resourceProxy(ws, path, SURFACE, PEAKS_ACTIONS) as unknown as Peaks;
}

/** RenderAsk — `tl.so.tilo.video-editor.RenderAsk`. */
export interface RenderAsk extends tilo.Resource {
  "4xhy5qpi": string;  // project
  nlg2n4yg: string;  // resolution
  n3bktwt4: string;  // container
  ylbeg2vl: number;  // fps
  mqmxnv4s: string;  // name
}

/** The actions `RenderAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RENDERASK_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.RenderAsk` kind at `path`. */
export function renderAsk(ws: tilo.Workspace, path: string): RenderAsk {
  return resourceProxy(ws, path, SURFACE, RENDERASK_ACTIONS) as unknown as RenderAsk;
}

/** JobAnswer — `tl.so.tilo.video-editor.JobAnswer`. */
export interface JobAnswer extends tilo.Resource {
  "2qflgu2y": string;  // job
}

/** The actions `JobAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const JOBANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.JobAnswer` kind at `path`. */
export function jobAnswer(ws: tilo.Workspace, path: string): JobAnswer {
  return resourceProxy(ws, path, SURFACE, JOBANSWER_ACTIONS) as unknown as JobAnswer;
}

/** JobState — `tl.so.tilo.video-editor.JobState`. */
export interface JobState extends tilo.Resource {
  "2lqqe2u4": string;  // state
  wddixskv: number;  // done
  yfo7z67v: number;  // total
  ivfsfn6q: string;  // url
  tlb73mjx: string;  // error
}

/** The actions `JobState` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const JOBSTATE_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.JobState` kind at `path`. */
export function jobState(ws: tilo.Workspace, path: string): JobState {
  return resourceProxy(ws, path, SURFACE, JOBSTATE_ACTIONS) as unknown as JobState;
}

/** Shot — `tl.so.tilo.video-editor.Shot`. */
export interface Shot extends tilo.Resource {
  fagho4gr: string;  // name
  fsxs4zep: number;  // fps
  ebteysdk: number;  // width
  a4torznk: number;  // height
  v6ck4d4l: number;  // mark_in
  hzxujhk3: number;  // mark_out
  yjp3sehi: string[];  // media
  q3ecfgq3: string[];  // tracks
  wgoywspl: string[];  // clips
  w26d474g: string[];  // markers
}

/** The actions `Shot` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHOT_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.Shot` kind at `path`. */
export function shot(ws: tilo.Workspace, path: string): Shot {
  return resourceProxy(ws, path, SURFACE, SHOT_ACTIONS) as unknown as Shot;
}

/** ShotMedia — `tl.so.tilo.video-editor.ShotMedia`. */
export interface ShotMedia extends tilo.Resource {
  jws44pym: string;  // id
  "6irok5cm": string;  // name
  "2yxap4ne": string;  // kind
  j3rpcgim: number;  // duration
  pwfziujj: number;  // width
  "7cokd24o": number;  // height
  ldk5mv2m: number;  // fps
  "5gvee3he": number;  // palette
  betxnhkz: string;  // path
  "7ppacpgk": boolean;  // has_audio
  cosd36fu: string;  // content
  tmz6b4vk: number;  // size
  uk5cqaxq: number[];  // color
  a5zwa247: string;  // align
  mjhc4g63: string;  // font
  lykfeg5x: boolean;  // bold
  npxtqckt: boolean;  // italic
  "4b6rl7fz": number;  // backdrop
}

/** The actions `ShotMedia` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHOTMEDIA_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.ShotMedia` kind at `path`. */
export function shotMedia(ws: tilo.Workspace, path: string): ShotMedia {
  return resourceProxy(ws, path, SURFACE, SHOTMEDIA_ACTIONS) as unknown as ShotMedia;
}

/** ShotTrack — `tl.so.tilo.video-editor.ShotTrack`. */
export interface ShotTrack extends tilo.Resource {
  wzjt7soo: string;  // id
  "2phnzuqk": string;  // name
  mb6mtsoz: string;  // kind
  jr2eyeqg: number;  // index
  wyqopmjv: boolean;  // muted
  "3enbjrqj": boolean;  // solo
  lr3pdaah: boolean;  // locked
  zse3uh5c: number;  // gain_db
  aix6cf7x: number;  // pan
}

/** The actions `ShotTrack` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHOTTRACK_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.ShotTrack` kind at `path`. */
export function shotTrack(ws: tilo.Workspace, path: string): ShotTrack {
  return resourceProxy(ws, path, SURFACE, SHOTTRACK_ACTIONS) as unknown as ShotTrack;
}

/** ShotClip — `tl.so.tilo.video-editor.ShotClip`. */
export interface ShotClip extends tilo.Resource {
  jeeiaroo: string;  // id
  akxlh23d: string;  // track
  pptlby5v: string;  // media
  "3dgm7b2b": number;  // start
  zbz5amfa: number;  // in_point
  pkiaohdd: number;  // out_point
  xi2qtarh: number;  // opacity
  chhkku2t: number;  // volume
  s2fdck6n: string;  // transition
  rrf4qn3g: number;  // transition_duration
  xubtbe5w: number;  // fade_in
  jllwurew: number;  // fade_out
  clwasyxv: number;  // palette
  uge3tmju: number;  // speed_rate
  rxbq7fqf: boolean;  // speed_reverse
  "5utmqjdy": number;  // brightness
  "5kf4asry": number;  // contrast
  ugpps35t: number;  // saturation
  ueazn2zi: number;  // scale_x
  qxxmhhdr: number;  // scale_y
  hl2h6f76: number;  // pos_x
  nhestloi: number;  // pos_y
  ckydsf7y: number;  // rotation
  ttpiryuy: number;  // crop_left
  dazt7wt5: number;  // crop_right
  o5353t6h: number;  // crop_top
  lsgktf3d: number;  // crop_bottom
  mq4hgil7: string[];  // effects
  "6ahtwc73": string[];  // keys
  "2i54q4ly": boolean;  // key_enabled
  hcm6xmnu: number[];  // key_color
  ss3pryvp: number;  // key_tolerance
  odmxpb6p: number;  // key_softness
  vwxtiqh4: number;  // key_spill
  v6saof6k: string;  // mask_shape
  drrsji3r: number;  // mask_x
  q63mhtzf: number;  // mask_y
  w3owjsm6: number;  // mask_w
  lrguhlws: number;  // mask_h
  u4sfvd6p: number;  // mask_rotation
  zteue6rc: number;  // mask_feather
  ah7jtdcc: boolean;  // mask_invert
  "2wno4yo2": string;  // link
  "3rzhv2vf": boolean;  // link_follows
  hulnlv2t: boolean;  // sound_split
}

/** The actions `ShotClip` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHOTCLIP_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.ShotClip` kind at `path`. */
export function shotClip(ws: tilo.Workspace, path: string): ShotClip {
  return resourceProxy(ws, path, SURFACE, SHOTCLIP_ACTIONS) as unknown as ShotClip;
}

/** ShotEffect — `tl.so.tilo.video-editor.ShotEffect`. */
export interface ShotEffect extends tilo.Resource {
  ai7thy2p: string;  // id
  seoccvzw: string;  // form
  y2yjk3ou: number;  // amount
}

/** The actions `ShotEffect` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHOTEFFECT_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.ShotEffect` kind at `path`. */
export function shotEffect(ws: tilo.Workspace, path: string): ShotEffect {
  return resourceProxy(ws, path, SURFACE, SHOTEFFECT_ACTIONS) as unknown as ShotEffect;
}

/** ShotKey — `tl.so.tilo.video-editor.ShotKey`. */
export interface ShotKey extends tilo.Resource {
  fpeyg4lm: string;  // param
  frbcwy5c: number;  // t
  hxcqnoii: number;  // v
  pf3f6zou: string;  // interp
}

/** The actions `ShotKey` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHOTKEY_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.ShotKey` kind at `path`. */
export function shotKey(ws: tilo.Workspace, path: string): ShotKey {
  return resourceProxy(ws, path, SURFACE, SHOTKEY_ACTIONS) as unknown as ShotKey;
}

/** ThumbAsk — `tl.so.tilo.video-editor.ThumbAsk`. */
export interface ThumbAsk extends tilo.Resource {
  zbk6etmx: string;  // url
  mom4w6zg: number;  // at
  qdxfi3dv: number;  // width
  a4wvyrpr: number;  // height
}

/** The actions `ThumbAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const THUMBASK_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.ThumbAsk` kind at `path`. */
export function thumbAsk(ws: tilo.Workspace, path: string): ThumbAsk {
  return resourceProxy(ws, path, SURFACE, THUMBASK_ACTIONS) as unknown as ThumbAsk;
}

/** SampleAsk — `tl.so.tilo.video-editor.SampleAsk`. */
export interface SampleAsk extends tilo.Resource {
  "47t7hgmm": string;  // project
  lph6xeeo: number;  // at
  hpbggrop: number;  // x
  nklgpqyv: number;  // y
}

/** The actions `SampleAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAMPLEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.SampleAsk` kind at `path`. */
export function sampleAsk(ws: tilo.Workspace, path: string): SampleAsk {
  return resourceProxy(ws, path, SURFACE, SAMPLEASK_ACTIONS) as unknown as SampleAsk;
}

/** SampleAnswer — `tl.so.tilo.video-editor.SampleAnswer`. */
export interface SampleAnswer extends tilo.Resource {
  twwt37df: number;  // red
  "6j7i6jgf": number;  // green
  qbxkptqm: number;  // blue
}

/** The actions `SampleAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAMPLEANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.SampleAnswer` kind at `path`. */
export function sampleAnswer(ws: tilo.Workspace, path: string): SampleAnswer {
  return resourceProxy(ws, path, SURFACE, SAMPLEANSWER_ACTIONS) as unknown as SampleAnswer;
}

/** ShotMarker — `tl.so.tilo.video-editor.ShotMarker`. */
export interface ShotMarker extends tilo.Resource {
  "5evudwdu": string;  // id
  bh2yiei5: number;  // t
  ajyxetst: string;  // name
  "2wvqaalc": number;  // palette
  ueznappo: string;  // note
}

/** The actions `ShotMarker` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHOTMARKER_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.ShotMarker` kind at `path`. */
export function shotMarker(ws: tilo.Workspace, path: string): ShotMarker {
  return resourceProxy(ws, path, SURFACE, SHOTMARKER_ACTIONS) as unknown as ShotMarker;
}

/** Saved — `tl.so.tilo.video-editor.Saved`. */
export interface Saved extends tilo.Resource {
  y5f2zb7y: string;  // name
  pfj7sab5: number;  // fps
  "4nw6mpgl": number;  // width
  tyqdqd4o: number;  // height
  idvvsvmm: number;  // mark_in
  dadn3d73: number;  // mark_out
  hzulw2ku: string[];  // media
  sgtdffn3: string[];  // tracks
  tgtkyvzv: string[];  // clips
  mbkqck5v: string[];  // markers
}

/** The actions `Saved` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAVED_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.Saved` kind at `path`. */
export function saved(ws: tilo.Workspace, path: string): Saved {
  return resourceProxy(ws, path, SURFACE, SAVED_ACTIONS) as unknown as Saved;
}

/** SavedMedia — `tl.so.tilo.video-editor.SavedMedia`. */
export interface SavedMedia extends tilo.Resource {
  y4ivogi5: string;  // name
  "4yybplx2": string;  // form
  lrk7dehr: number;  // duration
  zzc32v7l: number;  // width
  qj5gefuh: number;  // height
  "23dpu2sh": number;  // fps
  soogrhvl: number;  // palette
  ooln2ibo: string;  // path
  ydskn4p7: boolean;  // has_audio
  im5ank6o: string;  // words
  tmrphs4y: number;  // size
  kqelnlf5: number;  // red
  vikauvue: number;  // green
  l67cmsns: number;  // blue
  ozd3qrh5: number;  // alpha
  ytkxp4t4: string;  // align
  oecwbkzv: boolean;  // bold
  qefzzmnw: boolean;  // italic
  npetd5wt: number;  // backdrop
}

/** The actions `SavedMedia` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAVEDMEDIA_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.SavedMedia` kind at `path`. */
export function savedMedia(ws: tilo.Workspace, path: string): SavedMedia {
  return resourceProxy(ws, path, SURFACE, SAVEDMEDIA_ACTIONS) as unknown as SavedMedia;
}

/** SavedTrack — `tl.so.tilo.video-editor.SavedTrack`. */
export interface SavedTrack extends tilo.Resource {
  hahxd2bf: string;  // name
  "6eogjiom": string;  // form
  kpfeeasv: number;  // index
  objfxiib: boolean;  // muted
  xucxzrou: boolean;  // solo
  ziybhwce: boolean;  // locked
  mddfj6qk: number;  // gain_db
  kmg2laxv: number;  // pan
}

/** The actions `SavedTrack` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAVEDTRACK_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.SavedTrack` kind at `path`. */
export function savedTrack(ws: tilo.Workspace, path: string): SavedTrack {
  return resourceProxy(ws, path, SURFACE, SAVEDTRACK_ACTIONS) as unknown as SavedTrack;
}

/** SavedClip — `tl.so.tilo.video-editor.SavedClip`. */
export interface SavedClip extends tilo.Resource {
  xelrculp: string;  // name
  nakmpr6q: number;  // track
  b6xll7vm: number;  // media
  "3styztut": number;  // start
  fosnfakw: number;  // in_point
  fyyrf7ca: number;  // out_point
  kl2xarv5: number;  // opacity
  xjyxglh4: number;  // volume
  "3ayzf2x5": string;  // transition
  dqqoukz2: number;  // transition_duration
  ked6w3da: number;  // fade_in
  gs542y2n: number;  // fade_out
  xuo76wt5: number;  // speed_rate
  joyeavyw: boolean;  // reverse
  y7jv62ls: number;  // brightness
  g5rdmkth: number;  // contrast
  zgf5atrf: number;  // saturation
  r4zzuhna: number;  // scale_x
  u3nbauiz: number;  // scale_y
  "7rxjc3mu": number;  // pos_x
  "4fkenwwm": number;  // pos_y
  n2rqdsw7: number;  // rotation
  fycn5pvk: number;  // crop_left
  "2tb4h5uc": number;  // crop_right
  "7madt25j": number;  // crop_top
  "3wy2xkbg": number;  // crop_bottom
  "2ui2wpyh": boolean;  // key_enabled
  cxqyoowf: number;  // key_tolerance
  rjd3q7ct: number;  // key_softness
  avubfu74: number;  // key_spill
  smqr4d4n: string;  // mask_shape
  yr4ncz4w: number;  // mask_x
  "7g7aeosa": number;  // mask_y
  jasgkfww: number;  // mask_w
  f4dke5o2: number;  // mask_h
  fdrernpm: number;  // mask_rotation
  to3xcmob: number;  // mask_feather
  "6dr3jtxx": boolean;  // mask_invert
  pdhb3rjf: string;  // link
  "6oop3g3z": string[];  // keys
}

/** The actions `SavedClip` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAVEDCLIP_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.SavedClip` kind at `path`. */
export function savedClip(ws: tilo.Workspace, path: string): SavedClip {
  return resourceProxy(ws, path, SURFACE, SAVEDCLIP_ACTIONS) as unknown as SavedClip;
}

/** SavedMarker — `tl.so.tilo.video-editor.SavedMarker`. */
export interface SavedMarker extends tilo.Resource {
  s6k7ckvj: string;  // name
  bglc57r5: number;  // t
  "2zmyfxy2": number;  // palette
  uqvihrf3: string;  // note
}

/** The actions `SavedMarker` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAVEDMARKER_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.SavedMarker` kind at `path`. */
export function savedMarker(ws: tilo.Workspace, path: string): SavedMarker {
  return resourceProxy(ws, path, SURFACE, SAVEDMARKER_ACTIONS) as unknown as SavedMarker;
}

/** Drawn — `tl.so.tilo.video-editor.Drawn`. */
export interface Drawn extends tilo.Resource {
  jaecw4vv: unknown[];  // shapes
  msyhaonn: string[];  // owners
}

/** The actions `Drawn` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DRAWN_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor.Drawn` kind at `path`. */
export function drawn(ws: tilo.Workspace, path: string): Drawn {
  return resourceProxy(ws, path, SURFACE, DRAWN_ACTIONS) as unknown as Drawn;
}

/** Video Editor — `tl.so.tilo.video-editor`. */
export interface VideoEditor extends tilo.Resource {
}

/** The actions `VideoEditor` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const VIDEOEDITOR_ACTIONS = [] as const;

/** The `tl.so.tilo.video-editor` kind at `path`. */
export function videoEditor(ws: tilo.Workspace, path: string): VideoEditor {
  return resourceProxy(ws, path, SURFACE, VIDEOEDITOR_ACTIONS) as unknown as VideoEditor;
}

