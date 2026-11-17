/**
 * Types for Clocks (`so.tilo.clocks`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Collection` is a `Resource` of the app's `clocks.collection`, made by its factory with the
 * resource's path (`clocks/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.clocks@de9ae8048fcfe612";

export interface Alarm {
  label: string;
  hour: number;
  minute: number;
  repeat: number;
  enabled: boolean;
  enabled_at_ms: number;
}

export interface Stopwatch {
  accumulated_ms: number;
  started_at_ms: number | null;
  laps_ms: number[];
}

export interface Timer {
  label: string;
  duration_ms: number;
  ends_at_ms: number | null;
  paused_remaining_ms: number | null;
}

/** One of "Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun". */
export type WeekdayKey = "Mon" | "Tue" | "Wed" | "Thu" | "Fri" | "Sat" | "Sun";

export interface WorldClock {
  city: string;
  timezone: string;
}

/** The arguments of `add-world-clock`. */
export interface AddWorldClockArgs {
  city: string;
  timezone: string;
}

/** The arguments of `remove-world-clock`. */
export interface RemoveWorldClockArgs {
  world_clock: string;
}

/** The arguments of `add-alarm`. */
export interface AddAlarmArgs {
  label: string;
  hour: number;
  minute: number;
  weekdays: WeekdayKey[];
}

/** The arguments of `set-alarm-enabled`. */
export interface SetAlarmEnabledArgs {
  alarm: string;
  enabled: boolean;
}

/** The arguments of `edit-alarm`. */
export interface EditAlarmArgs {
  alarm: string;
  label: string;
  hour: number;
  minute: number;
  weekdays: WeekdayKey[];
}

/** The arguments of `remove-alarm`. */
export interface RemoveAlarmArgs {
  alarm: string;
}

/** The arguments of `add-timer`. */
export interface AddTimerArgs {
  label: string;
  seconds: number;
}

/** The arguments of `pause-timer`. */
export interface PauseTimerArgs {
  timer: string;
}

/** The arguments of `resume-timer`. */
export interface ResumeTimerArgs {
  timer: string;
}

/** The arguments of `add-minute`. */
export interface AddMinuteArgs {
  timer: string;
}

/** The arguments of `remove-timer`. */
export interface RemoveTimerArgs {
  timer: string;
}

/** The arguments of `start-stopwatch`. */
export interface StartStopwatchArgs {
}

/** The arguments of `pause-stopwatch`. */
export interface PauseStopwatchArgs {
}

/** The arguments of `lap`. */
export interface LapArgs {
}

/** The arguments of `reset-stopwatch`. */
export interface ResetStopwatchArgs {
}

/** The arguments of `keep`. */
export interface KeepArgs {
  device: string;
  kept: unknown;
}

/** Clocks — `clocks.collection`. */
export interface Collection extends tilo.Resource {
  world_clocks: Array<tilo.Entry<WorldClock>>;
  alarms: Array<tilo.Entry<Alarm>>;
  timers: Array<tilo.Entry<Timer>>;
  readonly stopwatch: Stopwatch;
  addWorldClock(args: {
    city: string;
    timezone: string;
  }): Promise<void>;
  /** Runs `add-world-clock` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addWorldClockMany(items: AddWorldClockArgs[]): Promise<tilo.ActionAnswer[]>;
  removeWorldClock(args: {
    world_clock: string;
  }): Promise<void>;
  /** Runs `remove-world-clock` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeWorldClockMany(items: RemoveWorldClockArgs[]): Promise<tilo.ActionAnswer[]>;
  addAlarm(args: {
    label: string;
    hour: number;
    minute: number;
    weekdays: WeekdayKey[];
  }): Promise<void>;
  /** Runs `add-alarm` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addAlarmMany(items: AddAlarmArgs[]): Promise<tilo.ActionAnswer[]>;
  setAlarmEnabled(args: {
    alarm: string;
    enabled: boolean;
  }): Promise<void>;
  /** Runs `set-alarm-enabled` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  setAlarmEnabledMany(items: SetAlarmEnabledArgs[]): Promise<tilo.ActionAnswer[]>;
  editAlarm(args: {
    alarm: string;
    label: string;
    hour: number;
    minute: number;
    weekdays: WeekdayKey[];
  }): Promise<void>;
  /** Runs `edit-alarm` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  editAlarmMany(items: EditAlarmArgs[]): Promise<tilo.ActionAnswer[]>;
  removeAlarm(args: {
    alarm: string;
  }): Promise<void>;
  /** Runs `remove-alarm` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeAlarmMany(items: RemoveAlarmArgs[]): Promise<tilo.ActionAnswer[]>;
  addTimer(args: {
    label: string;
    seconds: number;
  }): Promise<void>;
  /** Runs `add-timer` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addTimerMany(items: AddTimerArgs[]): Promise<tilo.ActionAnswer[]>;
  pauseTimer(args: {
    timer: string;
  }): Promise<void>;
  /** Runs `pause-timer` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  pauseTimerMany(items: PauseTimerArgs[]): Promise<tilo.ActionAnswer[]>;
  resumeTimer(args: {
    timer: string;
  }): Promise<void>;
  /** Runs `resume-timer` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  resumeTimerMany(items: ResumeTimerArgs[]): Promise<tilo.ActionAnswer[]>;
  addMinute(args: {
    timer: string;
  }): Promise<void>;
  /** Runs `add-minute` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  addMinuteMany(items: AddMinuteArgs[]): Promise<tilo.ActionAnswer[]>;
  removeTimer(args: {
    timer: string;
  }): Promise<void>;
  /** Runs `remove-timer` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  removeTimerMany(items: RemoveTimerArgs[]): Promise<tilo.ActionAnswer[]>;
  startStopwatch(): Promise<void>;
  /** Runs `start-stopwatch` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  startStopwatchMany(items: StartStopwatchArgs[]): Promise<tilo.ActionAnswer[]>;
  pauseStopwatch(): Promise<void>;
  /** Runs `pause-stopwatch` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  pauseStopwatchMany(items: PauseStopwatchArgs[]): Promise<tilo.ActionAnswer[]>;
  lap(): Promise<void>;
  /** Runs `lap` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  lapMany(items: LapArgs[]): Promise<tilo.ActionAnswer[]>;
  resetStopwatch(): Promise<void>;
  /** Runs `reset-stopwatch` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  resetStopwatchMany(items: ResetStopwatchArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Collection` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const COLLECTION_ACTIONS = ["addWorldClock", "addWorldClockMany", "removeWorldClock", "removeWorldClockMany", "addAlarm", "addAlarmMany", "setAlarmEnabled", "setAlarmEnabledMany", "editAlarm", "editAlarmMany", "removeAlarm", "removeAlarmMany", "addTimer", "addTimerMany", "pauseTimer", "pauseTimerMany", "resumeTimer", "resumeTimerMany", "addMinute", "addMinuteMany", "removeTimer", "removeTimerMany", "startStopwatch", "startStopwatchMany", "pauseStopwatch", "pauseStopwatchMany", "lap", "lapMany", "resetStopwatch", "resetStopwatchMany"] as const;

/** The `clocks.collection` kind at `path`. */
export function collection(ws: tilo.Workspace, path: string): Collection {
  return resourceProxy(ws, path, SURFACE, COLLECTION_ACTIONS) as unknown as Collection;
}

/** Clocks on each device — `clocks.on-devices`. */
export interface OnDevices extends tilo.Resource {
  devices: Record<string, unknown>;
  keep(args: {
    device: string;
    kept: unknown;
  }): Promise<void>;
  /** Runs `keep` for each of `items`: one all-or-nothing step per call, all of them one run that History lists once and one undo takes back. */
  keepMany(items: KeepArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `OnDevices` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ONDEVICES_ACTIONS = ["keep", "keepMany"] as const;

/** The `clocks.on-devices` kind at `path`. */
export function onDevices(ws: tilo.Workspace, path: string): OnDevices {
  return resourceProxy(ws, path, SURFACE, ONDEVICES_ACTIONS) as unknown as OnDevices;
}

