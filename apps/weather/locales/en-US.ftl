## History

weather-save-location = Saved a place to follow
weather-use-as-default = Made { $item } the default place
weather-move-to-top = Moved { $item } to the top
weather-remove = Removed { $item }
weather-weather-places = { $op ->
    [move-to] Reordered the places
    [insert] Added a place
   *[other] Changed the places
}
weather-weather-default-place = Changed the default place
weather-place-name = Renamed { $item }

weather-morning-summary = { $temperature }, { $condition } · observation { $time }
weather-morning-source = Open-Meteo · fetched { $fetched }
weather-morning-fixture = Capture fixture
weather-morning-stale = Last saved observation
weather-morning-freshness = Provider observation


# The forecast page's own copy, through the toolkit's catalogue.

# The 24-hour strip and the 7-day forecast, as assistive technology reads
# them: one row per hour and per day, carrying the reading the strip paints.
weather-hours = Next 24 hours
weather-days = 7-day forecast
weather-hour-now = Now
weather-summary = { $temperature }, { $condition }, feels like { $feels }. { $hours } hourly readings and { $days } daily forecasts follow.
weather-hour-row = { $temperature }, { $sky }
weather-hour-row-rain = { $temperature }, { $sky }, { $chance } chance of precipitation
weather-day-row = { $high } to { $low }, { $sky }
weather-day-row-rain = { $high } to { $low }, { $sky }, { $chance } chance of precipitation
weather-day-extras = UV index { $uv }, wind up to { $wind }

# The unit a precipitation reading is given in, as the tiles show it.
weather-unit-millimetres = mm
weather-unit-inches = in

# How old the forecast on screen is. $when is a relative age ("5 min ago"),
# the clock time it was fetched at ("14:05"), or its date and time
# ("Jun 30, 2026, 14:05").
weather-updated-ago = Updated { $when }
weather-updated-clock = Updated at { $when }
weather-updated-date = Updated { $when }
weather-outdated-ago = Last updated { $when }, out of date
weather-outdated-clock = Last updated at { $when }, which is out of date
weather-outdated-date = Last updated { $when }, which is out of date
