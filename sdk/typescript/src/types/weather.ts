/**
 * Types for Weather (`so.tilo.weather`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`weather/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.weather@2eec6dbb293b7936";

/** The arguments of `use_as_default`. */
export interface UseAsDefaultArgs {
  place: Place;
}

/** The arguments of `move_to_top`. */
export interface MoveToTopArgs {
  place: Place;
}

/** The arguments of `move_place`. */
export interface MovePlaceArgs {
  ids: string[];
}

/** The arguments of `rename_place`. */
export interface RenamePlaceArgs {
  place: Place;
  name: string;
}

/** The arguments of `remove_place`. */
export interface RemovePlaceArgs {
  place: Place;
}

/** The arguments of `stop_refresh`. */
export interface StopRefreshArgs {
}

/** The arguments of `retry_place`. */
export interface RetryPlaceArgs {
  place: Place;
}

/** The arguments of `read_next`. */
export interface ReadNextArgs {
}

/** The arguments of `poll_weather`. */
export interface PollWeatherArgs {
}

/** The arguments of `look_for_place`. */
export interface LookForPlaceArgs {
}

/** The arguments of `end_search`. */
export interface EndSearchArgs {
  reader: Searching;
}

/** The arguments of `accept_found`. */
export interface AcceptFoundArgs {
  reader: Searching;
  choice: string;
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

/** Hour — `tl.so.tilo.weather.Hour`. */
export interface Hour extends tilo.Resource {
  hqg5l743: string;  // at
  nz766xgs: number;  // temperature
  bszivu27: number;  // chance
  w4hqkb3u: number;  // code
  rx2vbln3: boolean;  // day
  il4besep: string;  // icon
}

/** The actions `Hour` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const HOUR_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.Hour` kind at `path`. */
export function hour(ws: tilo.Workspace, path: string): Hour {
  return resourceProxy(ws, path, SURFACE, HOUR_ACTIONS) as unknown as Hour;
}

/** Day — `tl.so.tilo.weather.Day`. */
export interface Day extends tilo.Resource {
  jpx34w56: string;  // date
  "7yqjq2lc": number;  // weekday
  dyfvgxu3: number;  // high
  hgvg5w2j: number;  // low
  nle53w7c: number;  // chance
  "6nrssehp": number;  // code
  og6bqxmv: number;  // wind
  ttyi3z7y: number;  // uv
  zxdugvp7: string;  // rise
  f2qdxlbk: string;  // set
  scsrpy5m: string;  // icon
}

/** The actions `Day` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const DAY_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.Day` kind at `path`. */
export function day(ws: tilo.Workspace, path: string): Day {
  return resourceProxy(ws, path, SURFACE, DAY_ACTIONS) as unknown as Day;
}

/** Reading — `tl.so.tilo.weather.Reading`. */
export interface Reading extends tilo.Resource {
  wz5kgppi: string;  // at
  nfn3o6kh: number;  // temperature
  npx3rbww: number;  // apparent
  adkhdcmi: number;  // humidity
  z25jhm5f: number;  // precipitation
  pexond7v: number;  // code
  zdiyx5q6: number;  // wind
  tcofpdw2: number;  // wind_from
  ulscmhjp: number;  // pressure
  h2zd2igi: number;  // uv
  hilw2sek: boolean;  // day
  "3imkwm63": string;  // icon
}

/** The actions `Reading` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const READING_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.Reading` kind at `path`. */
export function reading(ws: tilo.Workspace, path: string): Reading {
  return resourceProxy(ws, path, SURFACE, READING_ACTIONS) as unknown as Reading;
}

/** Air — `tl.so.tilo.weather.Air`. */
export interface Air extends tilo.Resource {
  qhxusqd7: string;  // at
  b43at2ai: number | null;  // european
  rwv4mbrr: number | null;  // us
  fpf3tzoo: number | null;  // pm2_5
  we4qtcij: number | null;  // pm10
  vjey5w7m: number | null;  // no2
  "5pnfxgbq": number | null;  // ozone
}

/** The actions `Air` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const AIR_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.Air` kind at `path`. */
export function air(ws: tilo.Workspace, path: string): Air {
  return resourceProxy(ws, path, SURFACE, AIR_ACTIONS) as unknown as Air;
}

/** Place — `tl.so.tilo.weather.Place`. */
export interface Place extends tilo.Resource {
  bacjedt3: string;  // name
  aohkzfz3: string;  // region
  j6rsr6zo: string;  // country
  "26l3gl47": string;  // country_code
  ax54oez6: number;  // latitude
  ewmohhie: number;  // longitude
  y5lbxwn7: string;  // timezone
}

/** The actions `Place` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLACE_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.Place` kind at `path`. */
export function place(ws: tilo.Workspace, path: string): Place {
  return resourceProxy(ws, path, SURFACE, PLACE_ACTIONS) as unknown as Place;
}

/** PlaceReading — `tl.so.tilo.weather.PlaceReading`. */
export interface PlaceReading extends tilo.Resource {
  uzxnvv67: string;  // name
  hfv2phzu: string;  // place
  kgqshwck: boolean;  // read
  eecg6i33: Reading;  // current
  yc3wt5yt: Array<tilo.Entry<Hour>>;  // hours
  "7xtuywip": Array<tilo.Entry<Day>>;  // days
  "7eu3357v": Air;  // air
  gqv7rwej: boolean;  // has_air
  caagd74f: string | null;  // fetched
  juxtrvon: boolean;  // stale
  losawnjy: string;  // trouble
  /** The forecast a service answered, written into this device's reading of the place: its current reading, a day of hours and a week of days. False when the body is not a forecast the service could have sent. */
  apply_forecast(args: {
    body: string;
  }): Promise<boolean>;
}

/** The actions `PlaceReading` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLACEREADING_ACTIONS = ["apply_forecast"] as const;

/** The `tl.so.tilo.weather.PlaceReading` kind at `path`. */
export function placeReading(ws: tilo.Workspace, path: string): PlaceReading {
  return resourceProxy(ws, path, SURFACE, PLACEREADING_ACTIONS) as unknown as PlaceReading;
}

/** Refresh — `tl.so.tilo.weather.Refresh`. */
export interface Refresh extends tilo.Resource {
  "6a665z7q": string;  // state
  vawmqqsv: string[];  // waiting
  umztfj5e: number;  // done
  "6an5syvf": number;  // total
  lrbilhih: number;  // failed
  bf4xzg4n: string;  // note
}

/** The actions `Refresh` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const REFRESH_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.Refresh` kind at `path`. */
export function refresh(ws: tilo.Workspace, path: string): Refresh {
  return resourceProxy(ws, path, SURFACE, REFRESH_ACTIONS) as unknown as Refresh;
}

/** Found — `tl.so.tilo.weather.Found`. */
export interface Found extends tilo.Resource {
  mgz6jurw: string;  // name
  "6jas4neh": number;  // latitude
  gvppngv7: number;  // longitude
  kx6bm63e: string;  // country
  ubamq752: string;  // country_code
  a2wm7g5t: string;  // region
  "5hvczxlz": string;  // timezone
}

/** The actions `Found` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FOUND_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.Found` kind at `path`. */
export function found(ws: tilo.Workspace, path: string): Found {
  return resourceProxy(ws, path, SURFACE, FOUND_ACTIONS) as unknown as Found;
}

/** Searching — `tl.so.tilo.weather.Searching`. */
export interface Searching extends tilo.Resource {
  fmhvvp6l: string;  // wanted
  o4whnz6t: string;  // asked
  jwbxovvp: string;  // pending
  "5o5rohu5": Array<tilo.Entry<Found>>;  // found
  wgepvfwu: string;  // note
}

/** The actions `Searching` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SEARCHING_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.Searching` kind at `path`. */
export function searching(ws: tilo.Workspace, path: string): Searching {
  return resourceProxy(ws, path, SURFACE, SEARCHING_ACTIONS) as unknown as Searching;
}

/** Locations — `tl.so.tilo.weather.Locations`. */
export interface Locations extends tilo.Resource {
  b2qn6gqw: Array<tilo.Entry<Place>>;  // places
  vqfze2cq: Place | null;  // default
  ms5dkio6: Array<tilo.Entry<Searching>>;  // searches
  "73zdj7wy": string;  // today
  rusdri25: string;  // tomorrow
  /** Saves a place a person follows. A place near one already saved is that place, not a second entry, and a new place is the one windows show first. */
  save_place(args: {
    name: string;
    latitude: number;
    longitude: number;
    region?: string;
    country?: string;
    country_code?: string;
    timezone?: string;
  }): Promise<Place>;
  /** Shows a place's forecast first whenever the app opens. */
  use_as_default(args: {
    place: Place;
  }): Promise<void>;
  /** Runs `use_as_default` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  use_as_defaultMany(items: UseAsDefaultArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves a place to the top of the list. */
  move_to_top(args: {
    place: Place;
  }): Promise<void>;
  /** Runs `move_to_top` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_to_topMany(items: MoveToTopArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Moves the place a person dragged, before the one it was dropped on: the list's reorder reports the two ids in that order. */
  move_place(args: {
    ids: string[];
  }): Promise<void>;
  /** Runs `move_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  move_placeMany(items: MovePlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames a place the person follows. */
  rename_place(args: {
    place: Place;
    name: string;
  }): Promise<void>;
  /** Runs `rename_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_placeMany(items: RenamePlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Removes a place from the list; what was read of it goes with it. Undo brings it back. */
  remove_place(args: {
    place: Place;
  }): Promise<void>;
  /** Runs `remove_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_placeMany(items: RemovePlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  // the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
  /** Stops an update that is running; the places it had not read keep what they had. */
  stop_refresh(): Promise<void>;
  /** Runs `stop_refresh` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stop_refreshMany(items: StopRefreshArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Updates a place whose last read failed, once the person asks again. */
  retry_place(args: {
    place: Place;
  }): Promise<void>;
  /** Runs `retry_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  retry_placeMany(items: RetryPlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Reads one place an update has still to read, and says when the update has read them all. */
  read_next(): Promise<void>;
  /** Runs `read_next` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  read_nextMany(items: ReadNextArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes what the last read found: a place's reading and its hours and days land together, and the search's rows beside the field. What an update reads is nobody's step of their own, so landing it says nothing and offers no undo of its own. A second read of the same place replaces the first. */
  poll_weather(): Promise<void>;
  /** Runs `poll_weather` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  poll_weatherMany(items: PollWeatherArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Looks for what the person typed once it has stopped changing: the query waits one look, and the device is asked for the last query only. */
  look_for_place(): Promise<void>;
  /** Runs `look_for_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  look_for_placeMany(items: LookForPlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Looks for places by name: the window asks the service once per query as it is typed. */
  begin_search(): Promise<Searching>;
  /** Empties the field and stops looking. */
  end_search(args: {
    reader: Searching;
  }): Promise<void>;
  /** Runs `end_search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  end_searchMany(items: EndSearchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Saves the place a search found, by its place in the list of what it found, and shows it. */
  add_found(args: {
    reader: Searching;
    at: number;
  }): Promise<Place>;
  /** Takes the row a person picked from the search: saves the place and empties the search. */
  accept_found(args: {
    reader: Searching;
    choice: string;
  }): Promise<void>;
  /** Runs `accept_found` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  accept_foundMany(items: AcceptFoundArgs[]): Promise<tilo.ActionAnswer[]>;
}

/** The actions `Locations` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LOCATIONS_ACTIONS = ["save_place", "use_as_default", "use_as_defaultMany", "move_to_top", "move_to_topMany", "move_place", "move_placeMany", "rename_place", "rename_placeMany", "remove_place", "remove_placeMany", "stop_refresh", "stop_refreshMany", "retry_place", "retry_placeMany", "read_next", "read_nextMany", "poll_weather", "poll_weatherMany", "look_for_place", "look_for_placeMany", "begin_search", "end_search", "end_searchMany", "add_found", "accept_found", "accept_foundMany"] as const;

/** The `tl.so.tilo.weather.Locations` kind at `path`. */
export function locations(ws: tilo.Workspace, path: string): Locations {
  return resourceProxy(ws, path, SURFACE, LOCATIONS_ACTIONS) as unknown as Locations;
}

/** Readings — `tl.so.tilo.weather.Readings`. */
export interface Readings extends tilo.Resource {
  "6fze2ppw": Array<tilo.Entry<PlaceReading>>;  // of
  vf63zjb2: Refresh;  // refresh
  /** What this device read of `place`, made the first time something writes it. */
  reading_for(args: {
    place: Place;
  }): Promise<PlaceReading>;
  /** Reads one place's forecast and air quality. One failing leaves the other's answer standing, and a place the service did not answer keeps what it had and says why. */
  read_place(args: {
    place: Place;
  }): Promise<boolean>;
}

/** The actions `Readings` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const READINGS_ACTIONS = ["reading_for", "read_place"] as const;

/** The `tl.so.tilo.weather.Readings` kind at `path`. */
export function readings(ws: tilo.Workspace, path: string): Readings {
  return resourceProxy(ws, path, SURFACE, READINGS_ACTIONS) as unknown as Readings;
}

/** Weather — `tl.so.tilo.weather`. */
export interface Weather extends tilo.Resource {
}

/** The actions `Weather` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const WEATHER_ACTIONS = [] as const;

/** The `tl.so.tilo.weather` kind at `path`. */
export function weather(ws: tilo.Workspace, path: string): Weather {
  return resourceProxy(ws, path, SURFACE, WEATHER_ACTIONS) as unknown as Weather;
}

