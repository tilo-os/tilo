# Maps — source locale (en-US).

## History: what each change to the shared map did

maps-place-saved = { $new ->
    [true] Added { $item } to the saved places
   *[false] Removed { $item } from the saved places
}
maps-place-name = Renamed { $item }
maps-place-category = Changed what { $item } is
maps-place-address = Changed the address of { $item }
maps-workspace-places = Changed the places
maps-workspace-mode = Set directions to travel { $new ->
    [car] by car
    [transit] by public transport
    [bike] by bike
    [foot] on foot
   *[other] { $new }
}
maps-workspace-transit-time = Changed when transit journeys leave or arrive
maps-accept-places = Took in places a search found
maps-commit-search = Searched for { $query }
maps-clear-recents = Cleared the recent searches
maps-accept-route = Planned a route to { $dest_name }
maps-clear-route = Cleared the route
maps-accept-transit = Planned transit journeys to { $dest_name }
maps-clear-transit = Cleared the transit options

## What failed, said to the person who asked (the transport's own text is
## logged, never shown: a URL, an errno and an HTTP status are ours to read)

maps-failure = Couldn't finish { $what }. { $reason }
maps-failure-unreachable = There is no connection to the map service. Check your connection and try again.
maps-failure-timeout = The map service took too long to answer. Try again.
maps-failure-too-many = The map service is limiting how often this device may ask. Try again in a moment.
maps-failure-service = The map service is having trouble. Try again in a moment.
maps-failure-refused = The map service did not accept that request. Try a different place or route.
maps-failure-other = Something went wrong between here and the map service. Try again.

## Where you are, what a search found, and what the map is doing

maps-use-my-location = Use my location
maps-locating = Locating you…
maps-location-unavailable = Location unavailable
maps-search-results = { $count } { $count ->
    [one] result
   *[other] results
 } for “{ $query }”
maps-search-no-results = No results for “{ $query }”
maps-route-updated = Route updated
maps-route-choose-destination = Choose a destination to see the route
maps-route-another = Another route
maps-route-number = Route { $number } of { $count }
maps-departures-updated = Departures updated
maps-no-transit = No transit options found
maps-offline-downloading = Downloading { $count } tiles for offline use…
maps-offline-downloading-named = Downloading { $count } tiles for { $name }…
maps-offline-stopped = Stopped the download; the tiles already saved stay available.

## Offline maps: the sheet that saves a piece of the map for later

maps-offline-title = Offline maps
maps-offline-area = Area to download
maps-offline-close = Close offline maps
maps-offline-search-placeholder = Search for a city or a place
maps-offline-zoom-levels = Zoom levels
maps-offline-fewer-zoom-levels = Fewer zoom levels
maps-offline-more-zoom-levels = More zoom levels
maps-offline-download-view = Download this view
maps-offline-download-named = Download { $name }
maps-offline-download-area = Download selected area
maps-offline-stop = Stop downloading
maps-offline-delete-area = Delete downloaded area
maps-offline-clear = Clear offline map cache
maps-offline-clear-short = Clear offline maps
maps-offline-downloaded-areas = Downloaded areas
maps-offline-empty = Search for a place, or download what the map shows now.
maps-offline-progress = Downloading { $done } of { $total } tiles…
maps-offline-stopped-count = Stopped after { $total } tiles; they stay available offline
maps-offline-done-failed = Downloaded { $total } tiles, { $failed } failed
maps-offline-done = Downloaded { $total } tiles
maps-offline-estimate = About { $size } to download
maps-offline-stored = { $size } on this device
maps-offline-refused = Places you've looked at stay on this device, so they still show offline.

## The map's own interface: labels a person reads or a screen reader
## announces, and the words the turn instructions are built from.

maps-app-name = Maps
maps-search-places = Search places
maps-expand-panel = Expand panel
maps-collapse-panel = Collapse panel
maps-back-to-places = Back to places
maps-travel-by = Travel by { $mode }
maps-open-place = Open place
maps-open-named-place = Open { $name }
maps-open-nearby-place = Open nearby place
maps-close-place-details = Close place details
maps-directions-to-place = Directions to this place
maps-search-again = Search again for { $query }
maps-clear-search-history = Clear search history
maps-hide-places = Hide places
maps-show-places = Show places
maps-directions = Directions
maps-transit = Transit
maps-save-or-unsave = Save or unsave place
maps-save = Save
maps-saved = Saved
maps-clear-route-button = Clear route
maps-leave-at = Leave at
maps-arrive-by = Arrive by
maps-earlier = Earlier
maps-later = Later
maps-reset-to-now = Reset to now
maps-journey-option = Journey option { $number }
maps-more-map-options = More map options
maps-history = History
maps-zoom-in = Zoom in
maps-zoom-out = Zoom out
maps-map-layers = Map layers
maps-end-navigation = End navigation
maps-no-traffic-data = No traffic data
maps-traffic = Traffic
maps-menu = Menu
maps-more = More
maps-active-navigation = Active navigation
maps-end = End
maps-continue-on-route = Continue on the route
maps-address = Address
maps-hours = Hours
maps-phone = Phone
maps-website = Website
maps-close = Close
maps-done = Done
maps-place-unavailable = Map place is no longer available
maps-poi-unavailable = Map point of interest is no longer in this viewport
maps-panel-empty = Search for a place or an address to see it here.
maps-panel-places = Places
maps-results = Results
maps-nearby = Nearby
maps-saved-places = Saved places
maps-recent-searches = Recent searches
maps-clear = Clear
maps-location-service-offline = The location service is unavailable in this session
maps-location-request-in-progress = A location request is already in progress
maps-hide-traffic = Hide traffic
maps-loading-places = Loading places…
maps-nearby-count = { $count } places nearby
maps-location-disconnected = The location service disconnected before returning a fix
maps-current-location = Current location
maps-located = Located
maps-map-centre = Map centre
maps-leave-now = Leave now
maps-routing = Routing…
maps-finding-departures = Finding departures…
maps-arrive-at = Arrive at { $time }
maps-leave-arrive-at = Leave at { $departure }, arrive at { $arrival }
maps-leave = Leave
maps-arrive = Arrive
maps-now = Now
maps-departures-to = Departures to { $to }
maps-your-destination = your destination
maps-choose-destination-departures = Choose a destination to see departures.
maps-no-public-transport = No public transport goes there at this time.
maps-direct = Direct
maps-changes = { $count } { $count ->
    [one] change
   *[other] changes
 }
maps-walk-minutes = Walk { $minutes } min
maps-walk-to = Walk to { $place }
maps-line-towards = { $line } towards { $headsign }
maps-stops = { $count } { $count ->
    [one] stop
   *[other] stops
 }
maps-minutes = { $count } min
maps-live = Live
maps-scheduled = Scheduled
maps-live-times = Live times
maps-scheduled-times = Scheduled times
maps-times-from = { $freshness } from { $agency } via Transitous
maps-times-via = { $freshness } via Transitous
maps-attribution = Attribution
maps-licenses = Licenses
maps-offline-maps = Offline maps
maps-attribution-body = Map data and map tiles © OpenStreetMap contributors.
maps-licenses-body = The map data is © OpenStreetMap contributors, available under the Open Database License (ODbL). The map tiles are OpenStreetMap Standard tiles, used under the OpenStreetMap Foundation's tile usage policy.
maps-imagery-unavailable = Map imagery can't be fetched here
maps-imagery-refused = The map tile server refused the request
maps-imagery-unreachable = The map tile server couldn't be reached. Trying again shortly.
maps-loading-map = Loading map…
maps-offline-ready = Offline area ready ({ $count } tiles)
maps-offline-area-missing = That offline area is not on this device
maps-offline-removed = Removed { $name }, freeing { $size }
maps-offline-cleared = Cleared { $size } of offline tiles
maps-no-places-found = No places found
maps-choose-start = Choose starting point
maps-choose-destination = Choose destination
maps-swap = Swap start and destination
maps-category-landmark = Landmark
maps-category-cafe = Café
maps-category-restaurant = Restaurant
maps-category-bar = Bar
maps-category-museum = Museum
maps-category-park = Park
maps-category-transit = Transit stop
maps-category-hotel = Hotel
maps-category-shop = Shop
maps-category-hospital = Hospital
maps-category-other = Place
maps-mode-drive = Drive
maps-mode-transit = Transit
maps-mode-bike = Cycle
maps-mode-foot = Walk
maps-transit-walk = Walk
maps-transit-bus = Bus
maps-transit-tram = Tram
maps-transit-subway = Metro
maps-transit-rail = Train
maps-transit-ferry = Ferry
maps-panel-explore = Explore
maps-panel-directions = Directions
maps-congestion-free = Free flowing
maps-congestion-moderate = Moderate
maps-congestion-heavy = Heavy
maps-congestion-standstill = Standstill
maps-remaining = { $distance } to go
maps-turn-start = Start
maps-turn-head-out = Head out on { $road }
maps-turn-arrive = Arrive at your destination
maps-turn-continue = Continue
maps-turn-continue-on = Continue on { $road }
maps-turn-left = Turn left
maps-turn-left-onto = Turn left onto { $road }
maps-turn-sharp-left = Sharp left
maps-turn-sharp-left-onto = Sharp left onto { $road }
maps-turn-slight-left = Slight left
maps-turn-slight-left-onto = Slight left onto { $road }
maps-turn-right = Turn right
maps-turn-right-onto = Turn right onto { $road }
maps-turn-sharp-right = Sharp right
maps-turn-sharp-right-onto = Sharp right onto { $road }
maps-turn-slight-right = Slight right
maps-turn-slight-right-onto = Slight right onto { $road }
maps-turn-uturn = Make a U-turn
maps-turn-roundabout = At the roundabout, take the exit
maps-turn-roundabout-onto = At the roundabout, take the exit onto { $road }
maps-what-search = searching
maps-what-route = planning the route
maps-what-transit = looking up departures
maps-what-nearby = loading nearby places
maps-route-estimate = { $reason } The offline estimate is shown.

maps-addresses-places = Addresses and places
