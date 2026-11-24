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

d-j4qvmrkj = Job
    .doc = The job the device runs, by its id; empty while nothing runs.

d-6a665z7q = State
    .doc = `running` while the device reads the places, and `done` once it has.

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
    .doc = A person's own place search: what they typed, what the device found, and the job it runs.
    .noun = { $count ->
        [one] searching
       *[other] searchings
    }

d-fmhvvp6l = Wanted
    .doc = What is in the field.

d-o4whnz6t = Asked
    .doc = What the device was last asked for: a job starts when the two differ.

d-xx2oi6v5 = Job
    .doc = The search job the device runs, by its id; empty while nothing runs.

d-cqmaksmt = Searching
    .doc = The device is looking now.

d-5o5rohu5 = Found

d-wgepvfwu = Note
    .doc = Why the search could not be read, in words; empty otherwise.

d-ibg56elx-filter-searching = Searching

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

d-dctjhygi = Ask
    .doc = The places one update is asked for.
    .noun = { $count ->
        [one] ask
       *[other] asks
    }

d-wf3zmolm = Places

d-f3wnxzqx = Ask place
    .noun = { $count ->
        [one] ask place
       *[other] ask places
    }

d-gdy7duy6 = Place id

d-5btxbecx = Latitude

d-dcshjxzm = Longitude

d-f3wnxzqx-filter-latitude = Latitude

d-f3wnxzqx-filter-longitude = Longitude

d-lfx3p224 = Search ask
    .doc = The search one job is asked for.
    .noun = { $count ->
        [one] search ask
       *[other] search asks
    }

d-4opjitfa = Query

d-elznullt = Take ask
    .doc = The job a take names.
    .noun = { $count ->
        [one] take ask
       *[other] take asks
    }

d-6vnqzh4l = Job

d-otchvpgk = Job
    .doc = How far a job has got.
    .noun = { $count ->
        [one] job
       *[other] jobs
    }

d-lwhvatgx = State

d-ka2h5wk7 = Done

d-lbqaguom = Total

d-otchvpgk-filter-done = Done

d-otchvpgk-filter-total = Total

d-hhwkcala = Taken
    .doc = What a job finished since it was last taken.
    .noun = { $count ->
        [one] taken
       *[other] takens
    }

d-gbqubsmc = Answers
    .doc = Each place's answer, as its own JSON text: a list of records is a list of resources in this language, and JSON text makes none, so each is read into its record here.

d-d4onixyo = Places
    .doc = Each place a search found, as its own JSON text.

d-ra4o5i2z = Error
    .doc = Why a search could not be read, in the engine's words.

d-sx243l4e = Answer
    .doc = One place's answer: its reading, the hours and days beside it as parallel lists, and its air reading.
    .noun = { $count ->
        [one] answer
       *[other] answers
    }

d-5prprzwo = Place id
    .doc = The place the answer is about, by its id; not `id`, which every record's is its own.

d-papzs3jy = Ok

d-2t7oi34p = Reason

d-opdrm37r = Status

d-oh3swlwe = Message

d-szxryqtq = Fetched
    .doc = When the device read it, in epoch seconds.

d-udln4y7a = Time

d-bt4m22bz = Temperature

d-5wz5vx2n = Apparent

d-kk67hrwa = Humidity

d-wssxi3hh = Precipitation

d-z2xmdn5s = Code

d-45ln3j6b = Wind

d-7sdt6b3s = Wind from

d-6pbvus4v = Pressure

d-7twngswb = Uv

d-gfeyv7li = Is day

d-do7cloel = Hour at

d-adqc7myi = Hour temp

d-m4tdmh3g = Hour chance

d-6w6giapb = Hour code

d-ercv5gi6 = Hour day

d-6vlbpjn2 = Day date

d-xpkpnifa = Day weekday

d-7mza74v3 = Day high

d-v5etkrnq = Day low

d-lsdyx245 = Day chance

d-hw7afts4 = Day code

d-rv4jwm5h = Day wind

d-eksw2wiu = Day uv

d-cl5k7le3 = Day rise

d-zjervfc5 = Day set

d-4kcw2w6l = Air ok

d-clhldsl6 = Air at

d-fxpfimss = Air european

d-zhjhkay7 = Air us

d-55ge75je = Air pm2 5

d-363wfecw = Air pm10

d-w77tgu5z = Air no2

d-qo2ngnib = Air ozone

d-sx243l4e-filter-ok = Ok

d-sx243l4e-filter-status = Status

d-sx243l4e-filter-fetched = Fetched

d-sx243l4e-filter-temperature = Temperature

d-sx243l4e-filter-apparent = Apparent

d-sx243l4e-filter-humidity = Humidity

d-sx243l4e-filter-precipitation = Precipitation

d-sx243l4e-filter-code = Code

d-sx243l4e-filter-wind = Wind

d-sx243l4e-filter-wind_from = Wind from

d-sx243l4e-filter-pressure = Pressure

d-sx243l4e-filter-uv = Uv

d-sx243l4e-filter-is_day = Is day

d-sx243l4e-filter-air_ok = Air ok

d-sx243l4e-filter-air_european = Air european

d-sx243l4e-filter-air_us = Air us

d-sx243l4e-filter-air_pm2_5 = Air pm2 5

d-sx243l4e-filter-air_pm10 = Air pm10

d-sx243l4e-filter-air_no2 = Air no2

d-sx243l4e-filter-air_ozone = Air ozone

d-f3sknmsd = Save a place
    .doc = Saves a place a person follows.
    .did = saved a place
    .w-a-place-needs-a-name = A place needs a name
    .w-a-place-lies-between-latitudes-90 = A place lies between latitudes -90 and 90
    .w-a-place-lies-between-longitudes-180 = A place lies between longitudes -180 and 180

d-f3sknmsd-name = Name

d-f3sknmsd-latitude = Latitude

d-f3sknmsd-longitude = Longitude

d-f3sknmsd-region = Region

d-f3sknmsd-country = Country

d-f3sknmsd-country_code = Country code

d-f3sknmsd-timezone = Timezone

d-xp2ceo5y = Show first
    .doc = Shows a place's forecast first whenever the app opens.
    .did = chose the place shown first

d-xp2ceo5y-place = Place

d-cirfp6lv = Move to top
    .doc = Moves a place to the top of the list.
    .w-this-place-is-already-at-the = This place is already at the top

d-cirfp6lv-place = Place

d-wqobjlmz = Remove place
    .doc = Removes a place from the list; what was read of it goes with it.
    .did = removed a place

d-wqobjlmz-place = Place

d-kl6crcze = Refresh forecasts
    .doc = Reads the forecasts again: every place, or the one `place` names.
    .did = updated the forecasts
    .w-there-are-no-places-to-update = There are no places to update
    .w-an-update-is-already-running = An update is already running
    .w-couldn-t-start-updating = Couldn't start updating: { $why }

d-kl6crcze-place = Place

d-7muahxxg = Stop updating
    .doc = Stops an update that is running.

d-ryiczm2w = Try again
    .doc = Updates a place whose last read failed, once the person asks again.
    .did = asked for a forecast again

d-ryiczm2w-place = Place

d-yajyk4wl = Poll weather
    .doc = Takes what the device has read so far: each place's answer as it lands, and what the service says of it.
    .w-the-update-stopped = The update stopped: { $why }
    .w-the-update-s-answer-did-not = The update's answer did not read: { $why }
    .w-a-place-s-answer-did-not = A place's answer did not read: { $why }
    .w-couldn-t-look-for-places = Couldn't look for places: { $why }
    .w-the-place-search-stopped = The place search stopped: { $why }

d-ig7wjove = Arrive
    .doc = Opening the window reads the places whose forecast is missing or no longer current.

d-lxgnovv7 = Look for a place
    .doc = Looks for places by name.

d-muewds7a = End search
    .doc = Empties the field and stops looking.

d-muewds7a-reader = Reader

d-6vszu5un = Add found
    .doc = Saves the place a search found, by its place in the list of what it found, and shows it.

d-6vszu5un-reader = Reader

d-6vszu5un-at = At

d-7zdozvog = Accept found
    .doc = Takes the row a person picked from the search: saves the place and empties the search.

d-7zdozvog-reader = Reader

d-7zdozvog-choice = Choice

d-hbcsago3 = Why answer
    .w-the-weather-service-refused-the-request = The weather service refused the request (HTTP { $status })

d-muq4w6r3 = Save place~present

d-o45nvib3 = Move to top~present

d-imdl6rzh = Refresh~present

d-46yntdob = Hour tile
    .w-now = Now

d-md7zkdhz = Hours strip
    .w-no-hour-by-hour-forecast-yet = No hour-by-hour forecast yet.

d-5je2us5k = Day row
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
