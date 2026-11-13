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
export const SURFACE = "so.tilo.video-editor@c1e632de27d6ebcf";

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
  effects: ClipEffect[];
  transform: Transform;
  speed: Speed;
  key: ChromaKey;
  mask: Mask;
  anim: unknown;
  markers: Marker[];
  link: string | null;
  sound_split: boolean;
  link_follows: boolean;
}

export interface ClipEffect {
  id: string;
  kind: EffectKind;
  amount: number;
}

/** One of "grayscale", "sepia", "invert", "warm", "cool", "vignette". */
export type EffectKind = "grayscale" | "sepia" | "invert" | "warm" | "cool" | "vignette";

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

/** One of "none", "rect", "ellipse". */
export type MaskShape = "none" | "rect" | "ellipse";

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

/** One of "video", "audio", "image", "text". */
export type MediaKind = "video" | "audio" | "image" | "text";

export interface Speed {
  rate: number;
  reverse: boolean;
}

/** One of "left", "center", "right". */
export type TextAlign = "left" | "center" | "right";

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

/** One of "none", "cross_dissolve", "fade_in", "dip_to_black", "wipe". */
export type Transition = "none" | "cross_dissolve" | "fade_in" | "dip_to_black" | "wipe";

/** Project — `video-editor.project`. */
export interface Project extends tilo.Resource {
  name: string;
  fps: number;
  width: number;
  height: number;
  format_explicit: boolean;
  mark_in: number | null;
  mark_out: number | null;
  media: MediaItem[];
  tracks: Track[];
  clips: Clip[];
  markers: Marker[];
  setFrameSize(args: {
    width: number;
    height: number;
  }): Promise<tilo.Run | void>;
  setFrameRate(args: {
    fps: number;
  }): Promise<tilo.Run | void>;
  setRenderRange(args: {
    start: number;
    end: number;
  }): Promise<tilo.Run | void>;
  markIn(args: {
    at: number;
  }): Promise<tilo.Run | void>;
  markOut(args: {
    at: number;
  }): Promise<tilo.Run | void>;
  clearMarks(): Promise<tilo.Run | void>;
  addTitle(args: {
    text: string;
    colour?: string;
    size?: number;
    align?: string;
  }): Promise<unknown | tilo.Run>;
  setTitle(args: {
    media: string;
    text?: string;
    colour?: string;
    size?: number;
    align?: string;
  }): Promise<unknown | tilo.Run>;
  addClip(args: {
    media: string;
    track?: string;
    at?: number;
    in?: number;
    out?: number;
    fade_in?: number;
    fade_out?: number;
  }): Promise<unknown | tilo.Run>;
  moveClip(args: {
    clip: string;
    at: number;
    track?: string;
  }): Promise<unknown | tilo.Run>;
  setClip(args: {
    clip: string;
    fade_in?: number;
    fade_out?: number;
    opacity?: number;
    volume?: number;
    transition?: string;
    transition_duration?: number;
  }): Promise<unknown | tilo.Run>;
  removeClip(args: {
    clip: string;
    ripple?: boolean;
  }): Promise<unknown | tilo.Run>;
  addMarker(args: {
    at: number;
    name: string;
  }): Promise<tilo.Run | void>;
  placeTwoAngles(args: {
    at: number;
  }): Promise<tilo.Run | void>;
  unplaceTwoAngles(args: {
    at: number;
  }): Promise<tilo.Run | void>;
  removeMarker(args: {
    marker: string;
  }): Promise<tilo.Run | void>;
  addTrack(args: {
    kind: TrackKind;
  }): Promise<tilo.Run | void>;
  resetMix(args: {
    track: string;
  }): Promise<tilo.Run | void>;
  trimLanes(args: {
    tracks: string[];
    db: number;
    headroom: number;
  }): Promise<tilo.Run | void>;
  deleteTrack(args: {
    track: string;
  }): Promise<tilo.Run | void>;
  placeClip(args: {
    media: string;
    track: string;
    start: number;
  }): Promise<tilo.Run | void>;
  deleteMedia(args: {
    media: string;
  }): Promise<tilo.Run | void>;
  useAsProjectFormat(args: {
    media: string;
  }): Promise<tilo.Run | void>;
  delete(args: {
    clip: string;
  }): Promise<tilo.Run | void>;
  rippleDelete(args: {
    clip: string;
  }): Promise<tilo.Run | void>;
  splitClip(args: {
    clip: string;
    at: number;
  }): Promise<tilo.Run | void>;
  trimInClip(args: {
    clip: string;
    at: number;
  }): Promise<tilo.Run | void>;
  trimOutClip(args: {
    clip: string;
    at: number;
  }): Promise<tilo.Run | void>;
  setSpeed(args: {
    clip: string;
    rate: number;
  }): Promise<tilo.Run | void>;
  toggleReverse(args: {
    clip: string;
  }): Promise<tilo.Run | void>;
  resetSpeed(args: {
    clip: string;
  }): Promise<tilo.Run | void>;
  setParam(args: {
    clip: string;
    param: string;
    at: number;
    value: number;
  }): Promise<tilo.Run | void>;
  addEffect(args: {
    clip: string;
    effect: EffectKind;
  }): Promise<tilo.Run | void>;
  removeEffect(args: {
    clip: string;
    effect: string;
  }): Promise<tilo.Run | void>;
  moveEffect(args: {
    clip: string;
    effect: string;
    up: boolean;
  }): Promise<tilo.Run | void>;
  setEffectAmount(args: {
    clip: string;
    effect: string;
    amount: number;
  }): Promise<tilo.Run | void>;
  clearEffects(args: {
    clip: string;
  }): Promise<tilo.Run | void>;
  unlink(args: {
    clip: string;
  }): Promise<tilo.Run | void>;
  linkAudio(args: {
    clip: string;
  }): Promise<tilo.Run | void>;
  linkWith(args: {
    clip: string;
    other: string;
  }): Promise<tilo.Run | void>;
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

/** The `video-editor.project` kind at `path`. */
export function project(ws: tilo.Workspace, path: string): Project {
  return resourceProxy(ws, path, SURFACE) as unknown as Project;
}

