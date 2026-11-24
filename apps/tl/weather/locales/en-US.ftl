# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-weather = Weather

d-so-tilo-weather-keywords = weather, forecast, temperature, rain, wind, snow, wetter, vorhersage, temperatur, regen, schnee

d-ama7ascz = Hour
    .doc = One hour of the hours ahead, as the place's own clock reads it.
    .noun = { $count ->
        [one] hour
       *[other] hours
    }

d-hqg5l743 = At
    .doc = The place's local time, `2026-06-30T14:00`.

d-nz766xgs = Temperature

d-bszivu27 = Chance
    .doc = How likely rain is, in percent.

d-w4hqkb3u = Code
    .doc = The weather code the hour carries.

d-rx2vbln3 = Day
    .doc = Whether it is daylight then.

d-il4besep = Icon
    .doc = The glyph the hour shows, by the icon set's name.

d-ama7ascz-filter-temperature = Temperature

d-ama7ascz-filter-chance = Chance

d-ama7ascz-filter-code = Code

d-ama7ascz-filter-day = Day

d-i25z7rgj = Day
    .doc = One day of the week ahead.
    .noun = { $count ->
        [one] day
       *[other] days
    }

d-jpx34w56 = Date
    .doc = The day itself, `2026-06-30`.

d-7yqjq2lc = Weekday
    .doc = Its weekday, counted from Monday as 0: what names it once it is neither today nor tomorrow.

d-dyfvgxu3 = High

d-hgvg5w2j = Low

d-nle53w7c = Chance
    .doc = How likely rain is, in percent.

d-6nrssehp = Code

d-og6bqxmv = Wind

d-ttyi3z7y = Uv

d-zxdugvp7 = Rise
    .doc = The place's own sunrise and sunset, `2026-06-30T05:01`.

d-f2qdxlbk = Set

d-scsrpy5m = Icon

d-i25z7rgj-filter-weekday = Weekday

d-i25z7rgj-filter-high = High

d-i25z7rgj-filter-low = Low

d-i25z7rgj-filter-chance = Chance

d-i25z7rgj-filter-code = Code

d-i25z7rgj-filter-wind = Wind

d-i25z7rgj-filter-uv = Uv

d-rtf6mrzj = Reading
    .doc = What the sky is doing now, in the place's own day.
    .noun = { $count ->
        [one] reading
       *[other] readings
    }

d-wz5kgppi = At

d-nfn3o6kh = Temperature

d-npx3rbww = Apparent

d-adkhdcmi = Humidity

d-z25jhm5f = Precipitation

d-pexond7v = Code

d-zdiyx5q6 = Wind

d-tcofpdw2 = Wind from
    .doc = Where the wind comes from, in degrees.

d-ulscmhjp = Pressure

d-h2zd2igi = Uv

d-hilw2sek = Day

d-3imkwm63 = Icon

d-rtf6mrzj-filter-temperature = Temperature

d-rtf6mrzj-filter-apparent = Apparent

d-rtf6mrzj-filter-humidity = Humidity

d-rtf6mrzj-filter-precipitation = Precipitation

d-rtf6mrzj-filter-code = Code

d-rtf6mrzj-filter-wind = Wind

d-rtf6mrzj-filter-wind_from = Wind from

d-rtf6mrzj-filter-pressure = Pressure

d-rtf6mrzj-filter-uv = Uv

d-rtf6mrzj-filter-day = Day

d-5cdhibod = Air
    .doc = The air over a place.
    .noun = { $count ->
        [one] air
       *[other] airs
    }

d-qhxusqd7 = At

d-b43at2ai = European
    .doc = The European AQI (0–20 good … above 100 extremely poor).

d-rwv4mbrr = Us
    .doc = The United States AQI (0–500).

d-fpf3tzoo = Pm2 5

d-we4qtcij = Pm10

d-vjey5w7m = No2

d-5pnfxgbq = Ozone

d-5cdhibod-filter-european = European

d-5cdhibod-filter-us = Us

d-5cdhibod-filter-pm2_5 = Pm2 5

d-5cdhibod-filter-pm10 = Pm10

d-5cdhibod-filter-no2 = No2

d-5cdhibod-filter-ozone = Ozone

d-6hp3dzdj = Place
    .doc = One place a person follows, and what this device last read of it.
    .noun = { $count ->
        [one] place
       *[other] places
    }

d-bacjedt3 = Name

d-aohkzfz3 = Region
    .doc = The administrative region, when the search named one.

d-j6rsr6zo = Country

d-26l3gl47 = Country code

d-ax54oez6 = Latitude

d-ewmohhie = Longitude

d-y5lbxwn7 = Timezone
    .doc = The place's IANA zone, or `auto` when the search named none.

d-ikazv44r = Read
    .doc = Whether this device has read the place: what the readings below hold until it has.

d-gbrotevm = Current
    .doc = What the sky is doing there now.

d-qt6zc3me = Hours
    .doc = The hours ahead, as the device read them.

d-fjbo2bs7 = Days
    .doc = The week ahead.

d-qz7n6v2n = Air
    .doc = The air over the place.

d-uthtg3cy = Has air
    .doc = Whether the air service answered: its reading stands even when this is false.

d-42oai4b2 = Fetched
    .doc = When this device last read the place.

d-lkrdcuut = Stale
    .doc = The reading is older than the service's hour and no longer current.

d-ckqlainl = Trouble
    .doc = Why the last read failed, in words; empty while the place stands well.

d-goqo6yd3 = Refresh
    .doc = How far an update has got, and how the last one ended.
    .noun = { $count ->
        [one] refresh
       *[other] refreshes
    }

d-6a665z7q = State
    .doc = `running` while the device reads the places, and `done` once it has.

d-vawmqqsv = Waiting
    .doc = The places this update has still to read, by their ids.

d-umztfj5e = Done

d-6an5syvf = Total

d-lrbilhih = Failed
    .doc = How many places the last update could not read.

d-bf4xzg4n = Note
    .doc = Why it could not start, in words; empty otherwise.

d-goqo6yd3-filter-done = Done

d-goqo6yd3-filter-total = Total

d-goqo6yd3-filter-failed = Failed

d-wsn652ix = Found
    .doc = One place a search found.
    .noun = { $count ->
        [one] found
       *[other] founds
    }

d-mgz6jurw = Name

d-6jas4neh = Latitude

d-gvppngv7 = Longitude

d-kx6bm63e = Country

d-ubamq752 = Country code

d-a2wm7g5t = Region

d-5hvczxlz = Timezone

d-wsn652ix-filter-latitude = Latitude

d-wsn652ix-filter-longitude = Longitude

d-ibg56elx = Searching
    .doc = A person's own place search: what they typed, what the device found, and what it was asked for.
    .noun = { $count ->
        [one] searching
       *[other] searchings
    }

d-fmhvvp6l = Wanted
    .doc = What is in the field.

d-o4whnz6t = Asked
    .doc = What the device was last asked for: it looks when the two differ.

d-5o5rohu5 = Found

d-wgepvfwu = Note
    .doc = Why the search could not be read, in words; empty otherwise.

d-sevkk4o7 = Locations
    .doc = The places a person follows, the one a window shows first, and how each read stands.
    .noun = { $count ->
        [one] location
       *[other] locations
    }

d-b2qn6gqw = Places

d-vqfze2cq = Default
    .doc = The place a window shows first; none for the first in the list.

d-esvv4ymu = Refresh

d-ms5dkio6 = Searches
    .doc = Each person's own search here.

d-73zdj7wy = Today
    .doc = Today where the person is, `2026-06-30`: what a day of a forecast is named by.

d-rusdri25 = Tomorrow
    .doc = The day after today, as `today` is.

d-sevkk4o7-filter-default = Default

d-sevkk4o7-filter-refresh = Refresh

d-beqyw3ws = Save a place
    .doc = Saves a place a person follows.
    .did = saved a place
    .w-a-place-needs-a-name = A place needs a name
    .w-a-place-lies-between-latitudes-90 = A place lies between latitudes -90 and 90
    .w-a-place-lies-between-longitudes-180 = A place lies between longitudes -180 and 180

d-beqyw3ws-name = Name

d-beqyw3ws-latitude = Latitude

d-beqyw3ws-longitude = Longitude

d-beqyw3ws-region = Region

d-beqyw3ws-country = Country

d-beqyw3ws-country_code = Country code

d-beqyw3ws-timezone = Timezone

d-sqjw6zx2 = Show first
    .doc = Shows a place's forecast first whenever the app opens.
    .did = chose the place shown first

d-sqjw6zx2-place = Place

d-nf7ixh7u = Move to top
    .doc = Moves a place to the top of the list.
    .w-this-place-is-already-at-the = This place is already at the top

d-nf7ixh7u-place = Place

d-3a546tol = Remove place
    .doc = Removes a place from the list; what was read of it goes with it.
    .did = removed a place

d-3a546tol-place = Place

d-ndqwjxtw = Refresh forecasts
    .doc = Reads the forecasts again: every place, or the one `place` names.
    .did = updated the forecasts
    .w-there-are-no-places-to-update = There are no places to update
    .w-an-update-is-already-running = An update is already running

d-ndqwjxtw-place = Place

d-ebjgu5ua = Stop updating
    .doc = Stops an update that is running; the places it had not read keep what they had.

d-ctbwrcxw = Try again
    .doc = Updates a place whose last read failed, once the person asks again.
    .did = asked for a forecast again

d-ctbwrcxw-place = Place

d-mubgbm45 = Read next
    .doc = Reads one place an update has still to read, and says when the update has read them all.

d-mjt2zbkr = Read place
    .doc = Reads one place's forecast and air quality.

d-wm5pv5it = Apply forecast
    .doc = The forecast a service answered, written into the place: its current reading, a day of hours and a week of days.

d-wm5pv5it-body = Body

d-np2uffp7 = Poll weather
    .doc = Takes what the last read found: a place's reading and its hours and days land together, and the search's rows beside the field.
    .w-the-place-search-could-not-be = The place search could not be read: { $why }

d-fiih6vhd = Arrive
    .doc = Opening the window reads the places whose forecast is missing or no longer current.

d-cevyiepd = Look for a place
    .doc = Looks for places by name: the window asks the service once per query as it is typed.

d-jblkootf = End search
    .doc = Empties the field and stops looking.

d-jblkootf-reader = Reader

d-bplcjqoa = Add found
    .doc = Saves the place a search found, by its place in the list of what it found, and shows it.

d-bplcjqoa-reader = Reader

d-bplcjqoa-at = At

d-uekzr2wm = Accept found
    .doc = Takes the row a person picked from the search: saves the place and empties the search.

d-uekzr2wm-reader = Reader

d-uekzr2wm-choice = Choice

d-cnvt7jm5 = Refusal
    .w-the-weather-service-refused-the-request = The weather service refused the request (HTTP { $status })

d-lh3eadxk = Save place~present

d-q62lxozz = Move to top~present

d-b7ir6734 = Refresh~present

d-46yntdob = Hour tile
    .w-now = Now

d-md7zkdhz = Hours strip
    .w-no-hour-by-hour-forecast-yet = No hour-by-hour forecast yet.

d-5je2us5k = Day row
    .w-uv-wind = UV { $round } · wind { $round2 }
    .w-to-degrees = { $coldest } to { $warmest } degrees

d-muxhyqlx = Hero
    .w-high-low = High { $degrees }° · low { $degrees2 }°
    .w-out-of-date = Out of date

d-ejanp5lv = Readings
    .w-feels-like = Feels like
    .w-wind = Wind
    .w-humidity = Humidity
    .w-rain = Rain
    .w-pressure = Pressure
    .w-hpa = hPa
    .w-uv-index = UV index

d-xsxxrp64 = Air quality
    .w-air-quality = Air quality
    .w-us-aqi = US AQI
    .w-pm2-5 = PM2.5
    .w-pm10 = PM10
    .w-nitrogen-dioxide = Nitrogen dioxide
    .w-ozone = Ozone
    .w-no-air-quality-reading-for-this = No air-quality reading for this place.

d-jyf2fgwu = Update bar
    .w-updating-of-places = Updating { $done } of { $total } places
    .w-updating-the-forecasts = Updating the forecasts
    .w-stop = Stop
    .w-1-place-could-not-be-updated = 1 place could not be updated
    .w-places-could-not-be-updated = { $failed } places could not be updated
    .w-what-was-read-before-still-stands = What was read before still stands; try again later.
    .w-try-again = Try again

d-53e2mq4f = Place bar
    .w-refresh = Refresh
    .w-show-first = Show first
    .w-move-to-top = Move to top
    .w-remove-place = Remove place

d-gbetzns2 = Forecast
    .w-couldn-t-update-this-forecast = Couldn't update this forecast
    .w-try-again = Try again
    .w-daylight = Daylight
    .w-next-24-hours = Next 24 hours
    .w-the-week-ahead = The week ahead
    .w-air-quality = Air quality
    .w-reading-the-forecast = Reading the forecast…

d-b52eh6m4 = Weather window
    .w-weather = Weather
    .w-add-place = Add place
    .w-refresh = Refresh
    .w-places = Places
    .w-no-places-yet = No places yet
    .w-follow-a-city-or-town-to = Follow a city or town to see its forecast here.
    .w-choose-a-place = Choose a place
    .w-add-a-place = Add a place
    .w-search-for-a-city-or-town = Search for a city or town by name
    .w-cancel = Cancel
    .w-a-city-or-town = A city or town
