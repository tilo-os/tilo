# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-clocks = Clocks

d-fncbrtzl = World clock
    .doc = A city whose time a person compares with their own.
    .noun = { $count ->
        [one] world clock
       *[other] world clocks
    }
    .untitled = City

d-kbeao6ct = City
    .doc = What the person calls the clock: the city's name.

d-g5vsvo2p = Zone
    .doc = The IANA zone it keeps time by, such as `Asia/Tokyo`.

d-lqrbc6rz = Alarm
    .doc = An alarm: a time of day, the days it repeats on, and where it rings.
    .noun = { $count ->
        [one] alarm
       *[other] alarms
    }
    .untitled = Alarm

d-e6jhyy6h = Label

d-cb6boqqg = Hour
    .doc = The hour it rings at, in the person's zone.

d-bvmw7lxl = Minute
    .doc = The minute it rings at.

d-s4ppnbtv = Days
    .doc = The weekdays it repeats on; none rings once, at the next such time.

d-adpv3jco = Enabled
    .doc = Whether it rings at all.

d-fbxt6ruu = At
    .doc = The instant it next rings, which the app keeps as the trigger reads it; none while it is off.

d-3bdkffqt = Rings on
    .doc = The device it rings on, by its id; empty rings on every device of the person's.

d-uizteftt = Snoozed
    .doc = Whether the ring at `at` is a snooze: a repeating alarm snoozed rings again and carries on with its days.

d-lqrbc6rz-filter-hour = Hour

d-lqrbc6rz-filter-minute = Minute

d-lqrbc6rz-filter-days = Days

d-lqrbc6rz-filter-enabled = Enabled

d-lqrbc6rz-filter-at = At

d-lqrbc6rz-filter-snoozed = Snoozed

d-oprnqjx3 = Timer
    .doc = A timer: a countdown that runs to zero and rings there.
    .noun = { $count ->
        [one] timer
       *[other] timers
    }
    .untitled = Timer

d-nhqnhguz = Label

d-5b7nw57j = Seconds
    .doc = How long it counts down, in seconds.

d-6anly4z4 = Ends at
    .doc = The instant it finishes; none while it is paused.

d-ufpse6mi = Left
    .doc = What it has left while it is paused, in seconds.

d-oprnqjx3-filter-seconds = Seconds

d-oprnqjx3-filter-ends_at = Ends at

d-oprnqjx3-filter-left = Left

d-euyscib3 = Lap
    .doc = A lap the stopwatch kept: the time it stood at.
    .noun = { $count ->
        [one] lap
       *[other] laps
    }

d-mq5diuqb = At
    .doc = The time it stood at, in seconds.

d-euyscib3-filter-at = At

d-3io25rgu = Stopwatch
    .doc = The stopwatch: what it has counted, the run it is in, and the laps it kept.
    .noun = { $count ->
        [one] stopwatch
       *[other] stopwatches
    }

d-yhijhjuh = Elapsed
    .doc = What it has counted before the run it is in, in seconds.

d-fg2gmze6 = Since
    .doc = The instant its run started; none while it is paused.

d-ngcmcd7g = Laps
    .doc = The times it stood at when a lap was kept, in order.

d-3io25rgu-filter-elapsed = Elapsed

d-3io25rgu-filter-since = Since

d-ppcjxruc = Clocks
    .doc = The workspace's clocks.
    .noun = { $count ->
        [one] clock
       *[other] clocks
    }

d-r2amisi7 = World clocks
    .doc = The cities the person compares time with.

d-gneumngu = Alarms
    .doc = The alarms set, on whichever device each rings.

d-kahapraa = Timers
    .doc = The timers running, paused or done.

d-rjh2x76r = Stopwatch
    .doc = The stopwatch: at zero until it is started.

d-ppcjxruc-filter-stopwatch = Stopwatch

d-pmxwuu3y = Weekday
    .doc = A day of the week an alarm repeats on, Monday first.

d-ohvi4cp6 = Monday

d-lzxlz4cd = Tuesday

d-ikwb6g2v = Wednesday

d-3imexqqa = Thursday

d-jcyf2yea = Friday

d-d5t3oavd = Saturday

d-qa4o45am = Sunday

d-ofkzixdx = Add world clock
    .doc = Adds a city's clock to the workspace.

d-ofkzixdx-city = City

d-ofkzixdx-zone = Zone

d-tp56af3m = Remove world clock
    .doc = Removes a city's clock.

d-tp56af3m-w = W

d-aozz4wlf = New alarm
    .doc = Sets an alarm: at the time of day `when` names, on the days it repeats on, ringing on `rings_on`, every device of the person's when it names none.
    .w-an-alarm-rings-at-an-hour = An alarm rings at an hour from 0 to 23
    .w-an-alarm-rings-at-a-minute = An alarm rings at a minute from 0 to 59

d-aozz4wlf-label = Label

d-aozz4wlf-hour = Hour

d-aozz4wlf-minute = Minute

d-aozz4wlf-days = Days

d-aozz4wlf-rings_on = Rings on

d-fivi3e67 = Edit alarm
    .doc = Changes an alarm's words, time and days.
    .w-an-alarm-rings-at-an-hour = An alarm rings at an hour from 0 to 23
    .w-an-alarm-rings-at-a-minute = An alarm rings at a minute from 0 to 59

d-fivi3e67-label = Label

d-fivi3e67-hour = Hour

d-fivi3e67-minute = Minute

d-fivi3e67-days = Days

d-sqirq543 = Set alarm enabled
    .doc = Turns an alarm on or off: off, it does not ring; on, it rings next at its time.

d-sqirq543-enabled = Enabled

d-uxdphvkl = Set alarm device
    .doc = Names the device an alarm rings on: one device's id, or none for every device of the person's.

d-uxdphvkl-rings_on = Rings on

d-eencngbt = Toggle day
    .doc = Repeats an alarm on `d`, or stops repeating on it: a day it names rings, a day it does not skips.

d-eencngbt-d = D

d-mgfcysje = Remove alarm
    .doc = Removes an alarm.

d-mgfcysje-a = A

d-2uk2bvmz = Snooze 5 minutes
    .doc = Snoozes the alarm ringing now: it rings again in five minutes.

d-7lw2tfxp = Ring the alarm now
    .doc = Rings an alarm when its moment comes, on the device it names, window or no window.
    .w-it-is = It is { $hour }:{ $pad2 }

d-7anuyebc = Start a timer
    .doc = Starts a timer of `seconds`.
    .w-a-timer-needs-a-duration-greater = A timer needs a duration greater than zero

d-7anuyebc-label = Label

d-7anuyebc-seconds = Seconds

d-latbht2w = Pause timer
    .doc = Pauses a running timer where it stands.

d-2n5kbb4o = Resume timer
    .doc = Resumes a paused timer, or starts a finished one over.

d-eepdn3h5 = Add minute
    .doc = Adds a minute to a timer, running or paused.

d-nwtaannw = Remove timer
    .doc = Removes a timer.

d-nwtaannw-t = T

d-gidp5so6 = Ring timer
    .doc = Rings a timer when it finishes, on every device of the person's, window or no window.
    .w-the-timer-finished = The timer finished

d-knkom4sf = Start the stopwatch
    .doc = Starts the stopwatch.

d-kcycaaaa = Pause the stopwatch
    .doc = Pauses the stopwatch where it stands.

d-7twb2otm = Lap
    .doc = Keeps a lap: the time the stopwatch stands at.

d-qcjegi7p = Reset the stopwatch
    .doc = Resets the stopwatch: it is at zero and stopped, with no laps.

d-x2tarjxv = Add alarm~present

d-u4chxfdj = Edit alarm~present

d-tv3yomxx = Add timer~present

d-qobxx5dl = Clocks page
    .w-clocks = Clocks
    .w-world = World
    .w-alarms = Alarms
    .w-timers = Timers
    .w-stopwatch = Stopwatch

d-sm7xxyrn = World place
    .w-add-a-city = Add a city
    .w-search-a-city-or-a-zone = Search a city or a zone
    .w-here = Here
    .w-no-cities-yet-add-one-to = No cities yet. Add one to compare times at a glance.

d-inqk2nou = Alarms place
    .w-no-alarms-yet-set-one-for = No alarms yet. Set one for a future time.
    .w-edit = Edit { $label }
    .w-what-is-this-alarm-for = What is this alarm for?
    .w-hour = Hour
    .w-h = h
    .w-minute = Minute
    .w-min = min
    .w-save-changes = Save changes
    .w-cancel = Cancel
    .w-new-alarm = New alarm
    .w-add-alarm = Add alarm

d-g2agvnl7 = Timers place
    .w-no-timers-yet = No timers yet
    .w-quick-starts = Quick starts
    .w-tea-5-min = Tea (5 min)
    .w-break-10-min = Break (10 min)
    .w-focus-25-min = Focus (25 min)
    .w-laundry-45-min = Laundry (45 min)
    .w-custom-timer = Custom timer
    .w-what-is-this-timer-for = What is this timer for?
    .w-minutes = Minutes
    .w-min = min
    .w-start-timer = Start timer

d-k3yetx4r = Stopwatch place
    .w-stopwatch = Stopwatch
    .w-start = Start
    .w-pause = Pause
    .w-lap = Lap
    .w-reset = Reset
