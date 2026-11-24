/**
 * Types for Video Editor (`so.tilo.video-editor`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Project` is a `Resource` of the app's `video-editor.project`, made by its factory with the
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
export const SURFACE = "so.tilo.video-editor@f0631a84d79983e3";

export interface ChromaKey {
  enabled: boolean;
  color: number[];
  tolerance: number;
  softness: number;
  spill: number;
}

export interface Clip {
  id: string;
  track: string;
  media: string;
  start: number;
  in_point: number;
  out_point: number;
  opacity: number;
  volume: number;
  transition: Transition;
  palette: number;
  filters: Filters;
  curves: unknown;
  fade_in: number;
  fade_out: number;
  transition_duration: number;
  effects: Array<tilo.Entry<ClipEffect>>;
  transform: Transform;
  speed: Speed;
  key: ChromaKey;
  mask: Mask;
  anim: unknown;
  markers: Array<tilo.Entry<Marker>>;
  link: string | null;
  sound_split: boolean;
  link_follows: boolean;
}

export interface ClipEffect {
  id: string;
  kind: EffectKind;
  amount: number;
}

export interface ClipPlacement {
  media: string;
  track: string | null;
  at: number | null;
  in: number | null;
  out: number | null;
  fade_in: number | null;
  fade_out: number | null;
}

/** One of "Grayscale", "Sepia", "Invert", "Warm", "Cool", "Vignette". */
export type EffectKind = "Grayscale" | "Sepia" | "Invert" | "Warm" | "Cool" | "Vignette";

export interface Filters {
  brightness: number;
  contrast: number;
  saturation: number;
}

export interface Marker {
  id: string;
  t: number;
  name: string;
  palette: number;
  note: string;
}

export interface Mask {
  shape: MaskShape;
  x: number;
  y: number;
  w: number;
  h: number;
  rotation: number;
  feather: number;
  invert: boolean;
}

/** One of "None", "Rect", "Ellipse". */
export type MaskShape = "None" | "Rect" | "Ellipse";

export interface MediaItem {
  id: string;
  name: string;
  kind: MediaKind;
  duration: number;
  width: number;
  height: number;
  fps: number;
  palette: number;
  path: string;
  text: TextSource | null;
  has_audio: boolean;
}

/** One of "Video", "Audio", "Image", "Text". */
export type MediaKind = "Video" | "Audio" | "Image" | "Text";

export interface Speed {
  rate: number;
  reverse: boolean;
}

/** One of "Left", "Center", "Right". */
export type TextAlign = "Left" | "Center" | "Right";

export interface TextSource {
  content: string;
  size: number;
  color: number[];
  align: TextAlign;
  font: string;
  bold: boolean;
  italic: boolean;
  backdrop: number;
}

export interface Track {
  id: string;
  name: string;
  kind: TrackKind;
  index: number;
  muted: boolean;
  locked: boolean;
  solo: boolean;
  gain_db: number;
  pan: number;
}

/** One of "video", "audio". */
export type TrackKind = "video" | "audio";

export interface Transform {
  scale_x: number;
  scale_y: number;
  pos_x: number;
  pos_y: number;
  rotation: number;
  anchor_x: number;
  anchor_y: number;
  crop_left: number;
  crop_right: number;
  crop_top: number;
  crop_bottom: number;
}

/** One of "None", "CrossDissolve", "FadeIn", "DipToBlack", "Wipe". */
export type Transition = "None" | "CrossDissolve" | "FadeIn" | "DipToBlack" | "Wipe";

/** The arguments of `set-frame-size`. */
export interface SetFrameSizeArgs {
  width: number;
  height: number;
}

/** The arguments of `set-frame-rate`. */
export interface SetFrameRateArgs {
  fps: number;
}

/** The arguments of `set-render-range`. */
export interface SetRenderRangeArgs {
  start: number;
  end: number;
}

/** The arguments of `mark-in`. */
export interface MarkInArgs {
  at: number;
}

/** The arguments of `mark-out`. */
export interface MarkOutArgs {
  at: number;
}

/** The arguments of `clear-marks`. */
export interface ClearMarksArgs {
}

/** The arguments of `add-marker`. */
export interface AddMarkerArgs {
  at: number;
  name: string;
}

/** The arguments of `place-two-angles`. */
export interface PlaceTwoAnglesArgs {
  at: number;
}

/** The arguments of `unplace-two-angles`. */
export interface UnplaceTwoAnglesArgs {
  at: number;
}

/** The arguments of `remove-marker`. */
export interface RemoveMarkerArgs {
  marker: string;
}

/** The arguments of `add-track`. */
export interface AddTrackArgs {
  kind: TrackKind;
}

/** The arguments of `reset-mix`. */
export interface ResetMixArgs {
  track: string;
}

/** The arguments of `trim-tracks`. */
export interface TrimTracksArgs {
  tracks: string[];
  db: number;
  headroom: number;
}

/** The arguments of `delete-track`. */
export interface DeleteTrackArgs {
  track: string;
}

/** The arguments of `place-clip`. */
export interface PlaceClipArgs {
  media: string;
  track: string;
  start: number;
}

/** The arguments of `delete-media`. */
export interface DeleteMediaArgs {
  media: string;
}

/** The arguments of `use-as-project-format`. */
export interface UseAsProjectFormatArgs {
  media: string;
}

/** The arguments of `delete`. */
export interface DeleteArgs {
  clip: string;
}

/** The arguments of `ripple-delete`. */
export interface RippleDeleteArgs {
  clip: string;
}

/** The arguments of `split-clip`. */
export interface SplitClipArgs {
  clip: string;
  at: number;
}

/** The arguments of `trim-in-clip`. */
export interface TrimInClipArgs {
  clip: string;
  at: number;
}

/** The arguments of `trim-out-clip`. */
export interface TrimOutClipArgs {
  clip: string;
  at: number;
}

/** The arguments of `set-speed`. */
export interface SetSpeedArgs {
  clip: string;
  rate: number;
}

/** The arguments of `toggle-reverse`. */
export interface ToggleReverseArgs {
  clip: string;
}

/** The arguments of `reset-speed`. */
export interface ResetSpeedArgs {
  clip: string;
}

/** The arguments of `set-param`. */
export interface SetParamArgs {
  clip: string;
  param: string;
  at: number;
  value: number;
}

/** The arguments of `add-effect`. */
export interface AddEffectArgs {
  clip: string;
  effect: EffectKind;
}

/** The arguments of `remove-effect`. */
export interface RemoveEffectArgs {
  clip: string;
  effect: string;
}

/** The arguments of `move-effect`. */
export interface MoveEffectArgs {
  clip: string;
  effect: string;
  up: boolean;
}

/** The arguments of `set-effect-amount`. */
export interface SetEffectAmountArgs {
  clip: string;
  effect: string;
  amount: number;
}

/** The arguments of `clear-effects`. */
export interface ClearEffectsArgs {
  clip: string;
}

/** The arguments of `unlink`. */
export interface UnlinkArgs {
  clip: string;
}

/** The arguments of `link-audio`. */
export interface LinkAudioArgs {
  clip: string;
}

/** The arguments of `link-with`. */
export interface LinkWithArgs {
  clip: string;
  other: string;
}

/** Project — `video-editor.project`. */
export interface Project extends tilo.Resource {
  name: string;
  fps: number;
  width: number;
  height: number;
  format_explicit: boolean;
  mark_in: number | null;
  mark_out: number | null;
  media: Array<tilo.Entry<MediaItem>>;
  tracks: Array<tilo.Entry<Track>>;
  clips: Array<tilo.Entry<Clip>>;
  markers: Array<tilo.Entry<Marker>>;
  setFrameSize(args: {
    width: number;
    height: number;
  }): Promise<void>;
  /** Runs `set-frame-size` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setFrameSizeMany(items: SetFrameSizeArgs[]): Promise<tilo.ActionAnswer[]>;
  setFrameRate(args: {
    fps: number;
  }): Promise<void>;
  /** Runs `set-frame-rate` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setFrameRateMany(items: SetFrameRateArgs[]): Promise<tilo.ActionAnswer[]>;
  setRenderRange(args: {
    start: number;
    end: number;
  }): Promise<void>;
  /** Runs `set-render-range` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setRenderRangeMany(items: SetRenderRangeArgs[]): Promise<tilo.ActionAnswer[]>;
  markIn(args: {
    at: number;
  }): Promise<void>;
  /** Runs `mark-in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  markInMany(items: MarkInArgs[]): Promise<tilo.ActionAnswer[]>;
  markOut(args: {
    at: number;
  }): Promise<void>;
  /** Runs `mark-out` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  markOutMany(items: MarkOutArgs[]): Promise<tilo.ActionAnswer[]>;
  clearMarks(): Promise<void>;
  /** Runs `clear-marks` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clearMarksMany(items: ClearMarksArgs[]): Promise<tilo.ActionAnswer[]>;
  addTitle(args: {
    text: string;
    colour?: string;
    size?: number;
    align?: string;
  }): Promise<unknown>;
  setTitle(args: {
    media: string;
    text?: string;
    colour?: string;
    size?: number;
    align?: string;
  }): Promise<unknown>;
  addClip(args: {
    media: string;
    track?: string;
    at?: number;
    in?: number;
    out?: number;
    fade_in?: number;
    fade_out?: number;
  }): Promise<unknown>;
  moveClip(args: {
    clip: string;
    at: number;
    track?: string;
  }): Promise<unknown>;
  setClip(args: {
    clip: string;
    fade_in?: number;
    fade_out?: number;
    opacity?: number;
    volume?: number;
    transition?: string;
    transition_duration?: number;
  }): Promise<unknown>;
  removeClip(args: {
    clip: string;
    ripple?: boolean;
  }): Promise<unknown>;
  addMarker(args: {
    at: number;
    name: string;
  }): Promise<void>;
  /** Runs `add-marker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addMarkerMany(items: AddMarkerArgs[]): Promise<tilo.ActionAnswer[]>;
  placeTwoAngles(args: {
    at: number;
  }): Promise<void>;
  /** Runs `place-two-angles` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  placeTwoAnglesMany(items: PlaceTwoAnglesArgs[]): Promise<tilo.ActionAnswer[]>;
  unplaceTwoAngles(args: {
    at: number;
  }): Promise<void>;
  /** Runs `unplace-two-angles` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unplaceTwoAnglesMany(items: UnplaceTwoAnglesArgs[]): Promise<tilo.ActionAnswer[]>;
  removeMarker(args: {
    marker: string;
  }): Promise<void>;
  /** Runs `remove-marker` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeMarkerMany(items: RemoveMarkerArgs[]): Promise<tilo.ActionAnswer[]>;
  addTrack(args: {
    kind: TrackKind;
  }): Promise<void>;
  /** Runs `add-track` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addTrackMany(items: AddTrackArgs[]): Promise<tilo.ActionAnswer[]>;
  resetMix(args: {
    track: string;
  }): Promise<void>;
  /** Runs `reset-mix` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resetMixMany(items: ResetMixArgs[]): Promise<tilo.ActionAnswer[]>;
  trimTracks(args: {
    tracks: string[];
    db: number;
    headroom: number;
  }): Promise<void>;
  /** Runs `trim-tracks` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  trimTracksMany(items: TrimTracksArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteTrack(args: {
    track: string;
  }): Promise<void>;
  /** Runs `delete-track` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteTrackMany(items: DeleteTrackArgs[]): Promise<tilo.ActionAnswer[]>;
  placeClip(args: {
    media: string;
    track: string;
    start: number;
  }): Promise<void>;
  /** Runs `place-clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  placeClipMany(items: PlaceClipArgs[]): Promise<tilo.ActionAnswer[]>;
  deleteMedia(args: {
    media: string;
  }): Promise<void>;
  /** Runs `delete-media` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMediaMany(items: DeleteMediaArgs[]): Promise<tilo.ActionAnswer[]>;
  useAsProjectFormat(args: {
    media: string;
  }): Promise<void>;
  /** Runs `use-as-project-format` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  useAsProjectFormatMany(items: UseAsProjectFormatArgs[]): Promise<tilo.ActionAnswer[]>;
  delete(args: {
    clip: string;
  }): Promise<void>;
  /** Runs `delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  deleteMany(items: DeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  rippleDelete(args: {
    clip: string;
  }): Promise<void>;
  /** Runs `ripple-delete` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rippleDeleteMany(items: RippleDeleteArgs[]): Promise<tilo.ActionAnswer[]>;
  splitClip(args: {
    clip: string;
    at: number;
  }): Promise<void>;
  /** Runs `split-clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  splitClipMany(items: SplitClipArgs[]): Promise<tilo.ActionAnswer[]>;
  trimInClip(args: {
    clip: string;
    at: number;
  }): Promise<void>;
  /** Runs `trim-in-clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  trimInClipMany(items: TrimInClipArgs[]): Promise<tilo.ActionAnswer[]>;
  trimOutClip(args: {
    clip: string;
    at: number;
  }): Promise<void>;
  /** Runs `trim-out-clip` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  trimOutClipMany(items: TrimOutClipArgs[]): Promise<tilo.ActionAnswer[]>;
  setSpeed(args: {
    clip: string;
    rate: number;
  }): Promise<void>;
  /** Runs `set-speed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setSpeedMany(items: SetSpeedArgs[]): Promise<tilo.ActionAnswer[]>;
  toggleReverse(args: {
    clip: string;
  }): Promise<void>;
  /** Runs `toggle-reverse` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggleReverseMany(items: ToggleReverseArgs[]): Promise<tilo.ActionAnswer[]>;
  resetSpeed(args: {
    clip: string;
  }): Promise<void>;
  /** Runs `reset-speed` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resetSpeedMany(items: ResetSpeedArgs[]): Promise<tilo.ActionAnswer[]>;
  setParam(args: {
    clip: string;
    param: string;
    at: number;
    value: number;
  }): Promise<void>;
  /** Runs `set-param` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setParamMany(items: SetParamArgs[]): Promise<tilo.ActionAnswer[]>;
  addEffect(args: {
    clip: string;
    effect: EffectKind;
  }): Promise<void>;
  /** Runs `add-effect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  addEffectMany(items: AddEffectArgs[]): Promise<tilo.ActionAnswer[]>;
  removeEffect(args: {
    clip: string;
    effect: string;
  }): Promise<void>;
  /** Runs `remove-effect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  removeEffectMany(items: RemoveEffectArgs[]): Promise<tilo.ActionAnswer[]>;
  moveEffect(args: {
    clip: string;
    effect: string;
    up: boolean;
  }): Promise<void>;
  /** Runs `move-effect` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  moveEffectMany(items: MoveEffectArgs[]): Promise<tilo.ActionAnswer[]>;
  setEffectAmount(args: {
    clip: string;
    effect: string;
    amount: number;
  }): Promise<void>;
  /** Runs `set-effect-amount` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  setEffectAmountMany(items: SetEffectAmountArgs[]): Promise<tilo.ActionAnswer[]>;
  clearEffects(args: {
    clip: string;
  }): Promise<void>;
  /** Runs `clear-effects` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clearEffectsMany(items: ClearEffectsArgs[]): Promise<tilo.ActionAnswer[]>;
  unlink(args: {
    clip: string;
  }): Promise<void>;
  /** Runs `unlink` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  unlinkMany(items: UnlinkArgs[]): Promise<tilo.ActionAnswer[]>;
  linkAudio(args: {
    clip: string;
  }): Promise<void>;
  /** Runs `link-audio` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  linkAudioMany(items: LinkAudioArgs[]): Promise<tilo.ActionAnswer[]>;
  linkWith(args: {
    clip: string;
    other: string;
  }): Promise<void>;
  /** Runs `link-with` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  linkWithMany(items: LinkWithArgs[]): Promise<tilo.ActionAnswer[]>;
  exportTimelineContent(): Promise<void>;
  renderTimeline(args?: {
    start?: number;
    end?: number;
    format?: string;
    size?: string;
  }): Promise<void>;
  exportVideoContent(args?: {
    format?: string;
    width?: number;
    height?: number;
    fps?: number;
    wait?: number;
  }): Promise<void>;
  exportVideoStatus(args: {
    job: string;
    wait?: number;
  }): Promise<void>;
  cancelVideoExport(args: {
    job: string;
  }): Promise<void>;
  viewFrameContent(args: {
    at: number;
    width?: number;
  }): Promise<void>;
}

/** The actions `Project` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PROJECT_ACTIONS = ["setFrameSize", "setFrameSizeMany", "setFrameRate", "setFrameRateMany", "setRenderRange", "setRenderRangeMany", "markIn", "markInMany", "markOut", "markOutMany", "clearMarks", "clearMarksMany", "addTitle", "setTitle", "addClip", "moveClip", "setClip", "removeClip", "addMarker", "addMarkerMany", "placeTwoAngles", "placeTwoAnglesMany", "unplaceTwoAngles", "unplaceTwoAnglesMany", "removeMarker", "removeMarkerMany", "addTrack", "addTrackMany", "resetMix", "resetMixMany", "trimTracks", "trimTracksMany", "deleteTrack", "deleteTrackMany", "placeClip", "placeClipMany", "deleteMedia", "deleteMediaMany", "useAsProjectFormat", "useAsProjectFormatMany", "delete", "deleteMany", "rippleDelete", "rippleDeleteMany", "splitClip", "splitClipMany", "trimInClip", "trimInClipMany", "trimOutClip", "trimOutClipMany", "setSpeed", "setSpeedMany", "toggleReverse", "toggleReverseMany", "resetSpeed", "resetSpeedMany", "setParam", "setParamMany", "addEffect", "addEffectMany", "removeEffect", "removeEffectMany", "moveEffect", "moveEffectMany", "setEffectAmount", "setEffectAmountMany", "clearEffects", "clearEffectsMany", "unlink", "unlinkMany", "linkAudio", "linkAudioMany", "linkWith", "linkWithMany", "exportTimelineContent", "renderTimeline", "exportVideoContent", "exportVideoStatus", "cancelVideoExport", "viewFrameContent"] as const;

/** The `video-editor.project` kind at `path`. */
export function project(ws: tilo.Workspace, path: string): Project {
  return resourceProxy(ws, path, SURFACE, PROJECT_ACTIONS) as unknown as Project;
}

