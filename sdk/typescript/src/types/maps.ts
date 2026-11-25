/**
 * Types for Maps (`so.tilo.maps`), from the app's surface.
 *
 * `tilo sdk gen --lang ts` writes this file from the app's `surface.json`; write
 * it again rather than editing it.
 *
 * `Notice` is a `Resource` of the app's `tl.tilo.host.Notice`, made by its factory with the
 * resource's path (`maps/<resource>`): its attributes are the app's fields
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
export const SURFACE = "so.tilo.maps@9bac66f79e6a6549";

/** What a place is: the marker's ink, the card's glyph, and the OpenStreetMap class it maps from. One of "landmark", "cafe", "restaurant", "bar", "museum", "park", "transit", "hotel", "shop", "hospital", "other". */
export type Category = "landmark" | "cafe" | "restaurant" | "bar" | "museum" | "park" | "transit" | "hotel" | "shop" | "hospital" | "other";

/** A turn of a route. One of "depart", "continue_on", "slight_left", "left", "sharp_left", "slight_right", "right", "sharp_right", "uturn", "roundabout", "merging", "arrive". */
export type Maneuver = "depart" | "continue_on" | "slight_left" | "left" | "sharp_left" | "slight_right" | "right" | "sharp_right" | "uturn" | "roundabout" | "merging" | "arrive";

/** How directions travel. One of "car", "transit", "bike", "foot". */
export type TravelMode = "car" | "transit" | "bike" | "foot";

/** The way one leg of a public transport journey travels. One of "walk", "bus", "tram", "subway", "rail", "ferry". */
export type TransitMode = "walk" | "bus" | "tram" | "subway" | "rail" | "ferry";

/** The arguments of `save_place`. */
export interface SavePlaceArgs {
  saved?: boolean;
}

/** The arguments of `rename_place`. */
export interface RenamePlaceArgs {
  name: string;
}

/** The arguments of `forget_found`. */
export interface ForgetFoundArgs {
}

/** The arguments of `remember`. */
export interface RememberArgs {
  query: string;
}

/** The arguments of `take_in`. */
export interface TakeInArgs {
  places: Place[];
}

/** The arguments of `accept_places`. */
export interface AcceptPlacesArgs {
  places: Place[];
}

/** The arguments of `search`. */
export interface SearchArgs {
  query: string;
}

/** The arguments of `clear_search`. */
export interface ClearSearchArgs {
}

/** The arguments of `clear_recents`. */
export interface ClearRecentsArgs {
}

/** The arguments of `open_place`. */
export interface OpenPlaceArgs {
  place: string;
}

/** The arguments of `show_point`. */
export interface ShowPointArgs {
  latitude: number;
  longitude: number;
}

/** The arguments of `locate`. */
export interface LocateArgs {
}

/** The arguments of `zoom_map`. */
export interface ZoomMapArgs {
  direction: number;
}

/** The arguments of `set_mode`. */
export interface SetModeArgs {
  mode: TravelMode;
}

/** The arguments of `set_transit_time`. */
export interface SetTransitTimeArgs {
  arrive_by: boolean;
  offset_min: number;
}

/** The arguments of `directions`. */
export interface DirectionsArgs {
  from_words: string;
  to_words: string;
}

/** The arguments of `plan_transit`. */
export interface PlanTransitArgs {
  from: Place;
  to: Place;
}

/** The arguments of `accept_route`. */
export interface AcceptRouteArgs {
  routes: Route[];
  from_name: string;
  to_name: string;
}

/** The arguments of `another_route`. */
export interface AnotherRouteArgs {
  at?: number;
}

/** The arguments of `fit_map`. */
export interface FitMapArgs {
}

/** The arguments of `clear_route`. */
export interface ClearRouteArgs {
}

/** The arguments of `accept_transit`. */
export interface AcceptTransitArgs {
  journeys: Journey[];
  to_name: string;
}

/** The arguments of `another_journey`. */
export interface AnotherJourneyArgs {
  at?: number;
}

/** The arguments of `clear_transit`. */
export interface ClearTransitArgs {
}

/** The arguments of `start_navigation`. */
export interface StartNavigationArgs {
}

/** The arguments of `end_navigation`. */
export interface EndNavigationArgs {
}

/** The arguments of `find_nearby`. */
export interface FindNearbyArgs {
}

/** The arguments of `open_poi`. */
export interface OpenPoiArgs {
  poi: string;
}

/** The arguments of `download_view`. */
export interface DownloadViewArgs {
  north?: number;
  south?: number;
  east?: number;
  west?: number;
  least_zoom?: number;
  most_zoom?: number;
}

/** The arguments of `download_place`. */
export interface DownloadPlaceArgs {
  query: string;
  least_zoom?: number;
  most_zoom?: number;
}

/** The arguments of `keep_area`. */
export interface KeepAreaArgs {
  area: KeptArea;
}

/** The arguments of `save_area`. */
export interface SaveAreaArgs {
  area: KeptArea;
}

/** The arguments of `area_saved`. */
export interface AreaSavedArgs {
  area: KeptArea;
  j: unknown;
}

/** The arguments of `stop_offline_area`. */
export interface StopOfflineAreaArgs {
  area: string;
}

/** The arguments of `continue_offline_area`. */
export interface ContinueOfflineAreaArgs {
  area: string;
}

/** The arguments of `delete_offline_area`. */
export interface DeleteOfflineAreaArgs {
  area: string;
}

/** The arguments of `clear_offline_areas`. */
export interface ClearOfflineAreasArgs {
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

/** Place — `tl.so.tilo.maps.Place`. */
export interface Place extends tilo.Resource {
  rcalop4o: string;  // name
  m5b55osu: string;  // key
  lozw4dq7: Category;  // category
  opf2vbzc: string;  // address
  fiavc35r: number;  // latitude
  w552wzze: number;  // longitude
  s2ed7lsi: string;  // phone
  wgimrb5u: string;  // website
  qoo35pg5: boolean;  // saved
  sewsxq7v: boolean;  // found
  /** Keeps a place in the person's places, or takes it out. */
  save_place(args?: {
    saved?: boolean;
  }): Promise<void>;
  /** Runs `save_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  save_placeMany(items: SavePlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Renames a saved place; the name a search brought is the place's until the person gives it another. */
  rename_place(args: {
    name: string;
  }): Promise<void>;
  /** Runs `rename_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rename_placeMany(items: RenamePlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Calls the place's phone number, through the system's own open action. */
  call_place(): Promise<void>;
  /** Opens the place's website in the app that opens web addresses. */
  open_site(): Promise<void>;
}

/** The actions `Place` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLACE_ACTIONS = ["save_place", "save_placeMany", "rename_place", "rename_placeMany", "call_place", "open_site"] as const;

/** The `tl.so.tilo.maps.Place` kind at `path`. */
export function place(ws: tilo.Workspace, path: string): Place {
  return resourceProxy(ws, path, SURFACE, PLACE_ACTIONS) as unknown as Place;
}

/** Step — `tl.so.tilo.maps.Step`. */
export interface Step extends tilo.Resource {
  nfgqw3um: string;  // instruction
  bvjnbo5z: number;  // distance_m
  a4qv6woo: Maneuver;  // maneuver
}

/** The actions `Step` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const STEP_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.Step` kind at `path`. */
export function step(ws: tilo.Workspace, path: string): Step {
  return resourceProxy(ws, path, SURFACE, STEP_ACTIONS) as unknown as Step;
}

/** Route — `tl.so.tilo.maps.Route`. */
export interface Route extends tilo.Resource {
  ymsz4m34: TravelMode;  // mode
  ty6bfuoo: number;  // distance_m
  wc6ysmvx: number;  // duration_s
  uyrheaim: number[];  // geometry
  fyf6eckz: string;  // from_name
  bwd4zoxi: string;  // to_name
  yufxlbcl: number;  // from_latitude
  ws5q4ft3: number;  // from_longitude
  pepwxqg3: number;  // to_latitude
  "3lxyvouf": number;  // to_longitude
  ql4xlomv: Step[];  // steps
}

/** The actions `Route` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ROUTE_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.Route` kind at `path`. */
export function route(ws: tilo.Workspace, path: string): Route {
  return resourceProxy(ws, path, SURFACE, ROUTE_ACTIONS) as unknown as Route;
}

/** Leg — `tl.so.tilo.maps.Leg`. */
export interface Leg extends tilo.Resource {
  "3hgwl46g": TransitMode;  // mode
  oivvgw5v: string;  // line
  dk4ne74p: string;  // headsign
  sd7j2mow: string;  // from_name
  zsmxifsc: string;  // to_name
  "4pplnxwl": string;  // dep
  a6ukygsr: string;  // arr
  agzaego5: number;  // duration_min
  ptjnyoqs: number;  // num_stops
  i5zg4biq: string;  // colour
  g4fcmpin: string;  // text_colour
  rkrkjx3z: string;  // agency
  "7wrlvcuf": boolean;  // realtime
  e6sm3pwz: number[];  // geometry
}

/** The actions `Leg` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const LEG_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.Leg` kind at `path`. */
export function leg(ws: tilo.Workspace, path: string): Leg {
  return resourceProxy(ws, path, SURFACE, LEG_ACTIONS) as unknown as Leg;
}

/** Journey — `tl.so.tilo.maps.Journey`. */
export interface Journey extends tilo.Resource {
  xjeu7nif: string;  // dep
  bhvk42wt: string;  // arr
  sgpde2bq: number;  // duration_min
  ixfei2g5: number;  // transfers
  ica6larv: Leg[];  // legs
}

/** The actions `Journey` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const JOURNEY_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.Journey` kind at `path`. */
export function journey(ws: tilo.Workspace, path: string): Journey {
  return resourceProxy(ws, path, SURFACE, JOURNEY_ACTIONS) as unknown as Journey;
}

/** TransitTime — `tl.so.tilo.maps.TransitTime`. */
export interface TransitTime extends tilo.Resource {
  bv3pklkc: boolean;  // arrive_by
  "5hirpmkr": number;  // offset_min
}

/** The actions `TransitTime` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const TRANSITTIME_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.TransitTime` kind at `path`. */
export function transitTime(ws: tilo.Workspace, path: string): TransitTime {
  return resourceProxy(ws, path, SURFACE, TRANSITTIME_ACTIONS) as unknown as TransitTime;
}

/** Poi — `tl.so.tilo.maps.Poi`. */
export interface Poi extends tilo.Resource {
  kghw4vpb: string;  // name
  osqgf2vf: string;  // key
  ndtgscii: Category;  // category
  ilnfptos: number;  // latitude
  fdly24ym: number;  // longitude
  pdpenkqy: string;  // address
  vraa6lbe: string;  // hours
  t3erfdxp: string;  // website
  hxlh5wj4: string;  // phone
  gdtnxbln: string;  // kind
}

/** The actions `Poi` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const POI_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.Poi` kind at `path`. */
export function poi(ws: tilo.Workspace, path: string): Poi {
  return resourceProxy(ws, path, SURFACE, POI_ACTIONS) as unknown as Poi;
}

/** Recent — `tl.so.tilo.maps.Recent`. */
export interface Recent extends tilo.Resource {
  dozhur5v: string;  // query
  eyiulpwh: string;  // at
}

/** The actions `Recent` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const RECENT_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.Recent` kind at `path`. */
export function recent(ws: tilo.Workspace, path: string): Recent {
  return resourceProxy(ws, path, SURFACE, RECENT_ACTIONS) as unknown as Recent;
}

/** KeptArea — `tl.so.tilo.maps.KeptArea`. */
export interface KeptArea extends tilo.Resource {
  ibpdrk45: string;  // name
  ukxlhhqi: string;  // key
  mxxu653h: number;  // north
  qniubcbe: number;  // south
  yaeyddn2: number;  // east
  z3kr346y: number;  // west
  wygwldfb: number;  // least_zoom
  "3fo6bsu2": number;  // most_zoom
  "2gfef4uc": number;  // tiles
  ukhwybsm: number;  // done
  x6eit2xc: boolean;  // stopped
  "7s2dgers": string;  // job
}

/** The actions `KeptArea` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const KEPTAREA_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.KeptArea` kind at `path`. */
export function keptArea(ws: tilo.Workspace, path: string): KeptArea {
  return resourceProxy(ws, path, SURFACE, KEPTAREA_ACTIONS) as unknown as KeptArea;
}

/** KeptAreas — `tl.so.tilo.maps.KeptAreas`. */
export interface KeptAreas extends tilo.Resource {
  id2odelq: Array<tilo.Entry<KeptArea>>;  // kept
}

/** The actions `KeptAreas` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const KEPTAREAS_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.KeptAreas` kind at `path`. */
export function keptAreas(ws: tilo.Workspace, path: string): KeptAreas {
  return resourceProxy(ws, path, SURFACE, KEPTAREAS_ACTIONS) as unknown as KeptAreas;
}

/** SaveAreaAsk — `tl.so.tilo.maps.SaveAreaAsk`. */
export interface SaveAreaAsk extends tilo.Resource {
  uebjyxpv: string;  // id
  bomeo3ju: string;  // name
  "3p6krsqd": number;  // north
  qa5rfbrz: number;  // south
  kn7knr7e: number;  // east
  tgv5i2z6: number;  // west
  u4sjgx7w: number;  // least_zoom
  xqoyoobw: number;  // most_zoom
  z3a4l3gz: string;  // basemap
}

/** The actions `SaveAreaAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SAVEAREAASK_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.SaveAreaAsk` kind at `path`. */
export function saveAreaAsk(ws: tilo.Workspace, path: string): SaveAreaAsk {
  return resourceProxy(ws, path, SURFACE, SAVEAREAASK_ACTIONS) as unknown as SaveAreaAsk;
}

/** ForgetAreaAsk — `tl.so.tilo.maps.ForgetAreaAsk`. */
export interface ForgetAreaAsk extends tilo.Resource {
  atq5mwsp: string;  // id
}

/** The actions `ForgetAreaAsk` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const FORGETAREAASK_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.ForgetAreaAsk` kind at `path`. */
export function forgetAreaAsk(ws: tilo.Workspace, path: string): ForgetAreaAsk {
  return resourceProxy(ws, path, SURFACE, FORGETAREAASK_ACTIONS) as unknown as ForgetAreaAsk;
}

/** Atlas — `tl.so.tilo.maps.Atlas`. */
export interface Atlas extends tilo.Resource {
  q3ezxhmv: Array<tilo.Entry<Place>>;  // places
  vaus6gd3: Array<tilo.Entry<Recent>>;  // recents
  bdrkt5dv: Array<tilo.Entry<Poi>>;  // pois
  sxa3os5i: Array<tilo.Entry<Route>>;  // routes
  ia244ab6: number;  // route_at
  xbpiwexi: Array<tilo.Entry<Journey>>;  // journeys
  aok7qtg7: number;  // journey_at
  jzvaaswo: TransitTime;  // transit_time
  ywuwhj6m: TravelMode;  // mode
  "7i6mpbfw": string;  // query
  onljlflv: string;  // status
  woyw2cxx: number;  // centre_latitude
  zwui54r7: number;  // centre_longitude
  "4azkiaic": number;  // centre_zoom
  d54rah5t: boolean;  // navigating
  /** Marks every place a search found as no longer found, and forgets the ones nobody kept. */
  forget_found(): Promise<void>;
  /** Runs `forget_found` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  forget_foundMany(items: ForgetFoundArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Remembers a search, newest first, at most eight. */
  remember(args: {
    query: string;
  }): Promise<void>;
  /** Runs `remember` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  rememberMany(items: RememberArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Adds the places a search found to the map, keeping one the person already kept as theirs. */
  take_in(args: {
    places: Place[];
  }): Promise<void>;
  /** Runs `take_in` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  take_inMany(items: TakeInArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the places another app or an agent found into the map. */
  accept_places(args: {
    places: Place[];
  }): Promise<void>;
  /** Runs `accept_places` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  accept_placesMany(items: AcceptPlacesArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Searches the map for the places `query` names, and shows what it found. */
  search(args: {
    query: string;
  }): Promise<void>;
  /** Runs `search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  searchMany(items: SearchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the last search's results away. */
  clear_search(): Promise<void>;
  /** Runs `clear_search` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_searchMany(items: ClearSearchArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the searches the person made away. */
  clear_recents(): Promise<void>;
  /** Runs `clear_recents` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_recentsMany(items: ClearRecentsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens the directions beside the map. */
  show_directions(): Promise<void>;
  /** Opens or closes the directions beside the map, as its own row's toggle asks. */
  set_directions(args: {
    open: boolean;
  }): Promise<void>;
  /** Shows the place `place` on the map: the map opens on it. */
  open_place(args: {
    place: string;
  }): Promise<void>;
  /** Runs `open_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_placeMany(items: OpenPlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows a point on the map, as a link or another app names it. */
  show_point(args: {
    latitude: number;
    longitude: number;
  }): Promise<void>;
  /** Runs `show_point` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  show_pointMany(items: ShowPointArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Finds the device and shows where it is. */
  locate(): Promise<void>;
  /** Runs `locate` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  locateMany(items: LocateArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Zooms the map a level in or out: a level of imagery is the camera's zoom doubled or halved. */
  zoom_map(args: {
    direction: number;
  }): Promise<void>;
  /** Runs `zoom_map` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  zoom_mapMany(items: ZoomMapArgs[]): Promise<tilo.ActionAnswer[]>;
  /** How the map's directions travel. */
  set_mode(args: {
    mode: TravelMode;
  }): Promise<void>;
  /** Runs `set_mode` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_modeMany(items: SetModeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** When public transport journeys leave, or when they must arrive. */
  set_transit_time(args: {
    arrive_by: boolean;
    offset_min: number;
  }): Promise<void>;
  /** Runs `set_transit_time` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  set_transit_timeMany(items: SetTransitTimeArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Plans a route between two places the person named, by the mode directions travel, and shows it. */
  directions(args: {
    from_words: string;
    to_words: string;
  }): Promise<void>;
  /** Runs `directions` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  directionsMany(items: DirectionsArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Finds the journeys public transport offers between two places. */
  plan_transit(args: {
    from: Place;
    to: Place;
  }): Promise<void>;
  /** Runs `plan_transit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  plan_transitMany(items: PlanTransitArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the routes directions found as the map's, and shows the first. */
  accept_route(args: {
    routes: Route[];
    from_name: string;
    to_name: string;
  }): Promise<void>;
  /** Runs `accept_route` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  accept_routeMany(items: AcceptRouteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the next route directions offered for the same journey. */
  another_route(args?: {
    at?: number;
  }): Promise<void>;
  /** Runs `another_route` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  another_routeMany(items: AnotherRouteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows everything the map holds: the route it draws, or every place it knows. */
  fit_map(): Promise<void>;
  /** Runs `fit_map` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  fit_mapMany(items: FitMapArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the route away. */
  clear_route(): Promise<void>;
  /** Runs `clear_route` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_routeMany(items: ClearRouteArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the journeys public transport offered as the map's, and shows the first. */
  accept_transit(args: {
    journeys: Journey[];
    to_name: string;
  }): Promise<void>;
  /** Runs `accept_transit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  accept_transitMany(items: AcceptTransitArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows the next journey public transport offered. */
  another_journey(args?: {
    at?: number;
  }): Promise<void>;
  /** Runs `another_journey` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  another_journeyMany(items: AnotherJourneyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes the transit journeys away. */
  clear_transit(): Promise<void>;
  /** Runs `clear_transit` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_transitMany(items: ClearTransitArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Follows the route the map draws: the live card says the turn ahead. */
  start_navigation(): Promise<void>;
  /** Runs `start_navigation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  start_navigationMany(items: StartNavigationArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops following the route. */
  end_navigation(): Promise<void>;
  /** Runs `end_navigation` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  end_navigationMany(items: EndNavigationArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows what stands around the map, from OpenStreetMap. */
  find_nearby(): Promise<void>;
  /** Runs `find_nearby` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  find_nearbyMany(items: FindNearbyArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Shows a point of interest the map found, and opens the map on it. */
  open_poi(args: {
    poi: string;
  }): Promise<void>;
  /** Runs `open_poi` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  open_poiMany(items: OpenPoiArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Saves what the map shows for offline use: the box on screen and the zoom levels whose tiles it keeps. A caller names the box itself where it has one (a press, an agent): without one it is the place the map is centred on. */
  download_view(args?: {
    north?: number;
    south?: number;
    east?: number;
    west?: number;
    least_zoom?: number;
    most_zoom?: number;
  }): Promise<void>;
  /** Runs `download_view` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  download_viewMany(items: DownloadViewArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Saves the area around the place `query` names for offline use, with the zoom levels whose tiles it keeps. */
  download_place(args: {
    query: string;
    least_zoom?: number;
    most_zoom?: number;
  }): Promise<void>;
  /** Runs `download_place` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  download_placeMany(items: DownloadPlaceArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Keeps `area` on this device, replacing an area of the same footprint or name, and starts its download. */
  keep_area(args: {
    area: KeptArea;
  }): Promise<void>;
  /** Runs `keep_area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  keep_areaMany(items: KeepAreaArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts keeping `area`'s tiles, as a job whose record says how far it has come. A tile already kept is not fetched again, so this also continues a stopped area. */
  save_area(args: {
    area: KeptArea;
  }): Promise<void>;
  /** Runs `save_area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  save_areaMany(items: SaveAreaArgs[]): Promise<tilo.ActionAnswer[]>;
  /** What an area's download ended with: what it kept, or that the person stopped it. */
  area_saved(args: {
    area: KeptArea;
    j: unknown;
  }): Promise<void>;
  /** Runs `area_saved` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  area_savedMany(items: AreaSavedArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Stops an area's download: the tiles kept so far stay available offline. */
  stop_offline_area(args: {
    area: string;
  }): Promise<void>;
  /** Runs `stop_offline_area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  stop_offline_areaMany(items: StopOfflineAreaArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Starts a stopped area's download again, keeping what it already has. */
  continue_offline_area(args: {
    area: string;
  }): Promise<void>;
  /** Runs `continue_offline_area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  continue_offline_areaMany(items: ContinueOfflineAreaArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes an area this device saved away, and the tiles of it no other kept area needs. */
  delete_offline_area(args: {
    area: string;
  }): Promise<void>;
  /** Runs `delete_offline_area` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  delete_offline_areaMany(items: DeleteOfflineAreaArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Takes every area this device saved away. */
  clear_offline_areas(): Promise<void>;
  /** Runs `clear_offline_areas` for each of `items`: one all-or-nothing step per call, all of them one invocation group that History lists once and one undo takes back. */
  clear_offline_areasMany(items: ClearOfflineAreasArgs[]): Promise<tilo.ActionAnswer[]>;
  /** Opens a `geo:` link: its search (`geo:0,0?q=cafe`), else its point (`geo:52.52,13.40`). */
  open_link(args: {
    link: string;
  }): Promise<unknown>;
}

/** The actions `Atlas` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ATLAS_ACTIONS = ["forget_found", "forget_foundMany", "remember", "rememberMany", "take_in", "take_inMany", "accept_places", "accept_placesMany", "search", "searchMany", "clear_search", "clear_searchMany", "clear_recents", "clear_recentsMany", "show_directions", "set_directions", "open_place", "open_placeMany", "show_point", "show_pointMany", "locate", "locateMany", "zoom_map", "zoom_mapMany", "set_mode", "set_modeMany", "set_transit_time", "set_transit_timeMany", "directions", "directionsMany", "plan_transit", "plan_transitMany", "accept_route", "accept_routeMany", "another_route", "another_routeMany", "fit_map", "fit_mapMany", "clear_route", "clear_routeMany", "accept_transit", "accept_transitMany", "another_journey", "another_journeyMany", "clear_transit", "clear_transitMany", "start_navigation", "start_navigationMany", "end_navigation", "end_navigationMany", "find_nearby", "find_nearbyMany", "open_poi", "open_poiMany", "download_view", "download_viewMany", "download_place", "download_placeMany", "keep_area", "keep_areaMany", "save_area", "save_areaMany", "area_saved", "area_savedMany", "stop_offline_area", "stop_offline_areaMany", "continue_offline_area", "continue_offline_areaMany", "delete_offline_area", "delete_offline_areaMany", "clear_offline_areas", "clear_offline_areasMany", "open_link"] as const;

/** The `tl.so.tilo.maps.Atlas` kind at `path`. */
export function atlas(ws: tilo.Workspace, path: string): Atlas {
  return resourceProxy(ws, path, SURFACE, ATLAS_ACTIONS) as unknown as Atlas;
}

/** Scale — `tl.so.tilo.maps.Scale`. */
export interface Scale extends tilo.Resource {
  izgrst56: string;  // label
  o2aq6igf: number;  // pixels
}

/** The actions `Scale` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const SCALE_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.Scale` kind at `path`. */
export function scale(ws: tilo.Workspace, path: string): Scale {
  return resourceProxy(ws, path, SURFACE, SCALE_ACTIONS) as unknown as Scale;
}

/** Places — `tl.so.tilo.maps.Places`. */
export interface Places extends tilo.Resource {
  "734zo2xo": Place[];  // found
  "73qevr6q": string;  // why
}

/** The actions `Places` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const PLACES_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.Places` kind at `path`. */
export function places(ws: tilo.Workspace, path: string): Places {
  return resourceProxy(ws, path, SURFACE, PLACES_ACTIONS) as unknown as Places;
}

/** Routes — `tl.so.tilo.maps.Routes`. */
export interface Routes extends tilo.Resource {
  q2tnydqa: Route[];  // found
  kmhzghib: string;  // why
}

/** The actions `Routes` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const ROUTES_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.Routes` kind at `path`. */
export function routes(ws: tilo.Workspace, path: string): Routes {
  return resourceProxy(ws, path, SURFACE, ROUTES_ACTIONS) as unknown as Routes;
}

/** Journeys — `tl.so.tilo.maps.Journeys`. */
export interface Journeys extends tilo.Resource {
  ix3p2v56: Journey[];  // found
  cddmwu37: string;  // why
}

/** The actions `Journeys` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const JOURNEYS_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.Journeys` kind at `path`. */
export function journeys(ws: tilo.Workspace, path: string): Journeys {
  return resourceProxy(ws, path, SURFACE, JOURNEYS_ACTIONS) as unknown as Journeys;
}

/** Pois — `tl.so.tilo.maps.Pois`. */
export interface Pois extends tilo.Resource {
  fsnuwhad: Poi[];  // found
  "3yeey62c": string;  // why
}

/** The actions `Pois` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const POIS_ACTIONS = [] as const;

/** The `tl.so.tilo.maps.Pois` kind at `path`. */
export function pois(ws: tilo.Workspace, path: string): Pois {
  return resourceProxy(ws, path, SURFACE, POIS_ACTIONS) as unknown as Pois;
}

/** Maps — `tl.so.tilo.maps`. */
export interface Maps extends tilo.Resource {
}

/** The actions `Maps` reaches before it is read: `ws` calls them without a
  `load()` first. */
export const MAPS_ACTIONS = [] as const;

/** The `tl.so.tilo.maps` kind at `path`. */
export function maps(ws: tilo.Workspace, path: string): Maps {
  return resourceProxy(ws, path, SURFACE, MAPS_ACTIONS) as unknown as Maps;
}

