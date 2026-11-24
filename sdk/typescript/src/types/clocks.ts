/**
 * Types for Clocks (`so.tilo.clocks`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`clocks/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.clocks@860b782a67ee26a5";

/** A day of the week an alarm repeats on, Monday first. One of "mon", "tue", "wed", "thu", "fri", "sat", "sun". */
export type Weekday = "mon" | "tue" | "wed" | "thu" | "fri" | "sat" | "sun";

/** The arguments of `edit_alarm`. */
export interface EditAlarmArgs {
  label: string;
  hour: number;
  minute: number;
  days?: Weekday[];
}

/** The arguments of `set_alarm_enabled`. */
export interface SetAlarmEnabledArgs {
  enabled?: boolean;
}

/** The arguments of `set_alarm_device`. */
export interface SetAlarmDeviceArgs {
  rings_on: string;
}

/** The arguments of `toggle_day`. */
export interface ToggleDayArgs {
  d: Weekday;
}

/** The arguments of `snooze_alarm`. */
export interface SnoozeAlarmArgs {
}

/** The arguments of `ring_alarm`. */
export interface RingAlarmArgs {
}

/** The arguments of `pause_timer`. */
export interface PauseTimerArgs {
}

/** The arguments of `resume_timer`. */
export interface ResumeTimerArgs {
}

/** The arguments of `add_minute`. */
export interface AddMinuteArgs {
}

/** The arguments of `remove_world_clock`. */
export interface RemoveWorldClockArgs {
  w: WorldClock;
}

/** The arguments of `remove_alarm`. */
export interface RemoveAlarmArgs {
  a: Alarm;
}

/** The arguments of `remove_timer`. */
export interface RemoveTimerArgs {
  t: Timer;
}

/** The arguments of `start_stopwatch`. */
export interface StartStopwatchArgs {
}

/** The arguments of `pause_stopwatch`. */
export interface PauseStopwatchArgs {
}

/** The arguments of `lap`. */
export interface LapArgs {
}

/** The arguments of `reset_stopwatch`. */
export interface ResetStopwatchArgs {
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

/** WorldClock — `tl.so.tilo.clocks.WorldClock`. */
export interface WorldClock extends tilo.Resource {
  kbeao6ct: string;  // city
  g5vsvo2p: string;  // zone
}

/** The actions `WorldClock` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WORLDCLOCK_ACTIONS = [] as const;

/** The `tl.so.tilo.clocks.WorldClock` kind at `path`. */
export function worldClock(ws: tilo.Workspace, path: string): WorldClock {
  return resourceProxy(ws, path, SURFACE, WORLDCLOCK_ACTIONS) as unknown as WorldClock;
}

/** Alarm — `tl.so.tilo.clocks.Alarm`. */
export interface Alarm extends tilo.Resource {
  e6jhyy6h: string;  // label
  cb6boqqg: number;  // hour
  bvmw7lxl: number;  // minute
  s4ppnbtv: Weekday[];  // days
  adpv3jco: boolean;  // enabled
  fbxt6ruu: string | null;  // at
  "3bdkffqt": string;  // rings_on
  uizteftt: boolean;  // snoozed
  /** Changes an alarm's words, time and days. Its on/off state stays as it was, and one that is on rings next at the time it now names. */
  edit_alarm(args: {
    label: string;
    hour: number;
    minute: number;
    days?: Weekday[];
  }): Promise<void>;
  /** Runs `edit_alarm` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  edit_alarmMany(items: EditAlarmArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Turns an alarm on or off: off, it does not ring; on, it rings next at its time. */
  set_alarm_enabled(args?: {
    enabled?: boolean;
  }): Promise<void>;
  /** Runs `set_alarm_enabled` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_alarm_enabledMany(items: SetAlarmEnabledArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Names the device an alarm rings on: one device's id, or none for every device of the person's. */
  set_alarm_device(args: {
    rings_on: string;
  }): Promise<void>;
  /** Runs `set_alarm_device` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_alarm_deviceMany(items: SetAlarmDeviceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Repeats an alarm on `d`, or stops repeating on it: a day it names rings, a day it does not skips. Its next ring moves to the next day it names. */
  toggle_day(args: {
    d: Weekday;
  }): Promise<void>;
  /** Runs `toggle_day` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  toggle_dayMany(items: ToggleDayArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Snoozes the alarm ringing now: it rings again in five minutes. */
  snooze_alarm(): Promise<void>;
  /** Runs `snooze_alarm` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  snooze_alarmMany(items: SnoozeAlarmArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Rings an alarm when its moment comes, on the device it names, window or no window. A repeating alarm rings again the next day it names; one that rings once turns itself off. */
  ring_alarm(): Promise<void>;
  /** Runs `ring_alarm` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  ring_alarmMany(items: RingAlarmArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Alarm` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ALARM_ACTIONS = ["edit_alarm", "edit_alarmMany", "set_alarm_enabled", "set_alarm_enabledMany", "set_alarm_device", "set_alarm_deviceMany", "toggle_day", "toggle_dayMany", "snooze_alarm", "snooze_alarmMany", "ring_alarm", "ring_alarmMany"] as const;

/** The `tl.so.tilo.clocks.Alarm` kind at `path`. */
export function alarm(ws: tilo.Workspace, path: string): Alarm {
  return resourceProxy(ws, path, SURFACE, ALARM_ACTIONS) as unknown as Alarm;
}

/** Timer — `tl.so.tilo.clocks.Timer`. */
export interface Timer extends tilo.Resource {
  nhqnhguz: string;  // label
  "5b7nw57j": number;  // seconds
  "6anly4z4": string | null;  // ends_at
  ufpse6mi: number | null;  // left
  /** Pauses a running timer where it stands. */
  pause_timer(): Promise<void>;
  /** Runs `pause_timer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pause_timerMany(items: PauseTimerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Resumes a paused timer, or starts a finished one over. */
  resume_timer(): Promise<void>;
  /** Runs `resume_timer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  resume_timerMany(items: ResumeTimerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds a minute to a timer, running or paused. */
  add_minute(): Promise<void>;
  /** Runs `add_minute` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  add_minuteMany(items: AddMinuteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Rings a timer when it finishes, on every device of the person's, window or no window. */
  ring_timer(): Promise<void>;
}

/** The actions `Timer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TIMER_ACTIONS = ["pause_timer", "pause_timerMany", "resume_timer", "resume_timerMany", "add_minute", "add_minuteMany", "ring_timer"] as const;

/** The `tl.so.tilo.clocks.Timer` kind at `path`. */
export function timer(ws: tilo.Workspace, path: string): Timer {
  return resourceProxy(ws, path, SURFACE, TIMER_ACTIONS) as unknown as Timer;
}

/** Lap — `tl.so.tilo.clocks.Lap`. */
export interface Lap extends tilo.Resource {
  mq5diuqb: number;  // at
}

/** The actions `Lap` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LAP_ACTIONS = [] as const;

/** The `tl.so.tilo.clocks.Lap` kind at `path`. */
export function lap(ws: tilo.Workspace, path: string): Lap {
  return resourceProxy(ws, path, SURFACE, LAP_ACTIONS) as unknown as Lap;
}

/** Stopwatch — `tl.so.tilo.clocks.Stopwatch`. */
export interface Stopwatch extends tilo.Resource {
  yhijhjuh: number;  // elapsed
  fg2gmze6: string | null;  // since
  ngcmcd7g: Array<tilo.Entry<Lap>>;  // laps
}

/** The actions `Stopwatch` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STOPWATCH_ACTIONS = [] as const;

/** The `tl.so.tilo.clocks.Stopwatch` kind at `path`. */
export function stopwatch(ws: tilo.Workspace, path: string): Stopwatch {
  return resourceProxy(ws, path, SURFACE, STOPWATCH_ACTIONS) as unknown as Stopwatch;
}

/** Clocks — `tl.so.tilo.clocks.Clocks`. */
export interface Clocks extends tilo.Resource {
  r2amisi7: Array<tilo.Entry<WorldClock>>;  // world_clocks
  gneumngu: Array<tilo.Entry<Alarm>>;  // alarms
  kahapraa: Array<tilo.Entry<Timer>>;  // timers
  rjh2x76r: Stopwatch;  // stopwatch
  hnwcprw3: boolean;  // active
  /** Adds a city's clock to the workspace. */
  add_world_clock(args: {
    city: string;
    zone: string;
  }): Promise<WorldClock>;
  /** Removes a city's clock. */
  remove_world_clock(args: {
    w: WorldClock;
  }): Promise<void>;
  /** Runs `remove_world_clock` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_world_clockMany(items: RemoveWorldClockArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Sets an alarm: at the time of day `when` names, on the days it repeats on, ringing on `rings_on`, every device of the person's when it names none. It rings first at the next such time. */
  add_alarm(args: {
    label: string;
    hour: number;
    minute: number;
    days?: Weekday[];
    rings_on?: string;
  }): Promise<Alarm>;
  /** Removes an alarm. */
  remove_alarm(args: {
    a: Alarm;
  }): Promise<void>;
  /** Runs `remove_alarm` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_alarmMany(items: RemoveAlarmArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a timer of `seconds`. */
  add_timer(args: {
    label: string;
    seconds: number;
  }): Promise<Timer>;
  /** Removes a timer. */
  remove_timer(args: {
    t: Timer;
  }): Promise<void>;
  /** Runs `remove_timer` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_timerMany(items: RemoveTimerArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts the stopwatch. */
  start_stopwatch(): Promise<void>;
  /** Runs `start_stopwatch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  start_stopwatchMany(items: StartStopwatchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Pauses the stopwatch where it stands. */
  pause_stopwatch(): Promise<void>;
  /** Runs `pause_stopwatch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  pause_stopwatchMany(items: PauseStopwatchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps a lap: the time the stopwatch stands at. */
  lap(): Promise<void>;
  /** Runs `lap` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  lapMany(items: LapArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Resets the stopwatch: it is at zero and stopped, with no laps. */
  reset_stopwatch(): Promise<void>;
  /** Runs `reset_stopwatch` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  reset_stopwatchMany(items: ResetStopwatchArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Clocks` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const CLOCKS_ACTIONS = ["add_world_clock", "remove_world_clock", "remove_world_clockMany", "add_alarm", "remove_alarm", "remove_alarmMany", "add_timer", "remove_timer", "remove_timerMany", "start_stopwatch", "start_stopwatchMany", "pause_stopwatch", "pause_stopwatchMany", "lap", "lapMany", "reset_stopwatch", "reset_stopwatchMany"] as const;

/** The `tl.so.tilo.clocks.Clocks` kind at `path`. */
export function clocks(ws: tilo.Workspace, path: string): Clocks {
  return resourceProxy(ws, path, SURFACE, CLOCKS_ACTIONS) as unknown as Clocks;
}

/** Clocks — `tl.so.tilo.clocks`. */
export interface TiloClocks extends tilo.Resource {
}

/** The actions `TiloClocks` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TILOCLOCKS_ACTIONS = [] as const;

/** The `tl.so.tilo.clocks` kind at `path`. */
export function tiloClocks(ws: tilo.Workspace, path: string): TiloClocks {
  return resourceProxy(ws, path, SURFACE, TILOCLOCKS_ACTIONS) as unknown as TiloClocks;
}

