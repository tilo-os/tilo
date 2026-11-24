# Maps: what the Rust app did, and where the tilolang app does it

The Rust app under `apps/maps` (`maps`, `maps-model`, `maps-model-wasm`), its tests and its
manifest are deleted in the same push that lands this package. Nothing is dropped: every row
below is done by `apps/tl/maps/app.tl`, and the row names the test that proves it
(`crates/lang/tilolang-runtime/tests/e2e_maps.rs`).

## Resources

| Was | Is |
|---|---|
| `maps.workspace`: places (`Keyed<Place>`), recents (capped 8), travel mode, transit time, the accepted route plan and transit plan | `Atlas`: `places`, `recents` (capped 8), `mode`, `transit_time`, `routes` + `route_at`, `journeys` + `journey_at`, plus `pois`, `query`, `status`, the centre, `traffic`, `navigating`, `offline` |
| Saved places as a search scope | `Place.saved`, and the type's own `search` filters (`saved`, `category`) so the system's search finds them |
| `maps.offline-areas`: the areas each device downloaded, kept in the person's own state | `Atlas.offline`: the areas the workspace keeps; the tiles themselves stay the device's, in the window's picture cache (`crates/engines/tilo-tiles`) |

## Actions (published verbs)

| Was | Is | Test |
|---|---|---|
| `accept-places` | `accept_places` | `a_search_takes_in_the_places_it_found` |
| `commit-search` | `search` (and `search_again`) | `a_search_takes_in_the_places_it_found` |
| `clear-recents` | `clear_recents` | `recent_searches_keep_the_newest_eight_and_clear` |
| `accept-route` | `accept_route` | `directions_find_a_route_with_its_turns_and_another_one` |
| `clear-route` | `clear_route` | `directions_find_a_route_with_its_turns_and_another_one` |
| `accept-transit` | `accept_transit` | `directions_by_public_transport_find_journeys` |
| `clear-transit` | `clear_transit` | `directions_by_public_transport_find_journeys` |
| `search` | `search` | `a_search_takes_in_the_places_it_found` |
| `locate` | `locate` (device work through `engine.call("maps.locate")`) | `the_device_says_where_it_is_and_no_where_it_cannot` |
| `toggle-traffic` | `toggle_traffic` | `the_traffic_layer_toggles` |
| `open-place` | `open_place` | `a_search_takes_in_the_places_it_found` |
| `directions` | `directions` | `directions_find_a_route_with_its_turns_and_another_one` |
| `open-poi` | `open_poi` | `nearby_takes_in_the_places_that_stand_around_and_opens_one` |
| `poi-directions` | `poi_directions` | `nearby_takes_in_the_places_that_stand_around_and_opens_one` |
| `show-point` | `show_point` | `a_point_shows_where_it_is_and_one_off_earth_does_not` |
| `delete-offline-area` | `delete_offline_area` | `an_offline_area_counts_its_tiles_and_says_how_far_it_got` |
| `clear-offline-areas` | `clear_offline_areas` | `an_offline_area_counts_its_tiles_and_says_how_far_it_got` |
| `open-link` (a `geo:` link) | `open_link` | `a_geo_link_searches_or_shows_its_point` |
| the `saved` field's edit | `save_place` | `a_search_replaces_the_last_ones_and_keeps_what_was_saved` |
| — (new, for agents and other apps) | `another_route`, `another_journey`, `set_mode`, `set_transit_time`, `find_nearby`, `download_view`, `download_place`, `note_saved`, `zoom_map`, `start_navigation`, `end_navigation` | the tests named above |

## Windows and features

| Was | Is | Test |
|---|---|---|
| A full-bleed map with OSM raster tiles, zoom and pan | `ui.Map(world: true, basemap: "https://tile.openstreetmap.org/%z/%x/%y.png")`; the shared map widget draws, caches and falls back to a coarser tile | `the_window_draws_the_map_with_its_imagery_markers_and_route` |
| Category-coloured markers, the selected one ringed, press to open | `ui.MapMarker(kind: …)`, the marker's `press` opens the place | `the_window_draws_the_map_with_its_imagery_markers_and_route` |
| The route drawn as a polyline | `ui.MapTrack(points: …)` | same |
| The places panel: search field, results, nearby points of interest, saved places, recent searches with Clear | `places_side`, `place_row`, `poi_row`; the system's `ui.SearchButton` opens search scoped to the app | `the_window_draws_the_map_with_its_imagery_markers_and_route` |
| The place card for the selected place | `place_card` | same |
| Directions: mode, summary, another route, turns, clear, start/end navigation | `directions_panel`, `turn_card` | `directions_find_a_route_with_its_turns_and_another_one`, `navigation_follows_the_route_and_ends` |
| Transit: leave at/arrive by, earlier/later, journeys and their legs, another journey, clear | `transit_panel`, `leg_row` | `directions_by_public_transport_find_journeys`, `the_time_transit_asks_for_moves_the_departures` |
| Controls: zoom in/out, use my location, traffic, nearby, offline maps, attribution | `map_controls` | `the_window_draws_the_map_with_its_imagery_markers_and_route` |
| Footer: OpenStreetMap credit, scale/zoom, “No traffic data” | `footer` | same |
| Offline maps sheet: zoom levels, download this view, download a place, the areas and their progress, delete, clear | `offline_sheet`, `area_row`; the map asks for the areas' tiles a few at a time and reports progress (`saved`) | `an_offline_area_counts_its_tiles_and_says_how_far_it_got` |
| Attribution and licences sheet | `info_dialog` | `the_window_draws_the_map_with_its_imagery_markers_and_route` |
| Live route activity (turn ahead, End) | `view live(a: Atlas)` with `live when a.navigating` | `navigation_follows_the_route_and_ends` |
| Another person's view at a place | the saved places list shows who else is at each place (`ui.ListItem(presence: …)`) | — (presence is the store's; the row passes it) |
| Failure words for no connection, a busy or limiting service, a refused request | `connection_word` | `a_failed_request_says_why_in_words` |
| History and undo of the map's changes | system history and undo over the `Atlas` resource | — |
| Search in the desktop's palette for the app's places | the types' `search` blocks index every place; `SearchButton` opens the system search scoped to the app | — |
| A `geo:` link | the app declares `schemes: "geo"` and publishes `open_link` | `a_geo_link_searches_or_shows_its_point` |
