/**
 * Types for Camera (`so.tilo.camera`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`camera/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.camera@0cd26b372352ad7e";

/** A capture is a still picture or a clip. One of "photo", "video". */
export type CaptureKind = "photo" | "video";

/** Which lens the viewfinder shows: the rear one, one facing the person, or the wide one. One of "back", "front", "wide". */
export type Facing = "back" | "front" | "wide";

/** The arguments of `register_capture`. */
export interface RegisterCaptureArgs {
  capture: SharedCapture;
}

/** The arguments of `record_scan`. */
export interface RecordScanArgs {
  raw: string;
}

/** The arguments of `set_grid`. */
export interface SetGridArgs {
  on: boolean;
}

/** The arguments of `set_level`. */
export interface SetLevelArgs {
  on: boolean;
}

/** The arguments of `use_mode`. */
export interface UseModeArgs {
  mode: string;
}

/** The arguments of `use_lens`. */
export interface UseLensArgs {
  lens: Facing;
}

/** The arguments of `cycle_lens`. */
export interface CycleLensArgs {
}

/** The arguments of `use_camera`. */
export interface UseCameraArgs {
  device: string;
}

/** The arguments of `retry_camera`. */
export interface RetryCameraArgs {
}

/** The arguments of `list_cameras`. */
export interface ListCamerasArgs {
}

/** The arguments of `set_resolution`. */
export interface SetResolutionArgs {
  resolution: string;
}

/** The arguments of `set_photo_format`. */
export interface SetPhotoFormatArgs {
  format: string;
}

/** The arguments of `set_timer`. */
export interface SetTimerArgs {
  seconds: number;
}

/** The arguments of `set_mirror_front`. */
export interface SetMirrorFrontArgs {
  on: boolean;
}

/** The arguments of `set_geotag`. */
export interface SetGeotagArgs {
  on: boolean;
}

/** The arguments of `set_hdr`. */
export interface SetHdrArgs {
  on: boolean;
}

/** The arguments of `set_default_lens`. */
export interface SetDefaultLensArgs {
  lens: Facing;
}

/** The arguments of `start_look`. */
export interface StartLookArgs {
}

/** The arguments of `stop_look`. */
export interface StopLookArgs {
}

/** The arguments of `look`. */
export interface LookArgs {
}

/** The arguments of `read_codes`. */
export interface ReadCodesArgs {
  url: string;
}

/** The arguments of `shutter`. */
export interface ShutterArgs {
}

/** The arguments of `arm_timer`. */
export interface ArmTimerArgs {
}

/** The arguments of `capture`. */
export interface CaptureArgs {
}

/** The arguments of `start_recording`. */
export interface StartRecordingArgs {
}

/** The arguments of `stop_recording`. */
export interface StopRecordingArgs {
}

/** The arguments of `poll`. */
export interface PollArgs {
}

/** The arguments of `shutter_due`. */
export interface ShutterDueArgs {
}

/** The arguments of `import_image`. */
export interface ImportImageArgs {
  file: unknown;
}

/** The arguments of `keep_scan`. */
export interface KeepScanArgs {
}

/** The arguments of `set_scanner`. */
export interface SetScannerArgs {
  device: string;
}

/** The arguments of `set_scan_resolution`. */
export interface SetScanResolutionArgs {
  resolution: number;
}

/** The arguments of `set_scan_colour`. */
export interface SetScanColourArgs {
  colour: string;
}

/** The arguments of `set_scan_source`. */
export interface SetScanSourceArgs {
  source: string;
}

/** The arguments of `set_ocr_language`. */
export interface SetOcrLanguageArgs {
  language: string;
}

/** The arguments of `capture_page`. */
export interface CapturePageArgs {
}

/** The arguments of `select_page`. */
export interface SelectPageArgs {
  page: ScannedPage;
}

/** The arguments of `rotate_page`. */
export interface RotatePageArgs {
}

/** The arguments of `recognize_text`. */
export interface RecognizeTextArgs {
}

/** The arguments of `move_page`. */
export interface MovePageArgs {
  later: boolean;
}

/** The arguments of `remove_page`. */
export interface RemovePageArgs {
}

/** The arguments of `crop_page`. */
export interface CropPageArgs {
  left: number;
  top: number;
  right: number;
  bottom: number;
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

/** ScanEntry — `tl.so.tilo.camera.ScanEntry`. */
export interface ScanEntry extends tilo.Resource {
  hsfv5zso: string;  // raw
  ep2nakxy: string;  // ts
}

/** The actions `ScanEntry` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCANENTRY_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.ScanEntry` kind at `path`. */
export function scanEntry(ws: tilo.Workspace, path: string): ScanEntry {
  return resourceProxy(ws, path, SURFACE, SCANENTRY_ACTIONS) as unknown as ScanEntry;
}

/** SharedCapture — `tl.so.tilo.camera.SharedCapture`. */
export interface SharedCapture extends tilo.Resource {
  "3w536lga": unknown;  // file
  "2h53klqs": CaptureKind;  // kind
  "7enc2ly3": string;  // ts
  dwoxfhf4: number;  // w
  wwonv6b5: number;  // h
  cchj7ycb: number;  // dur_ms
  ls5awfet: Facing;  // facing
  "4xq6x7ig": string;  // label
}

/** The actions `SharedCapture` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SHAREDCAPTURE_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.SharedCapture` kind at `path`. */
export function sharedCapture(ws: tilo.Workspace, path: string): SharedCapture {
  return resourceProxy(ws, path, SURFACE, SHAREDCAPTURE_ACTIONS) as unknown as SharedCapture;
}

/** Wifi — `tl.so.tilo.camera.Wifi`. */
export interface Wifi extends tilo.Resource {
  lzomebum: string;  // ssid
  r4cfx2eg: string;  // auth
  vjej4gya: string;  // password
  cpvlblyf: boolean;  // hidden
}

/** The actions `Wifi` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WIFI_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.Wifi` kind at `path`. */
export function wifi(ws: tilo.Workspace, path: string): Wifi {
  return resourceProxy(ws, path, SURFACE, WIFI_ACTIONS) as unknown as Wifi;
}

/** Contact — `tl.so.tilo.camera.Contact`. */
export interface Contact extends tilo.Resource {
  vqb7vg46: string;  // name
  m3wjb2ux: string;  // phone
  ozre57rz: string;  // email
  "66g4od3j": string;  // org
}

/** The actions `Contact` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CONTACT_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.Contact` kind at `path`. */
export function contact(ws: tilo.Workspace, path: string): Contact {
  return resourceProxy(ws, path, SURFACE, CONTACT_ACTIONS) as unknown as Contact;
}

/** CapturePreferences — `tl.so.tilo.camera.CapturePreferences`. */
export interface CapturePreferences extends tilo.Resource {
  kbzf222p: string;  // resolution
  zqovr7j4: string;  // photo_format
  qv6u7j3k: number;  // timer
  "7htbqsa6": boolean;  // grid
  xpbujo45: boolean;  // level
  ly3zxilj: boolean;  // mirror_front
  b4n265en: boolean;  // geotag
  uucfkei5: boolean;  // hdr
  nfm4slrd: Facing;  // default_lens
  cfihwt6m: string;  // mode
  v5w2qmvn: Facing;  // lens
  zaeshfry: string;  // device
  zuizuamu: CameraDevice[];  // cameras
  bckgp4bo: boolean;  // cameras_prompt
  ffavn4zg: string;  // preview
  b4frgf3q: string;  // stream
  s45visew: string;  // found
  v7ncmrtp: number;  // found_x
  frttwlrr: number;  // found_y
  di5cyk72: number;  // found_w
  "3adwwuks": number;  // found_h
  fnu3wna2: string;  // recording
  z6lzljr2: string | null;  // recording_since
  bkfl557v: string | null;  // shutter_at
  zu2gt2mk: string;  // trouble
}

/** The actions `CapturePreferences` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CAPTUREPREFERENCES_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.CapturePreferences` kind at `path`. */
export function capturePreferences(ws: tilo.Workspace, path: string): CapturePreferences {
  return resourceProxy(ws, path, SURFACE, CAPTUREPREFERENCES_ACTIONS) as unknown as CapturePreferences;
}

/** ScannedPage — `tl.so.tilo.camera.ScannedPage`. */
export interface ScannedPage extends tilo.Resource {
  "5tzdwbr2": unknown;  // file
  noe6wvhn: string;  // text
  a6zvslgh: number;  // order
}

/** The actions `ScannedPage` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCANNEDPAGE_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.ScannedPage` kind at `path`. */
export function scannedPage(ws: tilo.Workspace, path: string): ScannedPage {
  return resourceProxy(ws, path, SURFACE, SCANNEDPAGE_ACTIONS) as unknown as ScannedPage;
}

/** ScanSession — `tl.so.tilo.camera.ScanSession`. */
export interface ScanSession extends tilo.Resource {
  r3wtymsq: string;  // scanner
  "7wujh6qk": number;  // resolution
  mmi6goha: string;  // colour
  jmjue7md: string;  // source
  auvo6snv: string;  // language
  pgl6ysyg: Array<tilo.Entry<ScannedPage>>;  // pages
  "2yr4wc6x": string;  // job
  "7mup54lj": string;  // failed
  yn4bzy6x: number;  // selected
}

/** The actions `ScanSession` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCANSESSION_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.ScanSession` kind at `path`. */
export function scanSession(ws: tilo.Workspace, path: string): ScanSession {
  return resourceProxy(ws, path, SURFACE, SCANSESSION_ACTIONS) as unknown as ScanSession;
}

/** Camera — `tl.so.tilo.camera.Camera`. */
export interface Camera extends tilo.Resource {
  mbpsgka6: Array<tilo.Entry<ScanEntry>>;  // scans
  rx3qy2lc: SharedCapture | null;  // recent
  vdbaklqk: Array<tilo.Entry<CapturePreferences>>;  // preferences
  "5q2zbyut": Array<tilo.Entry<ScanSession>>;  // sessions
  /** Takes what the engine kept as the capture the app hands on. */
  register_capture(args: {
    capture: SharedCapture;
  }): Promise<void>;
  /** Runs `register_capture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  register_captureMany(items: RegisterCaptureArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps a scanned code in the history; the same code seen again moves to the front rather than repeating. */
  record_scan(args: {
    raw: string;
  }): Promise<void>;
  /** Runs `record_scan` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  record_scanMany(items: RecordScanArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the composition grid, or takes it away. */
  set_grid(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_grid` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_gridMany(items: SetGridArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the level guide, or takes it away. */
  set_level(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_level` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_levelMany(items: SetLevelArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The person's preferences, made the first time they are written. */
  prefs_of(): Promise<CapturePreferences>;
  /** The person's document, made the first time it is written. */
  session_of(): Promise<ScanSession>;
  /** Opens the viewfinder. Opens the code scanner. Opens the document scanner. Opens the capture settings. Switches the viewfinder between photos and clips. */
  use_mode(args: {
    mode: string;
  }): Promise<void>;
  /** Runs `use_mode` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  use_modeMany(items: UseModeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows one lens. */
  use_lens(args: {
    lens: Facing;
  }): Promise<void>;
  /** Runs `use_lens` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  use_lensMany(items: UseLensArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Switches to the next lens. */
  cycle_lens(): Promise<void>;
  /** Runs `cycle_lens` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  cycle_lensMany(items: CycleLensArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Uses the camera at this device node. */
  use_camera(args: {
    device: string;
  }): Promise<void>;
  /** Runs `use_camera` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  use_cameraMany(items: UseCameraArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Forgets the chosen camera and asks this device for its cameras again. */
  retry_camera(): Promise<void>;
  /** Runs `retry_camera` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  retry_cameraMany(items: RetryCameraArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks this device which cameras it has: the rows the settings page offers, one per camera. */
  list_cameras(): Promise<void>;
  /** Runs `list_cameras` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  list_camerasMany(items: ListCamerasArgs[]): Promise<tilo.ActionAnswer[]>;
  set_resolution(args: {
    resolution: string;
  }): Promise<void>;
  /** Runs `set_resolution` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_resolutionMany(items: SetResolutionArgs[]): Promise<tilo.ActionAnswer[]>;
  set_photo_format(args: {
    format: string;
  }): Promise<void>;
  /** Runs `set_photo_format` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_photo_formatMany(items: SetPhotoFormatArgs[]): Promise<tilo.ActionAnswer[]>;
  set_timer(args: {
    seconds: number;
  }): Promise<void>;
  /** Runs `set_timer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_timerMany(items: SetTimerArgs[]): Promise<tilo.ActionAnswer[]>;
  set_mirror_front(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_mirror_front` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_mirror_frontMany(items: SetMirrorFrontArgs[]): Promise<tilo.ActionAnswer[]>;
  set_geotag(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_geotag` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_geotagMany(items: SetGeotagArgs[]): Promise<tilo.ActionAnswer[]>;
  set_hdr(args: {
    on: boolean;
  }): Promise<void>;
  /** Runs `set_hdr` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_hdrMany(items: SetHdrArgs[]): Promise<tilo.ActionAnswer[]>;
  set_default_lens(args: {
    lens: Facing;
  }): Promise<void>;
  /** Runs `set_default_lens` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_default_lensMany(items: SetDefaultLensArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts the camera's moving picture: the window paints the newest frame as it arrives. */
  start_look(): Promise<void>;
  /** Runs `start_look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  start_lookMany(items: StartLookArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops the camera's moving picture, letting the device go. */
  stop_look(): Promise<void>;
  /** Runs `stop_look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stop_lookMany(items: StopLookArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the codes the newest frame holds, keeping the first one the scanner view shows. */
  look(): Promise<void>;
  /** Runs `look` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  lookMany(items: LookArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Decodes the codes in the picture at `url`, keeping the first one the scanner view shows. */
  read_codes(args: {
    url: string;
  }): Promise<void>;
  /** Runs `read_codes` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_codesMany(items: ReadCodesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The shutter: takes a photo now, starts the self-timer, or records. Space presses it, as a camera's shutter button is pressed. */
  shutter(): Promise<void>;
  /** Runs `shutter` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  shutterMany(items: ShutterArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Arms the self-timer the person chose, or cancels the one counting down. */
  arm_timer(): Promise<void>;
  /** Runs `arm_timer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  arm_timerMany(items: ArmTimerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a photo of the frame the camera shows, and hands it on. */
  capture(): Promise<void>;
  /** Runs `capture` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  captureMany(items: CaptureArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts recording from the frames the camera's engine thread takes. */
  start_recording(): Promise<void>;
  /** Runs `start_recording` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  start_recordingMany(items: StartRecordingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops the clip recording and hands it on once the engine has muxed it. */
  stop_recording(): Promise<void>;
  /** Runs `stop_recording` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stop_recordingMany(items: StopRecordingArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Asks how far the recording and the document scan have got, finishing what is done. */
  poll(): Promise<void>;
  /** Runs `poll` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pollMany(items: PollArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The self-timer's moment came: take the photo it counted down to, unless it was cancelled. */
  shutter_due(): Promise<void>;
  /** Runs `shutter_due` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  shutter_dueMany(items: ShutterDueArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes a chosen image as the code to read, without importing it. */
  import_image(args: {
    file: unknown;
  }): Promise<void>;
  /** Runs `import_image` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  import_imageMany(items: ImportImageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies what the scanned code says, or its password or link. */
  copy_code(args: {
    which: string;
  }): Promise<void>;
  /** Keeps the scanned code in the history. */
  keep_scan(): Promise<void>;
  /** Runs `keep_scan` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_scanMany(items: KeepScanArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Chooses the scanner the document scans with. */
  set_scanner(args: {
    device: string;
  }): Promise<void>;
  /** Runs `set_scanner` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_scannerMany(items: SetScannerArgs[]): Promise<tilo.ActionAnswer[]>;
  set_scan_resolution(args: {
    resolution: number;
  }): Promise<void>;
  /** Runs `set_scan_resolution` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_scan_resolutionMany(items: SetScanResolutionArgs[]): Promise<tilo.ActionAnswer[]>;
  set_scan_colour(args: {
    colour: string;
  }): Promise<void>;
  /** Runs `set_scan_colour` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_scan_colourMany(items: SetScanColourArgs[]): Promise<tilo.ActionAnswer[]>;
  set_scan_source(args: {
    source: string;
  }): Promise<void>;
  /** Runs `set_scan_source` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_scan_sourceMany(items: SetScanSourceArgs[]): Promise<tilo.ActionAnswer[]>;
  set_ocr_language(args: {
    language: string;
  }): Promise<void>;
  /** Runs `set_ocr_language` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_ocr_languageMany(items: SetOcrLanguageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Captures one page with the chosen scanner and settings. */
  capture_page(): Promise<void>;
  /** Runs `capture_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  capture_pageMany(items: CapturePageArgs[]): Promise<tilo.ActionAnswer[]>;
  select_page(args: {
    page: ScannedPage;
  }): Promise<void>;
  /** Runs `select_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  select_pageMany(items: SelectPageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns the selected page a quarter clockwise; its text is read again when the person asks. */
  rotate_page(): Promise<void>;
  /** Runs `rotate_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rotate_pageMany(items: RotatePageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads the text off the selected page in the chosen language. */
  recognize_text(): Promise<void>;
  /** Runs `recognize_text` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recognize_textMany(items: RecognizeTextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Copies the selected page's recognized text. */
  copy_text(): Promise<void>;
  /** Moves the selected page one place earlier, or later. */
  move_page(args: {
    later: boolean;
  }): Promise<void>;
  /** Runs `move_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_pageMany(items: MovePageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Throws the selected page away. */
  remove_page(): Promise<void>;
  /** Runs `remove_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_pageMany(items: RemovePageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The selected page's picture, as the crop frame works on it. */
  crop_source(): Promise<unknown | null>;
  /** Crops the selected page to the part the person kept; its text is read again when they ask. */
  crop_page(args: {
    left: number;
    top: number;
    right: number;
    bottom: number;
  }): Promise<void>;
  /** Runs `crop_page` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  crop_pageMany(items: CropPageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** The document as one searchable PDF: pages with recognized text carry it, the rest are pictures alone. */
  document_pdf(): Promise<unknown>;
  /** Opens the capture that was taken last in the app that owns its bytes. */
  open_recent(): Promise<void>;
}

/** The actions `Camera` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CAMERA_ACTIONS = ["register_capture", "register_captureMany", "record_scan", "record_scanMany", "set_grid", "set_gridMany", "set_level", "set_levelMany", "prefs_of", "session_of", "use_mode", "use_modeMany", "use_lens", "use_lensMany", "cycle_lens", "cycle_lensMany", "use_camera", "use_cameraMany", "retry_camera", "retry_cameraMany", "list_cameras", "list_camerasMany", "set_resolution", "set_resolutionMany", "set_photo_format", "set_photo_formatMany", "set_timer", "set_timerMany", "set_mirror_front", "set_mirror_frontMany", "set_geotag", "set_geotagMany", "set_hdr", "set_hdrMany", "set_default_lens", "set_default_lensMany", "start_look", "start_lookMany", "stop_look", "stop_lookMany", "look", "lookMany", "read_codes", "read_codesMany", "shutter", "shutterMany", "arm_timer", "arm_timerMany", "capture", "captureMany", "start_recording", "start_recordingMany", "stop_recording", "stop_recordingMany", "poll", "pollMany", "shutter_due", "shutter_dueMany", "import_image", "import_imageMany", "copy_code", "keep_scan", "keep_scanMany", "set_scanner", "set_scannerMany", "set_scan_resolution", "set_scan_resolutionMany", "set_scan_colour", "set_scan_colourMany", "set_scan_source", "set_scan_sourceMany", "set_ocr_language", "set_ocr_languageMany", "capture_page", "capture_pageMany", "select_page", "select_pageMany", "rotate_page", "rotate_pageMany", "recognize_text", "recognize_textMany", "copy_text", "move_page", "move_pageMany", "remove_page", "remove_pageMany", "crop_source", "crop_page", "crop_pageMany", "document_pdf", "open_recent"] as const;

/** The `tl.so.tilo.camera.Camera` kind at `path`. */
export function camera(ws: tilo.Workspace, path: string): Camera {
  return resourceProxy(ws, path, SURFACE, CAMERA_ACTIONS) as unknown as Camera;
}

/** StillAsk — `tl.so.tilo.camera.StillAsk`. */
export interface StillAsk extends tilo.Resource {
  tbd3ic3b: string;  // device
  ijab2zhm: number;  // width
  zyjznh7y: number;  // height
  w2ygpawb: string;  // format
}

/** The actions `StillAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STILLASK_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.StillAsk` kind at `path`. */
export function stillAsk(ws: tilo.Workspace, path: string): StillAsk {
  return resourceProxy(ws, path, SURFACE, STILLASK_ACTIONS) as unknown as StillAsk;
}

/** FrameAsk — `tl.so.tilo.camera.FrameAsk`. */
export interface FrameAsk extends tilo.Resource {
  ycmsih2o: string;  // device
  yvbln5ns: number;  // width
  kzl4t5dn: number;  // height
}

/** The actions `FrameAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FRAMEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.FrameAsk` kind at `path`. */
export function frameAsk(ws: tilo.Workspace, path: string): FrameAsk {
  return resourceProxy(ws, path, SURFACE, FRAMEASK_ACTIONS) as unknown as FrameAsk;
}

/** CodesAsk — `tl.so.tilo.camera.CodesAsk`. */
export interface CodesAsk extends tilo.Resource {
  vzfp47qk: string;  // url
  v7y3lgcv: number;  // width
}

/** The actions `CodesAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CODESASK_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.CodesAsk` kind at `path`. */
export function codesAsk(ws: tilo.Workspace, path: string): CodesAsk {
  return resourceProxy(ws, path, SURFACE, CODESASK_ACTIONS) as unknown as CodesAsk;
}

/** DecodedCode — `tl.so.tilo.camera.DecodedCode`. */
export interface DecodedCode extends tilo.Resource {
  fzwv4d4j: string;  // text
  lsk2z7xu: number;  // x
  ukafx4ho: number;  // y
  hqwha6oq: number;  // w
  mc2k2zbw: number;  // h
}

/** The actions `DecodedCode` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DECODEDCODE_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.DecodedCode` kind at `path`. */
export function decodedCode(ws: tilo.Workspace, path: string): DecodedCode {
  return resourceProxy(ws, path, SURFACE, DECODEDCODE_ACTIONS) as unknown as DecodedCode;
}

/** CodesAnswer — `tl.so.tilo.camera.CodesAnswer`. */
export interface CodesAnswer extends tilo.Resource {
  etfbstke: DecodedCode[];  // codes
}

/** The actions `CodesAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CODESANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.CodesAnswer` kind at `path`. */
export function codesAnswer(ws: tilo.Workspace, path: string): CodesAnswer {
  return resourceProxy(ws, path, SURFACE, CODESANSWER_ACTIONS) as unknown as CodesAnswer;
}

/** CameraDevice — `tl.so.tilo.camera.CameraDevice`. */
export interface CameraDevice extends tilo.Resource {
  e6k5e5oe: string;  // device
  umm5rpj4: string;  // label
}

/** The actions `CameraDevice` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CAMERADEVICE_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.CameraDevice` kind at `path`. */
export function cameraDevice(ws: tilo.Workspace, path: string): CameraDevice {
  return resourceProxy(ws, path, SURFACE, CAMERADEVICE_ACTIONS) as unknown as CameraDevice;
}

/** CamerasAnswer — `tl.so.tilo.camera.CamerasAnswer`. */
export interface CamerasAnswer extends tilo.Resource {
  "3sxueu6f": CameraDevice[];  // devices
  rnqfpsmo: boolean;  // prompt
}

/** The actions `CamerasAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CAMERASANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.CamerasAnswer` kind at `path`. */
export function camerasAnswer(ws: tilo.Workspace, path: string): CamerasAnswer {
  return resourceProxy(ws, path, SURFACE, CAMERASANSWER_ACTIONS) as unknown as CamerasAnswer;
}

/** JobAnswer — `tl.so.tilo.camera.JobAnswer`. */
export interface JobAnswer extends tilo.Resource {
  "3uzmfe3a": string;  // job
}

/** The actions `JobAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const JOBANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.JobAnswer` kind at `path`. */
export function jobAnswer(ws: tilo.Workspace, path: string): JobAnswer {
  return resourceProxy(ws, path, SURFACE, JOBANSWER_ACTIONS) as unknown as JobAnswer;
}

/** JobState — `tl.so.tilo.camera.JobState`. */
export interface JobState extends tilo.Resource {
  "5c2s6yqj": string;  // state
  b4mxjneo: string;  // url
  i7ypqcia: string;  // error
}

/** The actions `JobState` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const JOBSTATE_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.JobState` kind at `path`. */
export function jobState(ws: tilo.Workspace, path: string): JobState {
  return resourceProxy(ws, path, SURFACE, JOBSTATE_ACTIONS) as unknown as JobState;
}

/** RecordAsk — `tl.so.tilo.camera.RecordAsk`. */
export interface RecordAsk extends tilo.Resource {
  zm52cba6: string;  // device
  "2vmgzw6b": number;  // width
  "2oehkbmq": number;  // height
}

/** The actions `RecordAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RECORDASK_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.RecordAsk` kind at `path`. */
export function recordAsk(ws: tilo.Workspace, path: string): RecordAsk {
  return resourceProxy(ws, path, SURFACE, RECORDASK_ACTIONS) as unknown as RecordAsk;
}

/** RecordAskJob — `tl.so.tilo.camera.RecordAskJob`. */
export interface RecordAskJob extends tilo.Resource {
  vkx7nslm: string;  // job
}

/** The actions `RecordAskJob` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RECORDASKJOB_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.RecordAskJob` kind at `path`. */
export function recordAskJob(ws: tilo.Workspace, path: string): RecordAskJob {
  return resourceProxy(ws, path, SURFACE, RECORDASKJOB_ACTIONS) as unknown as RecordAskJob;
}

/** RecordStopped — `tl.so.tilo.camera.RecordStopped`. */
export interface RecordStopped extends tilo.Resource {
  tm6dvkh2: RecordedClip;  // video
}

/** The actions `RecordStopped` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RECORDSTOPPED_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.RecordStopped` kind at `path`. */
export function recordStopped(ws: tilo.Workspace, path: string): RecordStopped {
  return resourceProxy(ws, path, SURFACE, RECORDSTOPPED_ACTIONS) as unknown as RecordStopped;
}

/** RecordedClip — `tl.so.tilo.camera.RecordedClip`. */
export interface RecordedClip extends tilo.Resource {
  tzmimu3l: string;  // name
  "7znademx": number;  // duration_ms
  iwtkarql: number;  // w
  zmsyrjzf: number;  // h
  zgalivtz: number;  // size
}

/** The actions `RecordedClip` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RECORDEDCLIP_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.RecordedClip` kind at `path`. */
export function recordedClip(ws: tilo.Workspace, path: string): RecordedClip {
  return resourceProxy(ws, path, SURFACE, RECORDEDCLIP_ACTIONS) as unknown as RecordedClip;
}

/** ScanAsk — `tl.so.tilo.camera.ScanAsk`. */
export interface ScanAsk extends tilo.Resource {
  "3m6nff75": string;  // scanner
  o4hag5xq: number;  // resolution
  hvk7vnxj: string;  // colour
  "7zqczcaa": string;  // source
  sq2qwcuy: string;  // name
}

/** The actions `ScanAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCANASK_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.ScanAsk` kind at `path`. */
export function scanAsk(ws: tilo.Workspace, path: string): ScanAsk {
  return resourceProxy(ws, path, SURFACE, SCANASK_ACTIONS) as unknown as ScanAsk;
}

/** OcrAsk — `tl.so.tilo.camera.OcrAsk`. */
export interface OcrAsk extends tilo.Resource {
  "35d3ajgx": string;  // url
  tlhgxi3u: string;  // language
}

/** The actions `OcrAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OCRASK_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.OcrAsk` kind at `path`. */
export function ocrAsk(ws: tilo.Workspace, path: string): OcrAsk {
  return resourceProxy(ws, path, SURFACE, OCRASK_ACTIONS) as unknown as OcrAsk;
}

/** OcrAnswer — `tl.so.tilo.camera.OcrAnswer`. */
export interface OcrAnswer extends tilo.Resource {
  "3hu3iumn": string;  // text
  k2lbevvr: number;  // words
  nof45z64: number;  // deskew
}

/** The actions `OcrAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const OCRANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.OcrAnswer` kind at `path`. */
export function ocrAnswer(ws: tilo.Workspace, path: string): OcrAnswer {
  return resourceProxy(ws, path, SURFACE, OCRANSWER_ACTIONS) as unknown as OcrAnswer;
}

/** PdfPage — `tl.so.tilo.camera.PdfPage`. */
export interface PdfPage extends tilo.Resource {
  siogqny4: string;  // url
  anpoq2j7: string;  // text
}

/** The actions `PdfPage` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PDFPAGE_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.PdfPage` kind at `path`. */
export function pdfPage(ws: tilo.Workspace, path: string): PdfPage {
  return resourceProxy(ws, path, SURFACE, PDFPAGE_ACTIONS) as unknown as PdfPage;
}

/** PagesAsk — `tl.so.tilo.camera.PagesAsk`. */
export interface PagesAsk extends tilo.Resource {
  mmydep5v: PdfPage[];  // pages
  rclpsfrb: string;  // name
}

/** The actions `PagesAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PAGESASK_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.PagesAsk` kind at `path`. */
export function pagesAsk(ws: tilo.Workspace, path: string): PagesAsk {
  return resourceProxy(ws, path, SURFACE, PAGESASK_ACTIONS) as unknown as PagesAsk;
}

/** UrlAnswer — `tl.so.tilo.camera.UrlAnswer`. */
export interface UrlAnswer extends tilo.Resource {
  llffazxv: string;  // url
}

/** The actions `UrlAnswer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const URLANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.UrlAnswer` kind at `path`. */
export function urlAnswer(ws: tilo.Workspace, path: string): UrlAnswer {
  return resourceProxy(ws, path, SURFACE, URLANSWER_ACTIONS) as unknown as UrlAnswer;
}

/** Email — `tl.so.tilo.camera.Email`. */
export interface Email extends tilo.Resource {
  kdi6tquz: string;  // address
  odqiiw2m: string;  // subject
}

/** The actions `Email` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const EMAIL_ACTIONS = [] as const;

/** The `tl.so.tilo.camera.Email` kind at `path`. */
export function email(ws: tilo.Workspace, path: string): Email {
  return resourceProxy(ws, path, SURFACE, EMAIL_ACTIONS) as unknown as Email;
}

/** Camera — `tl.so.tilo.camera`. */
export interface TiloCamera extends tilo.Resource {
  /** Opens the library the captures go to. */
  open_photos(): Promise<void>;
  /** Opens this device's camera privacy settings. */
  open_camera_privacy(): Promise<void>;
}

/** The actions `TiloCamera` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOCAMERA_ACTIONS = ["open_photos", "open_camera_privacy"] as const;

/** The `tl.so.tilo.camera` kind at `path`. */
export function tiloCamera(ws: tilo.Workspace, path: string): TiloCamera {
  return resourceProxy(ws, path, SURFACE, TILOCAMERA_ACTIONS) as unknown as TiloCamera;
}

