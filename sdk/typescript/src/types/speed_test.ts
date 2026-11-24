/**
 * Types for Speed Test (`so.tilo.speed-test`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `SpeedTest` is a `Resource` of the app's `tl.so.tilo.speed-test.SpeedTest`, made by its factory with the
 * resource's path (`speed-test/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.speed-test@596ac912f06684e0";

/** The arguments of `start`. */
export interface StartArgs {
}

/** The arguments of `stop`. */
export interface StopArgs {
}

/** The arguments of `poll`. */
export interface PollArgs {
}

/** The arguments of `upload_stage`. */
export interface UploadStageArgs {
  run: Run;
}

/** The arguments of `record`. */
export interface RecordArgs {
  ping: number;
  jitter: number;
  download: number;
  upload: number;
  server: string;
  device: string;
}

/** The arguments of `clear`. */
export interface ClearArgs {
}

/** SpeedTest — `tl.so.tilo.speed-test.SpeedTest`. */
export interface SpeedTest extends tilo.Resource {
  hpwi5yye: Array<tilo.Entry<Measurement>>;  // results
  phw7nc7e: Array<tilo.Entry<Run>>;  // runs
  ckq5elpb: string | null;  // next_poll
  /** Starts a run: the window measures latency against the nearest Cloudflare test server, then the download and the upload on the seconds after, and shows the dial moving as it goes. */
  start(): Promise<void>;
  /** Runs `start` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  startMany(items: StartArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops the run in flight: the measurement stops and the dial goes back to ready. Nothing of a stopped run is kept. */
  stop(): Promise<void>;
  /** Runs `stop` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stopMany(items: StopArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Measures the next stage of the run on the page: the download, then the upload, which records what the run measured. A stage that fails ends the run with its words. */
  poll(): Promise<void>;
  /** Runs `poll` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pollMany(items: PollArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Uploads a run's bytes and records what the whole run measured. A refusal fails the stage. */
  upload_stage(args: {
    run: Run;
  }): Promise<void>;
  /** Runs `upload_stage` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  upload_stageMany(items: UploadStageArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Records a finished measurement, newest first, and lets the oldest go beyond what the workspace keeps: an agent or a script may record one too. */
  record(args: {
    ping: number;
    jitter: number;
    download: number;
    upload: number;
    server: string;
    device: string;
  }): Promise<void>;
  /** Runs `record` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  recordMany(items: RecordArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Forgets every measurement. */
  clear(): Promise<void>;
  /** Runs `clear` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clearMany(items: ClearArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `SpeedTest` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SPEEDTEST_ACTIONS = ["start", "startMany", "stop", "stopMany", "poll", "pollMany", "upload_stage", "upload_stageMany", "record", "recordMany", "clear", "clearMany"] as const;

/** The `tl.so.tilo.speed-test.SpeedTest` kind at `path`. */
export function speedTest(ws: tilo.Workspace, path: string): SpeedTest {
  return resourceProxy(ws, path, SURFACE, SPEEDTEST_ACTIONS) as unknown as SpeedTest;
}

/** Measurement — `tl.so.tilo.speed-test.Measurement`. */
export interface Measurement extends tilo.Resource {
  "5f54c4ou": number;  // ping
  q5jjysbc: number;  // jitter
  kded6ppq: number;  // download
  x6xgknqc: number;  // upload
  vcibj65j: string;  // server
  vbch5rhv: string;  // device
}

/** The actions `Measurement` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MEASUREMENT_ACTIONS = [] as const;

/** The `tl.so.tilo.speed-test.Measurement` kind at `path`. */
export function measurement(ws: tilo.Workspace, path: string): Measurement {
  return resourceProxy(ws, path, SURFACE, MEASUREMENT_ACTIONS) as unknown as Measurement;
}

/** Run — `tl.so.tilo.speed-test.Run`. */
export interface Run extends tilo.Resource {
  fbbyyume: string;  // phase
  vufvfoij: number;  // live
  stw2oyj6: number;  // bytes
  nwtvekek: string;  // server
  t3padrs4: number;  // ping
  vejad7bv: number;  // jitter
  "2kviwxr6": number;  // download
  wkhwlknc: string;  // error
  /** Times a few round trips and keeps the server the first one found. The dial shows the last reading while the run measures. */
  ping_stage(): Promise<boolean>;
  /** Downloads a run's bytes in chunks of a reply the language holds whole, timing them together, and keeps the rate they came at. */
  download_stage(): Promise<boolean>;
}

/** The actions `Run` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RUN_ACTIONS = ["ping_stage", "download_stage"] as const;

/** The `tl.so.tilo.speed-test.Run` kind at `path`. */
export function run(ws: tilo.Workspace, path: string): Run {
  return resourceProxy(ws, path, SURFACE, RUN_ACTIONS) as unknown as Run;
}

/** Speed Test — `tl.so.tilo.speed-test`. */
export interface TiloSpeedTest extends tilo.Resource {
}

/** The actions `TiloSpeedTest` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOSPEEDTEST_ACTIONS = [] as const;

/** The `tl.so.tilo.speed-test` kind at `path`. */
export function tiloSpeedTest(ws: tilo.Workspace, path: string): TiloSpeedTest {
  return resourceProxy(ws, path, SURFACE, TILOSPEEDTEST_ACTIONS) as unknown as TiloSpeedTest;
}

