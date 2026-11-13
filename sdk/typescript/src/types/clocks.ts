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
 * says where it is.
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

/** Clocks — `clocks.collection`. */
export interface Collection extends tilo.Resource {
  world_clocks: WorldClock[];
  alarms: Alarm[];
  timers: Timer[];
  readonly stopwatch: Stopwatch;
  addWorldClock(args: {
    city: string;
    timezone: string;
  }): Promise<tilo.Run | void>;
  removeWorldClock(args: {
    world_clock: string;
  }): Promise<tilo.Run | void>;
  addAlarm(args: {
    label: string;
    hour: number;
    minute: number;
    weekdays: WeekdayKey[];
  }): Promise<tilo.Run | void>;
  setAlarmEnabled(args: {
    alarm: string;
    enabled: boolean;
  }): Promise<tilo.Run | void>;
  editAlarm(args: {
    alarm: string;
    label: string;
    hour: number;
    minute: number;
    weekdays: WeekdayKey[];
  }): Promise<tilo.Run | void>;
  removeAlarm(args: {
    alarm: string;
  }): Promise<tilo.Run | void>;
  addTimer(args: {
    label: string;
    seconds: number;
  }): Promise<tilo.Run | void>;
  pauseTimer(args: {
    timer: string;
  }): Promise<tilo.Run | void>;
  resumeTimer(args: {
    timer: string;
  }): Promise<tilo.Run | void>;
  addMinute(args: {
    timer: string;
  }): Promise<tilo.Run | void>;
  removeTimer(args: {
    timer: string;
  }): Promise<tilo.Run | void>;
  startStopwatch(): Promise<tilo.Run | void>;
  pauseStopwatch(): Promise<tilo.Run | void>;
  lap(): Promise<tilo.Run | void>;
  resetStopwatch(): Promise<tilo.Run | void>;
}

/** The `clocks.collection` kind at `path`. */
export function collection(ws: tilo.Workspace, path: string): Collection {
  return resourceProxy(ws, path, SURFACE) as unknown as Collection;
}

/** Clocks on each device — `clocks.on-devices`. */
export interface OnDevices extends tilo.Resource {
  devices: Record<string, unknown>;
  keep(args: {
    device: string;
    kept: unknown;
  }): Promise<tilo.Run | void>;
}

/** The `clocks.on-devices` kind at `path`. */
export function onDevices(ws: tilo.Workspace, path: string): OnDevices {
  return resourceProxy(ws, path, SURFACE) as unknown as OnDevices;
}

