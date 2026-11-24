# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-maps = Maps

d-so-tilo-maps-keywords = map, navigation, directions, route, openstreetmap, osm, location, places, navigate, karte, karten, ort, orte, wege, standort

d-b5qgd2ou = Place
    .doc = A place the map knows: one a search found, one a person kept, one an agent accepted.
    .noun = { $count ->
        [one] place
       *[other] places
    }
    .untitled = Place

d-7omk6fxl = Name
    .doc = What people read it as.

d-jzfkismo = Key
    .doc = The name the map service gives it, so what a search finds twice is one place.

d-krefztsk = Category

d-oo5cyi2s = Address
    .doc = Its address, as the service writes it.

d-if6ecymb = Latitude

d-xj2tbyfv = Longitude

d-yb5xojzc = Saved
    .doc = Whether the person kept it in their places.

d-xrhsgfmm = Found
    .doc = Whether the last search found it, rather than the person having kept it.

d-b5qgd2ou-filter-saved = Saved

d-b5qgd2ou-filter-category = Category

d-nn64d3fw = Step
    .doc = One turn of a route, in the order the person takes them.
    .noun = { $count ->
        [one] step
       *[other] steps
    }

d-jslvd2zo = Instruction

d-bx4d2y62 = Distance m

d-2wu6uhb6 = Maneuver

d-nn64d3fw-filter-distance_m = Distance m

d-nn64d3fw-filter-maneuver = Maneuver

d-rvxurton = Route
    .doc = A route as directions found it: how far, how long, the line to draw, and its turns.
    .noun = { $count ->
        [one] route
       *[other] routes
    }

d-w2mcf323 = Mode

d-xl7mdkpo = Distance m

d-rdp44rmk = Duration s

d-uyrheaim = Geometry
    .doc = The places the line runs through, latitude then longitude in turn.

d-fyf6eckz = From name

d-bwd4zoxi = To name

d-yufxlbcl = From latitude

d-ws5q4ft3 = From longitude

d-pepwxqg3 = To latitude

d-3lxyvouf = To longitude

d-ql4xlomv = Steps

d-rvxurton-filter-mode = Mode

d-rvxurton-filter-distance_m = Distance m

d-rvxurton-filter-duration_s = Duration s

d-rvxurton-filter-from_latitude = From latitude

d-rvxurton-filter-from_longitude = From longitude

d-rvxurton-filter-to_latitude = To latitude

d-rvxurton-filter-to_longitude = To longitude

d-5ydr2uej = Leg
    .doc = One leg of a public transport journey.
    .noun = { $count ->
        [one] leg
       *[other] legs
    }

d-3hgwl46g = Mode

d-oivvgw5v = Line

d-dk4ne74p = Headsign

d-sd7j2mow = From name

d-zsmxifsc = To name

d-4pplnxwl = Dep

d-a6ukygsr = Arr

d-agzaego5 = Duration min

d-ptjnyoqs = Num stops

d-i5zg4biq = Colour

d-g4fcmpin = Text colour

d-rkrkjx3z = Agency

d-7wrlvcuf = Realtime
    .doc = Whether the times are the service's live times rather than its timetable.

d-5ydr2uej-filter-mode = Mode

d-5ydr2uej-filter-duration_min = Duration min

d-5ydr2uej-filter-num_stops = Num stops

d-5ydr2uej-filter-realtime = Realtime

d-iapaez57 = Journey
    .doc = A journey public transport offers from one place to another.
    .noun = { $count ->
        [one] journey
       *[other] journeys
    }

d-xjeu7nif = Dep

d-bhvk42wt = Arr

d-sgpde2bq = Duration min

d-ixfei2g5 = Transfers

d-ica6larv = Legs

d-iapaez57-filter-duration_min = Duration min

d-iapaez57-filter-transfers = Transfers

d-unf4oqsm = Transit time
    .doc = When a person's transit journeys leave, or when they must arrive.
    .noun = { $count ->
        [one] transit time
       *[other] transit times
    }

d-bv3pklkc = Arrive by

d-5hirpmkr = Offset min
    .doc = Minutes from now, or before now for an arrival.

d-unf4oqsm-filter-arrive_by = Arrive by

d-unf4oqsm-filter-offset_min = Offset min

d-hnngdhyc = Poi
    .doc = A point of interest standing in what the person looks at, from OpenStreetMap.
    .noun = { $count ->
        [one] poi
       *[other] pois
    }

d-kghw4vpb = Name

d-osqgf2vf = Key

d-ndtgscii = Category

d-ilnfptos = Latitude

d-fdly24ym = Longitude

d-pdpenkqy = Address

d-vraa6lbe = Hours

d-t3erfdxp = Website

d-hxlh5wj4 = Phone

d-gdtnxbln = Kind
    .doc = What kind of thing it is, in its own words ("museum", "italian").

d-hnngdhyc-filter-category = Category

d-hnngdhyc-filter-latitude = Latitude

d-hnngdhyc-filter-longitude = Longitude

d-hyiwxese = Recent
    .doc = One search a person made, newest first: what the panel offers before they type.
    .noun = { $count ->
        [one] recent
       *[other] recents
    }

d-dozhur5v = Query

d-eyiulpwh = At

d-hyiwxese-filter-at = At

d-gijd5vfp = Offline area
    .doc = An area of the map this device keeps for offline use: its bounds, the zoom levels whose tiles it keeps, and how far saving it has come.
    .noun = { $count ->
        [one] offline area
       *[other] offline areas
    }

d-q2xakaa7 = Name

d-4ntlybip = North

d-2oacylth = South

d-mvzekw5x = East

d-j2r3s5ch = West

d-mmthjc2s = Least zoom

d-cmuxwz44 = Most zoom

d-3mucu3ea = Tiles
    .doc = The tiles the area holds, as the map counts them.

d-ueltqy2u = Done
    .doc = How many of them are kept on the device.

d-xlmztacf = Atlas
    .doc = The workspace's map: the places it knows, the searches it made, the directions it found and the areas this device keeps offline.
    .noun = { $count ->
        [one] atla
       *[other] atlas
    }

d-q3ezxhmv = Places
    .doc = Every place the map knows: the ones the person kept, and the ones a search found.

d-vaus6gd3 = Recents
    .doc = The searches the person made, newest first.

d-bdrkt5dv = Pois
    .doc = The points of interest standing around what the map shows.

d-sxa3os5i = Routes
    .doc = The routes directions found, in the order they were offered.

d-ia244ab6 = Route at
    .doc = Which of the routes the map draws.

d-xbpiwexi = Journeys
    .doc = The journeys public transport offers, from the last time it was asked.

d-aok7qtg7 = Journey at
    .doc = Which journey the map draws.

d-jzvaaswo = Transit time
    .doc = When transit journeys leave, or when they must arrive.

d-ywuwhj6m = Mode
    .doc = How directions travel.

d-7i6mpbfw = Query
    .doc = The last search's words, so the panel says what it shows.

d-onljlflv = Status
    .doc = What the map is doing, or why what was asked did not happen.

d-woyw2cxx = Centre latitude
    .doc = The place the map opens on, and returns to.

d-zwui54r7 = Centre longitude

d-4azkiaic = Centre zoom
    .doc = How close the map opens: the tile service's own zoom levels.

d-qobzxkm4 = Traffic
    .doc = Whether the traffic layer is on.

d-d54rah5t = Navigating
    .doc = Whether the person is following a route: the live card says the turn ahead.

d-c6himdmu = Offline
    .doc = The areas this device keeps for offline use.

d-xlmztacf-filter-traffic = Traffic

d-ahqfw7eh = Places
    .doc = What a geocoder's answer holds: the places it found, or why it has none.
    .noun = { $count ->
        [one] place
       *[other] places
    }

d-734zo2xo = Found

d-73qevr6q = Why

d-d3y2ubxi = Routes
    .doc = What a routing service's answer holds: the routes it found, or why it has none.
    .noun = { $count ->
        [one] route
       *[other] routes
    }

d-q2tnydqa = Found

d-kmhzghib = Why

d-b7n4w3zo = Journeys
    .doc = What a transit service's answer holds: the journeys it found, or why it has none.
    .noun = { $count ->
        [one] journey
       *[other] journeys
    }

d-ix3p2v56 = Found

d-cddmwu37 = Why

d-lqxwwiug = Pois
    .doc = What a map opened on a place holds: the points of interest around it, or why it has none.
    .noun = { $count ->
        [one] pois
       *[other] poises
    }

d-fsnuwhad = Found

d-3yeey62c = Why

d-pnt3r5fh = Category
    .doc = What a place is: the marker's ink, the card's glyph, and the OpenStreetMap class it maps from.

d-upzs3qkn = A landmark, a monument, a sight
    .doc = A landmark, a monument, a sight.

d-pkrsaacd = A café
    .doc = A café.

d-mpazezbv = A restaurant, a fast food place
    .doc = A restaurant, a fast food place.

d-3e5q2ijn = A bar or a pub
    .doc = A bar or a pub.

d-kxi5h2rb = A museum or a gallery
    .doc = A museum or a gallery.

d-bxmkupyx = A park or a garden
    .doc = A park or a garden.

d-4zknrelh = A stop, a station, a bus stop
    .doc = A stop, a station, a bus stop.

d-dwct4abs = A hotel or a hostel
    .doc = A hotel or a hostel.

d-gfmzi3qr = A shop
    .doc = A shop.

d-jipcl6u3 = A hospital, a clinic, a doctor
    .doc = A hospital, a clinic, a doctor.

d-wxxxvca4 = Anything else
    .doc = Anything else.

d-33wxlq7h = Travel mode
    .doc = How directions travel.

d-pjkvdmwk = By car
    .doc = By car.

d-zitymayc = By public transport
    .doc = By public transport.

d-lfa3rl6s = By bike
    .doc = By bike.

d-vseev2rl = On foot
    .doc = On foot.

d-g476xvac = Maneuver
    .doc = A turn of a route.

d-e3tczbcu = Depart

d-mnbjennf = Continue on

d-xwhxaimf = Slight left

d-ni4lcmis = Left

d-k3lu4ge3 = Sharp left

d-m2ye55z2 = Slight right

d-jt4au2t2 = Right

d-33tyunh6 = Sharp right

d-aymgcjsx = Uturn

d-qivd66vw = Roundabout

d-wyyyf2ov = Merging

d-sdo2uhip = Arrive

d-pkrgpzz5 = Transit mode
    .doc = The way one leg of a public transport journey travels.

d-fraiou23 = Walk

d-qvohweza = Bus

d-jjrvri4g = Tram

d-xxr3zm5d = Subway

d-dgcc5jwo = Rail

d-d4iv5agm = Ferry

d-zahh2cbi = Forget the places a search found
    .doc = Marks every place a search found as no longer found, and forgets the ones nobody kept.

d-bcc6aj7f = Remember a search
    .doc = Remembers a search, newest first, at most eight.

d-bcc6aj7f-query = Query

d-dgdwvxgb = Take in places
    .doc = Adds the places a search found to the map, keeping one the person already kept as theirs.

d-dgdwvxgb-places = Places

d-efzqdr6v = Accept places
    .doc = Takes the places another app or an agent found into the map.
    .did = took in places a search found

d-efzqdr6v-places = Places

d-zikhmvg4 = Search maps
    .doc = Searches the map for the places `query` names, and shows what it found.
    .did = changed the map

d-zikhmvg4-query = Query

d-y3g4r54a = Clear search
    .doc = Takes the last search's results away.

d-ox6t7ge3 = Clear search history
    .doc = Takes the searches the person made away.
    .did = cleared the recent searches

d-25erw4bh = Save place
    .doc = Keeps a place in the person's places, or takes it out.

d-25erw4bh-saved = Saved

d-molc2lgo = Open place
    .doc = Shows the place `place` on the map: the map opens on it.
    .did = changed the map

d-molc2lgo-place = Place

d-iztax2no = Show a point on the map
    .doc = Shows a point on the map, as a link or another app names it.
    .did = showed a point on the map

d-iztax2no-latitude = Latitude

d-iztax2no-longitude = Longitude

d-3hjskyvu = Use my location
    .doc = Finds the device and shows where it is.
    .did = found where the device is

d-4mqy7nqs = Zoom the map
    .doc = Zooms the map a level in or out.

d-4mqy7nqs-direction = Direction

d-4mqy7nqs-zoom = Zoom

d-7axqlj5f = Set mode
    .doc = How the map's directions travel.
    .did = changed the map

d-7axqlj5f-mode = Mode

d-hdtdbrvo = Set transit time
    .doc = When public transport journeys leave, or when they must arrive.
    .did = changed when transit journeys leave or arrive

d-hdtdbrvo-arrive_by = Arrive by

d-hdtdbrvo-offset_min = Offset min

d-q77k3ou3 = Get directions
    .doc = Plans a route between two places the person named, by the mode directions travel, and shows it.
    .did = changed the map

d-q77k3ou3-from_words = From words

d-q77k3ou3-to_words = To words

d-bi2r2qga = Departures
    .doc = Finds the journeys public transport offers between two places.
    .did = changed the map

d-bi2r2qga-from = From

d-bi2r2qga-to = To

d-2wnunnrv = Accept route
    .doc = Takes the routes directions found as the map's, and shows the first.
    .did = changed the map

d-2wnunnrv-routes = Routes

d-2wnunnrv-from_name = From name

d-2wnunnrv-to_name = To name

d-u6n4c2ot = Another route
    .doc = Shows the next route directions offered for the same journey.

d-u6n4c2ot-at = At

d-r6jn5iso = Clear route
    .doc = Takes the route away.
    .did = cleared the route

d-o44pvrkh = Accept transit
    .doc = Takes the journeys public transport offered as the map's, and shows the first.
    .did = changed the map

d-o44pvrkh-journeys = Journeys

d-o44pvrkh-to_name = To name

d-q7ri6eap = Another journey
    .doc = Shows the next journey public transport offered.

d-q7ri6eap-at = At

d-a7wcssio = Clear transit routes
    .doc = Takes the transit journeys away.
    .did = cleared the transit options

d-veqhdpil = Start navigation
    .doc = Follows the route the map draws: the live card says the turn ahead.

d-mfa5tjvv = End navigation
    .doc = Stops following the route.

d-vmhadutz = What is nearby
    .doc = Shows what stands around the map, from OpenStreetMap.
    .did = looked for what is nearby

d-brot5bcq = Open place of interest
    .doc = Shows a point of interest the map found, and opens the map on it.

d-brot5bcq-poi = Poi

d-mldvi5ic = Show traffic
    .doc = Shows the traffic layer on the map, or takes it away.
    .did = turned the traffic layer on or off

d-mldvi5ic-traffic = Traffic

d-hvjfoctq = Download this view
    .doc = Saves what the map shows for offline use: its box and the zoom levels whose tiles it keeps.
    .did = saved what the map shows for offline use

d-hvjfoctq-least_zoom = Least zoom

d-hvjfoctq-most_zoom = Most zoom

d-j55kqk7i = Download place
    .doc = Saves the area `query` names for offline use, with the zoom levels whose tiles it keeps.
    .did = changed the map

d-j55kqk7i-query = Query

d-j55kqk7i-least_zoom = Least zoom

d-j55kqk7i-most_zoom = Most zoom

d-4sv7or3w = Note saved
    .doc = Says how far saving an area has come, as the map reports it.
    .did = changed the map

d-4sv7or3w-area = Area

d-4sv7or3w-at = At

d-sl5lylwr = Delete offline area
    .doc = Takes an area this device saved away.
    .did = changed the map

d-sl5lylwr-area = Area

d-234blzdd = Clear offline maps
    .doc = Takes every area this device saved away.
    .did = cleared the offline maps

d-zbnit5ey = Open link
    .doc = Opens a `geo:` link: its search (`geo:0,0?q=cafe`), else its point (`geo:52.52,13.40`).

d-zbnit5ey-link = Link

d-sszbhqmi = Save place~present

d-5v5x4tkt = Toggle traffic~present

d-oeeojlox = Map
    .w-search-places = Search places
    .w-results = Results
    .w-nearby = Nearby
    .w-saved = Saved
    .w-recent = Recent
    .w-search-for-a-place-or-an = Search for a place or an address to see it here.
    .w-no-places-found = No places found
    .w-nothing-nearby-yet = Nothing nearby yet
    .w-look-for-what-stands-around-the = Look for what stands around the map.
    .w-no-saved-places-yet = No saved places yet
    .w-save-a-place-to-find-it = Save a place to find it here.
    .w-no-recent-searches-yet = No recent searches yet
    .w-recent-searches = Recent searches
    .w-clear = Clear
    .w-map = Map
    .w-https-tile-openstreetmap-org-z-x = https://tile.openstreetmap.org/%z/%x/%y.png
    .w-search-for-a-place-to-see = Search for a place to see it on the map
    .w-zoom-in = Zoom in
    .w-zoom-out = Zoom out
    .w-use-my-location = Use my location
    .w-what-is-nearby = What is nearby
    .w-show-traffic = Show traffic
    .w-offline-maps = Offline maps
    .w-attribution = Attribution
    .w-dismiss = Dismiss
    .w-close-place-details = Close place details
    .w-save = Save
    .w-directions = Directions
    .w-close-offline-maps = Close offline maps
    .w-fewer-zoom-levels = Fewer zoom levels
    .w-zoom-levels = Zoom levels { $least_zoom }–{ $most_zoom }
    .w-more-zoom-levels = More zoom levels
    .w-download-this-view = Download this view
    .w-search-for-a-city-or-a = Search for a city or a place
    .w-download-place = Download place
    .w-search-for-a-place-or-download = Search for a place, or download what the map shows now.
    .w-downloaded-areas = Downloaded areas
    .w-clear-offline-maps = Clear offline maps
    .w-done = Done
    .w-map-data-and-map-tiles-openstreetmap = Map data and map tiles © OpenStreetMap contributors.
    .w-the-map-data-is-openstreetmap-contributors = The map data is © OpenStreetMap contributors, available under the Open Database License (ODbL). The map tiles are OpenStreetMap Standard tiles, used under the OpenStreetMap Foundation's tile usage policy.

d-sb4mqylm = Place acts
    .w-saved = Saved
    .w-save = Save
    .w-directions-to-this-place = Directions to this place

d-rfg2gu4h = Poi row
    .w-directions-to-this-place = Directions to this place

d-dnrz2rhj = Footer
    .w-openstreetmap-contributors = © OpenStreetMap contributors
    .w-no-traffic-data = No traffic data
    .w-zoom = Zoom { $round }

d-zrj5mbzs = Directions panel
    .w-directions = Directions
    .w-to = to { $to_name }
    .w-another-route = Another route
    .w-route-of = Route { $value } of { $len }
    .w-end-navigation = End navigation
    .w-start-navigation = Start navigation
    .w-clear-route = Clear route
    .w-turns = Turns
    .w-open-a-place-and-ask-for = Open a place and ask for directions to it.

d-fxxzz6k4 = Transit panel
    .w-leave-at = Leave at
    .w-arrive-by = Arrive by
    .w-earlier = Earlier
    .w-min = { $abs } min
    .w-later = Later
    .w-direct = Direct
    .w-changes = { $transfers } changes
    .w-another-journey = Another journey
    .w-clear-transit = Clear transit
    .w-choose-a-place-to-travel-to = Choose a place to travel to

d-tqnx77nc = Leg row
    .w-walk-to = Walk to { $to_name }
    .w-towards = { $line } towards { $headsign }

d-3p2cxc7k = Area row
    .w-of-tiles = { $done } of { $tiles } tiles
    .w-delete-downloaded-area = Delete downloaded area

d-up2iielj = Turn card
    .w-to-go = { $distance_text } to go
    .w-continue-on-the-route = Continue on the route
    .w-end = End

d-ex73ittg = Live
    .w-continue-on-the-route = Continue on the route
    .w-end = End

d-wjvabv4j = Place
    .w-saved = Saved
    .w-save = Save
    .w-directions = Directions
    .w-open-on-the-map = Open on the map
