/**
 * Types for Weather (`so.tilo.weather`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.toml`; write
 * it again rather than editing it.
 *
 * `Hour` is a `Resource` of the app's `tl.so.tilo.weather.Hour`, made by its factory with the
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
export const SURFACE = "so.tilo.weather@e5dcf21ad5da326c";

/** The arguments of `use_as_default`. */
export interface UseAsDefaultArgs {
  place: Place;
}

/** The arguments of `move_to_top`. */
export interface MoveToTopArgs {
  place: Place;
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

/** The arguments of `poll_weather`. */
export interface PollWeatherArgs {
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
  ikazv44r: boolean;  // read
  gbrotevm: Reading;  // current
  qt6zc3me: Array<tilo.Entry<Hour>>;  // hours
  fjbo2bs7: Array<tilo.Entry<Day>>;  // days
  qz7n6v2n: Air;  // air
  uthtg3cy: boolean;  // has_air
  "42oai4b2": string | null;  // fetched
  lkrdcuut: boolean;  // stale
  ckqlainl: string;  // trouble
}

/** The actions `Place` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLACE_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.Place` kind at `path`. */
export function place(ws: tilo.Workspace, path: string): Place {
  return resourceProxy(ws, path, SURFACE, PLACE_ACTIONS) as unknown as Place;
}

/** Refresh — `tl.so.tilo.weather.Refresh`. */
export interface Refresh extends tilo.Resource {
  j4qvmrkj: string;  // job
  "6a665z7q": string;  // state
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
  xx2oi6v5: string;  // job
  cqmaksmt: boolean;  // searching
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
  esvv4ymu: Refresh;  // refresh
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
  /** Removes a place from the list; what was read of it goes with it. Undo brings it back. */
  remove_place(args: {
    place: Place;
  }): Promise<void>;
  /** Runs `remove_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  remove_placeMany(items: RemovePlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  // the app's `refresh` action is the SDK's own: run it with `action("refresh")`.
  /** Stops an update that is running. */
  stop_refresh(): Promise<void>;
  /** Runs `stop_refresh` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stop_refreshMany(items: StopRefreshArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Updates a place whose last read failed, once the person asks again. */
  retry_place(args: {
    place: Place;
  }): Promise<void>;
  /** Runs `retry_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  retry_placeMany(items: RetryPlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes what the device has read so far: each place's answer as it lands, and what the service says of it. What an update reads is nobody's step of their own, so landing it says nothing and offers no undo of its own. A second read of the same place replaces the first. */
  poll_weather(): Promise<void>;
  /** Runs `poll_weather` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  poll_weatherMany(items: PollWeatherArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Looks for places by name. The device asks the service away from the frame; the window takes the rows as they land. */
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
export const LOCATIONS_ACTIONS = ["save_place", "use_as_default", "use_as_defaultMany", "move_to_top", "move_to_topMany", "remove_place", "remove_placeMany", "stop_refresh", "stop_refreshMany", "retry_place", "retry_placeMany", "poll_weather", "poll_weatherMany", "begin_search", "end_search", "end_searchMany", "add_found", "accept_found", "accept_foundMany"] as const;

/** The `tl.so.tilo.weather.Locations` kind at `path`. */
export function locations(ws: tilo.Workspace, path: string): Locations {
  return resourceProxy(ws, path, SURFACE, LOCATIONS_ACTIONS) as unknown as Locations;
}

/** Ask — `tl.so.tilo.weather.Ask`. */
export interface Ask extends tilo.Resource {
  wf3zmolm: AskPlace[];  // places
}

/** The actions `Ask` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ASK_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.Ask` kind at `path`. */
export function ask(ws: tilo.Workspace, path: string): Ask {
  return resourceProxy(ws, path, SURFACE, ASK_ACTIONS) as unknown as Ask;
}

/** AskPlace — `tl.so.tilo.weather.AskPlace`. */
export interface AskPlace extends tilo.Resource {
  gdy7duy6: string;  // place_id
  "5btxbecx": number;  // latitude
  dcshjxzm: number;  // longitude
}

/** The actions `AskPlace` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ASKPLACE_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.AskPlace` kind at `path`. */
export function askPlace(ws: tilo.Workspace, path: string): AskPlace {
  return resourceProxy(ws, path, SURFACE, ASKPLACE_ACTIONS) as unknown as AskPlace;
}

/** SearchAsk — `tl.so.tilo.weather.SearchAsk`. */
export interface SearchAsk extends tilo.Resource {
  "4opjitfa": string;  // query
}

/** The actions `SearchAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SEARCHASK_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.SearchAsk` kind at `path`. */
export function searchAsk(ws: tilo.Workspace, path: string): SearchAsk {
  return resourceProxy(ws, path, SURFACE, SEARCHASK_ACTIONS) as unknown as SearchAsk;
}

/** TakeAsk — `tl.so.tilo.weather.TakeAsk`. */
export interface TakeAsk extends tilo.Resource {
  "6vnqzh4l": string;  // job
}

/** The actions `TakeAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TAKEASK_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.TakeAsk` kind at `path`. */
export function takeAsk(ws: tilo.Workspace, path: string): TakeAsk {
  return resourceProxy(ws, path, SURFACE, TAKEASK_ACTIONS) as unknown as TakeAsk;
}

/** Job — `tl.so.tilo.weather.Job`. */
export interface Job extends tilo.Resource {
  lwhvatgx: string;  // state
  ka2h5wk7: number;  // done
  lbqaguom: number;  // total
}

/** The actions `Job` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const JOB_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.Job` kind at `path`. */
export function job(ws: tilo.Workspace, path: string): Job {
  return resourceProxy(ws, path, SURFACE, JOB_ACTIONS) as unknown as Job;
}

/** Taken — `tl.so.tilo.weather.Taken`. */
export interface Taken extends tilo.Resource {
  gbqubsmc: string[];  // answers
  d4onixyo: string[];  // places
  ra4o5i2z: string;  // error
}

/** The actions `Taken` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TAKEN_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.Taken` kind at `path`. */
export function taken(ws: tilo.Workspace, path: string): Taken {
  return resourceProxy(ws, path, SURFACE, TAKEN_ACTIONS) as unknown as Taken;
}

/** Answer — `tl.so.tilo.weather.Answer`. */
export interface Answer extends tilo.Resource {
  "5prprzwo": string;  // place_id
  papzs3jy: boolean;  // ok
  "2t7oi34p": string;  // reason
  opdrm37r: number;  // status
  oh3swlwe: string;  // message
  szxryqtq: number;  // fetched
  udln4y7a: string;  // time
  bt4m22bz: number;  // temperature
  "5wz5vx2n": number;  // apparent
  kk67hrwa: number;  // humidity
  wssxi3hh: number;  // precipitation
  z2xmdn5s: number;  // code
  "45ln3j6b": number;  // wind
  "7sdt6b3s": number;  // wind_from
  "6pbvus4v": number;  // pressure
  "7twngswb": number;  // uv
  gfeyv7li: boolean;  // is_day
  do7cloel: string[];  // hour_at
  adqc7myi: number[];  // hour_temp
  m4tdmh3g: number[];  // hour_chance
  "6w6giapb": number[];  // hour_code
  ercv5gi6: boolean[];  // hour_day
  "6vlbpjn2": string[];  // day_date
  xpkpnifa: number[];  // day_weekday
  "7mza74v3": number[];  // day_high
  v5etkrnq: number[];  // day_low
  lsdyx245: number[];  // day_chance
  hw7afts4: number[];  // day_code
  rv4jwm5h: number[];  // day_wind
  eksw2wiu: number[];  // day_uv
  cl5k7le3: string[];  // day_rise
  zjervfc5: string[];  // day_set
  "4kcw2w6l": boolean;  // air_ok
  clhldsl6: string;  // air_at
  fxpfimss: number | null;  // air_european
  zhjhkay7: number | null;  // air_us
  "55ge75je": number | null;  // air_pm2_5
  "363wfecw": number | null;  // air_pm10
  w77tgu5z: number | null;  // air_no2
  qo2ngnib: number | null;  // air_ozone
}

/** The actions `Answer` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ANSWER_ACTIONS = [] as const;

/** The `tl.so.tilo.weather.Answer` kind at `path`. */
export function answer(ws: tilo.Workspace, path: string): Answer {
  return resourceProxy(ws, path, SURFACE, ANSWER_ACTIONS) as unknown as Answer;
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

