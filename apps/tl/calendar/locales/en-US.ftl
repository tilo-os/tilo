# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-calendar = Calendar

d-qpfc7vua = Attendee
    .doc = Someone invited to an event, and what they answered: `needs-action` until they say.
    .noun = { $count ->
        [one] attendee
       *[other] attendees
    }

d-oiolhfhy = Name

d-zr5l6sfu = Email
    .doc = Their email address, where the invitation goes, and what names them here.

d-wyfwksgq = Status
    .doc = What they answered: `needs-action`, `accepted`, `declined` or `tentative`.

d-fweteite = Role
    .doc = What part they take: `required`, `optional` or `chair`.

d-uvsswhpm = Project
    .doc = A project: what a set of events and tasks is for, and the colour it takes.
    .noun = { $count ->
        [one] project
       *[other] projects
    }

d-iivr4dfm = Name

d-5kprlx3s = Color

d-fsndgah6 = Order
    .doc = Where it lists among the projects.

d-uvsswhpm-filter-color = Color

d-uvsswhpm-filter-order = Order

d-q2pjd3ck = Task
    .doc = A task: what to do, by when, and in which project.
    .noun = { $count ->
        [one] task
       *[other] tasks
    }
    .untitled = New task

d-r2vx64eh = Title

d-s6raiajy = Done

d-kibl2yut = Due
    .doc = The day it is due by, if any.

d-2o4hdoux = Remind
    .doc = When to tell whoever is doing it that its moment came, if a reminder is set.

d-ivfbe5bc = Notes
    .doc = Anything worth remembering about it.

d-whecqzmz = Priority

d-en7w5zf7 = Project
    .doc = The project it belongs to; none while it waits in the inbox.

d-xna3jvyc = Parent
    .doc = The task it is a step of; none for a task of its own.

d-q2pjd3ck-filter-inbox = Inbox

d-q2pjd3ck-filter-today = Today

d-q2pjd3ck-filter-upcoming = Upcoming

d-q2pjd3ck-filter-done = Done

d-q2pjd3ck-filter-due = Due

d-q2pjd3ck-filter-project = Project

d-q2pjd3ck-filter-priority = Priority

d-q2pjd3ck-filter-parent = Parent

d-blagq4dk = Event
    .doc = Something happening on a day: what, when, where, and who is coming.
    .noun = { $count ->
        [one] event
       *[other] events
    }
    .untitled = New event

d-qqkexjof = Key
    .doc = The entry's id: its `UID`, the name its server and every client know it by.

d-yclpjnie = Href
    .doc = Where its server keeps it; empty for an entry no server holds.

d-lpmkddba = Etag
    .doc = The version its server last gave it, so a change is written only over what was read.

d-5b3dmvgx = Title

d-f36rh6hq = Start
    .doc = The wall time it starts at, as a calendar writes it: `2026-11-24T09:00:00`.

d-ahyl252g = End
    .doc = The wall time it ends at, the same way.

d-yyqvqgb4 = All day
    .doc = Whether it takes the whole day rather than the hours it names.

d-dmlhzgmz = Timezone
    .doc = The zone its wall times are read in: empty or `floating` for the same time of day everywhere, else `UTC` or an IANA zone such as `Europe/Berlin`.

d-mvt5vjkt = Location

d-gqmucsa7 = Notes
    .doc = What the server says of it: its `DESCRIPTION`, as text.

d-igx7zgee = Body
    .doc = What the person writes of it, in rich text.

d-ypsmw6x6 = Attendees
    .doc = Who is coming, and what each answered.

d-ayba7si7 = Categories
    .doc = The person's own words for what kind of event it is.

d-phwonegh = Recurrence
    .doc = Its frequency, in iCalendar's words (`FREQ=WEEKLY`); empty for one that does not repeat.

d-6oxxhsol = Repeat
    .doc = How it repeats: which unit the rule counts in.

d-zjilh6px = Recurrence every
    .doc = How often the repeat comes, in its own unit: every 2 weeks is 2.

d-uyipmxcj = Recurrence until
    .doc = The last day it repeats by, if it ends on a day.

d-zzjroxh5 = Recurrence count
    .doc = How many times it repeats in all, if it ends after a count.

d-dn2juryi = Recurrence weekdays
    .doc = The days of the week it repeats on, as `MO`..`SU`; empty for the day it stands on.

d-xgv6mdnt = Exdates
    .doc = The days its series leaves out, each as a day's text.

d-js6qgkt2 = Day
    .doc = The day it stands on: kept with its moments so a window finds it by a day of its own.

d-jz3mybhh = End day
    .doc = The day it ends on, kept with `end` as `day` is kept with `start`: a day past the first stands for every day it runs through.

d-zwcmfzky = Color
    .doc = The colour it takes: the name of one of the palette's places, empty for its calendar's.

d-qe2xcyxt = Project
    .doc = What the calendar service files it under; the server holds nothing of it.

d-p2upd4ee = Filed under
    .doc = The project the person files it under.

d-l44x7h7l = Organizer
    .doc = The address of whoever organises it; empty while this workspace does.

d-n3y6kpek = Cancelled
    .doc = Whether one date of a series is cancelled rather than moved.

d-7mqdwjrk = Calendar
    .doc = The calendar it belongs to: the workspace's own while none.

d-72habrff = Remind
    .doc = When to tell the people coming that it starts, if anyone is to be told: 1970 for none.

d-2ukd2xzk = Recurrence of
    .doc = The event this one stands apart from, when it is one date of a series changed on its own; none for an event of its own and for the series itself.

d-x6t7zpds = Original start
    .doc = The date of the series this one stands in for, when it is one date changed on its own.

d-blagq4dk-filter-upcoming = Upcoming

d-blagq4dk-filter-coming = Coming

d-blagq4dk-filter-today = Today

d-blagq4dk-filter-repeating = Repeating

d-blagq4dk-filter-day = Day

d-blagq4dk-filter-calendar = Calendar

d-blagq4dk-filter-recurrence_of = Recurrence of

d-7lkss2cg = Calendar
    .doc = A calendar: the color-coded place events are kept.
    .noun = { $count ->
        [one] calendar
       *[other] calendars
    }

d-gzrwihvd = Key
    .doc = The calendar's id: the address its server keeps it at, or one given to a calendar of the workspace's own.

d-pwrd7qqx = Account
    .doc = The key of the account whose server holds it; empty for one of the workspace's own.

d-dn3prfqt = Sync token
    .doc = How far the server's changes were read the last time, so a restart asks only for more.

d-kc5ls3bi = Name

d-kaeu2eyo = Color
    .doc = The colour it is drawn in: the name of one of the palette's places.

d-xm42jvvk = Writable
    .doc = Whether its server lets this workspace add and change its events.

d-7lkss2cg-filter-writable = Writable

d-53uahg6d = Calendars
    .doc = The workspace's events and its calendars.
    .noun = { $count ->
        [one] calendar
       *[other] calendars
    }

d-g3gq3jmz = Events

d-ozulkrgt = Calendars

d-jum24lyb = Tasks
    .doc = Every task the calendar keeps, in the order people put them.

d-japv422q = Projects

d-i5l2mswn = Anchor at
    .doc = The moment the window stands on: the day it shows, at that day's midnight where the person is, so the timeline's anchor is the day itself whatever their zone.

d-53uahg6d-filter-anchor_at = Anchor at

d-kitpj7rj = Moment
    .doc = An event at one of its moments: the time it is shown at and the day that time is on in the event's own zone, in order with every other.
    .noun = { $count ->
        [one] moment
       *[other] moments
    }

d-kd3noj7y = Event

d-ejfozpye = At

d-7hfwgky5 = Day

d-kitpj7rj-filter-event = Event

d-kitpj7rj-filter-at = At

d-kitpj7rj-filter-day = Day

d-fyjnvya3 = Priority
    .doc = How much a task matters, most urgent last.

d-7aeeltah = No priority

d-vdxowaof = Low

d-vi6ycr4b = Medium

d-vqrkky62 = High

d-khekppb7 = Color
    .doc = The colours a calendar, an event or a project can take, each a place in the theme.

d-izcenp23 = Indigo

d-ztqu6rob = Emerald

d-sry2vjm5 = Amber

d-ahvlon4n = Red

d-zxs6cm36 = Blue

d-rbvm6tti = Violet

d-qw4eohq4 = Slate

d-m4xs4kw5 = Repeat
    .doc = How an event repeats: every day, week, month or year, or on chosen days of the week.

d-xj7cutos = None

d-xh6fjimx = Every day

d-tfsn4ro7 = Every week

d-z6vytity = Every month

d-zivuldfk = Every year

d-h6ffk5fr = The days chosen of every week

d-2rwifk7b = Own
    .doc = What a workspace's calendar starts with: its own calendar, which holds events an account does not, so an event can be made the moment the calendar opens.
    .w-my-calendar = My calendar

d-or5ltszn = Land
    .doc = Opening the calendar shows this week.

d-rpxn5jol = New event
    .doc = Starts an event on a day the person points at, at nine in the morning for an hour, or at the moment the words name.

d-rpxn5jol-title = Title

d-rpxn5jol-day = Day

d-rpxn5jol-at = At

d-rpxn5jol-minutes = Minutes

d-ws3idlc6 = New event
    .doc = Makes an event of the hours dragged across, from where the drag began to where it ended.
    .w-a-drag-goes-forward-in-time = A drag goes forward in time

d-ws3idlc6-start = Start

d-ws3idlc6-end = End

d-5fpyx3ag = Move
    .doc = Moves an event where it was dropped: a day chip moves to that day keeping its time of day, and an hour moves to the hour it was dropped at.

d-5fpyx3ag-start = Start

d-5fpyx3ag-place = Place

d-3fhya4jn = Resize
    .doc = Ends an event where its edge was dropped.

d-3fhya4jn-end_at = End at

d-wxnfyq2o = Project
    .doc = Files an event under a project, or files it under none.

d-wxnfyq2o-p = P

d-obvjd33k = Colour
    .doc = Sets the colour an event is drawn in.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed

d-obvjd33k-color = Color

d-jxqvsawj = Its calendar's colour
    .doc = Takes an event's own colour off: it is drawn in its calendar's.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed

d-2vx7zp6l = Colour
    .doc = Sets the colour a calendar is drawn in.

d-2vx7zp6l-color = Color

d-nlrpnve5 = Categories
    .doc = Sets the categories an event is filed under in its person's words.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed

d-nlrpnve5-categories = Categories

d-se6shkk2 = Time zone
    .doc = Sets the zone an event's wall times are read in: `Europe/Berlin`, `UTC`, or `floating` for the same time of day everywhere.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed
    .w-a-zone-is-floating-utc-or = A zone is floating, UTC, or a name such as Europe/Berlin
    .w-that-is-not-a-time-zone = That is not a time zone this build knows

d-se6shkk2-zone = Zone

d-yollo4f5 = Add a category
    .doc = Files an event under one more category.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed
    .w-a-category-is-a-few-words = A category is a few words

d-yollo4f5-category = Category

d-uwnaecvd = Remove a category
    .doc = Takes a category off an event.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed

d-uwnaecvd-category = Category

d-5eqjpk6o = Organiser
    .doc = Makes someone the organiser of an event.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed

d-5eqjpk6o-who = Who

d-qdqgaukz = At the start
    .doc = Reminds the people coming when the event starts.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed

d-uif2dwsr = Before
    .doc = Reminds them a given number of minutes before it starts.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed
    .w-a-reminder-is-before-the-start = A reminder is before the start

d-uif2dwsr-minutes = Minutes

d-vjp2n4ek = No reminder
    .doc = Tells nobody, once more.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed
    .w-it-has-no-reminder = It has no reminder

d-fowc4rav = Repeat
    .doc = Repeats an event, its rule in iCalendar's words, how often, and where it ends.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed

d-fowc4rav-how = How

d-fowc4rav-every = Every

d-fowc4rav-weekdays = Weekdays

d-fowc4rav-until = Until

d-fowc4rav-count = Count

d-q23cd64i = Every week on
    .doc = Repeats an event on chosen days of the week from the day it stands on.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed
    .w-choose-the-days-it-repeats-on = Choose the days it repeats on

d-q23cd64i-days = Days

d-e64m52e3 = Does not repeat
    .doc = Stops an event repeating from the day it stands on.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed
    .w-it-does-not-repeat = It does not repeat

d-yeztwom7 = Skip this date
    .doc = Skips one date of a repeating event.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed
    .w-it-does-not-repeat = It does not repeat

d-yeztwom7-day = Day

d-ows6cr6i = Restore this date
    .doc = Gives a skipped date of a repeating event back.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed

d-ows6cr6i-day = Day

d-44i5zfi7 = New task
    .doc = Adds a task to the inbox or to a project.

d-44i5zfi7-p = P

d-44i5zfi7-due = Due

d-44i5zfi7-title = Title

d-pajga22i = Complete
    .doc = Checks a task off, or opens it again.
    .w-complete = Complete
    .w-reopen = Reopen

d-pajga22i-done = Done

d-53vfjjzi = Pick date
    .doc = Sets the day a task is due by.

d-53vfjjzi-due = Due

d-vka4mzet = Today
    .doc = Makes a task due today.

d-cz63daqa = Tomorrow
    .doc = Makes a task due tomorrow.

d-hf3h2qfi = Next week
    .doc = Makes a task due at the start of next week.

d-biws2vsv = No date
    .doc = Clears the day a task is due by.
    .w-it-has-no-date = It has no date

d-qp6cix3j = Priority
    .doc = Sets how much a task matters.

d-qp6cix3j-priority = Priority

d-rz4omsam = Project
    .doc = Files a task under a project, or under none for the inbox.

d-rz4omsam-p = P

d-tvabo7ia = Add a step
    .doc = Adds a step under a task, in its project, and opens the task if it was done: there is work under it now.

d-tvabo7ia-t = T

d-tvabo7ia-title = Title

d-ei6hkzlc = Delete task
    .doc = Deletes a task and the steps under it.

d-ei6hkzlc-t = T

d-aaijm6cm = New project
    .doc = Starts a project.
    .w-new-project = New project

d-4qqhxlyv = Colour
    .doc = Sets the colour a project is drawn in.

d-4qqhxlyv-color = Color

d-yivccbzw = Move
    .doc = Moves a project one place earlier or later among the projects.

d-yivccbzw-p = P

d-yivccbzw-up = Up

d-o2egikzm = Rename project
    .doc = Renames a project.

d-o2egikzm-name = Name

d-xtjwddq3 = Delete project
    .doc = Deletes a project; its events and tasks go back to none.

d-xtjwddq3-p = P

d-zaeu6beo = Only this event
    .doc = Moves one date of a repeating event: an event of its own stands in its place and the series keeps every other date.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed
    .w-it-does-not-repeat = It does not repeat

d-zaeu6beo-at = At

d-zaeu6beo-start = Start

d-zaeu6beo-end = End

d-bd3x7ejn = Cancel this date
    .doc = Cancels one date of a repeating event.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed
    .w-it-does-not-repeat = It does not repeat

d-bd3x7ejn-at = At

d-25bxqiqq = Restore this date
    .doc = Gives a cancelled or moved date of a series back.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed

d-25bxqiqq-at = At

d-xagcihzh = This and following events
    .doc = Moves a repeating event from one date on: the series ends the day before, and a new one begins where it was moved to, with every date after it.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed
    .w-it-does-not-repeat = It does not repeat

d-xagcihzh-at = At

d-xagcihzh-start = Start

d-xagcihzh-end = End

d-765a5vhw = Show this day
    .doc = Stands the window on the day a moment names.

d-765a5vhw-at = At

d-5uzhjzk5 = Previous day
    .doc = Shows the day before.

d-ewzzoi2n = Next day
    .doc = Shows the day after.

d-tsbzrzd5 = Today
    .doc = Shows today.

d-6src2gs5 = Sync calendars
    .doc = Asks each account's server for its calendars now, so what it holds comes in as it does when the server is heard from.

d-rox4evo5 = New calendar
    .doc = Adds a calendar of the workspace's own.
    .w-new-calendar = New calendar

d-37r6seh2 = Rename calendar
    .doc = Renames a calendar.

d-37r6seh2-name = Name

d-ael6ndgj = Delete event
    .doc = Deletes an event.

d-ael6ndgj-e = E

d-qg4djklq = Move to day
    .doc = Moves an event to a day, keeping the time of day it starts and how long it lasts, and its reminder at the same distance from its start.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed
    .w-it-is-on-that-day-already = It is on that day already

d-qg4djklq-day = Day

d-ifyhqlkq = Change day
    .doc = Moves an event to the day a moment names, keeping the time of day it starts.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed

d-ifyhqlkq-at = At

d-e7aimj6j = Change start
    .doc = Sets the time of day an event starts, its end keeping how long it lasts.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed

d-e7aimj6j-at = At

d-4fezz4oa = Change end
    .doc = Sets when an event ends.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed
    .w-an-event-ends-after-it-starts = An event ends after it starts

d-4fezz4oa-at = At

d-lbfkyak2 = Repeat
    .doc = Repeats an event every day, week, month or year from the day it stands on, on the days chosen of a week, and no longer on the days it named before.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed

d-lbfkyak2-how = How

d-z7u3n5xs = How often
    .doc = How often it repeats, in the unit it repeats in: every two weeks is 2.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed
    .w-it-repeats-at-least-once = It repeats at least once

d-z7u3n5xs-every = Every

d-ckl3jlil = Repeat until
    .doc = The last day it repeats by, or none for a series that does not end.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed

d-ckl3jlil-until = Until

d-mm3tzg2x = Repeat until
    .doc = The last day it repeats by, as the day a moment names.

d-mm3tzg2x-at = At

d-y5e66bkb = Times
    .doc = How many times it repeats in all, or none for a series that does not end.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed

d-y5e66bkb-count = Count

d-6tammo57 = Calendar
    .doc = Files an event under a calendar the person picks.
    .w-this-calendar-is-connected-for-reading = This calendar is connected for reading only, so nothing in it is changed
    .w-it-is-in-that-calendar-already = It is in that calendar already

d-6tammo57-k = K

d-td6wjjza = Invite
    .doc = Invites someone to an event, by the name and address the person gives.
    .w-that-is-not-an-email-address = That is not an email address
    .w-they-are-coming-already = They are coming already

d-td6wjjza-name = Name

d-td6wjjza-email = Email

d-luj3kjit = Answer
    .doc = Records someone's answer to an invitation: `accepted`, `declined` or `tentative`.
    .w-an-answer-is-accepted-declined-or = An answer is accepted, declined or maybe

d-luj3kjit-g = G

d-luj3kjit-status = Status

d-gtgyzaen = Remove guest
    .doc = Takes someone off an event's guests: nothing is sent to them.

d-gtgyzaen-g = G

d-fesqobfx = Invite by mail
    .doc = Starts a message inviting a guest, the event's calendar invitation carried beside it: what Mail shows the person, to read and send.
    .w-invitation = Invitation: { $title }
    .w-you-are-invited-to-on-at = You are invited to { $title } on { $day } at { $start }. The attached calendar invitation holds the details.

d-fesqobfx-g = G

d-ndflsrl3 = Ring due
    .doc = Rings an event's reminder when its moment comes, and nothing for an event that has none.

d-nzbj5co5 = Remind now
    .doc = Tells the people coming that an event starts, once, and clears its reminder.
    .w-it-has-no-reminder = It has no reminder
    .w-starts = Starts { $start }

d-7zsx6jhg = Show in calendar
    .doc = Goes to the week an event is on.

d-bwfktkuo = Previous week
    .doc = The window shows the week before.

d-jgqshvk4 = Next week
    .doc = The window shows the week after.

d-xcvv34ug = Previous month
    .doc = The window shows the month before.

d-46ixi6nl = Next month
    .doc = The window shows the month after.

d-uvs7cpof = Today
    .doc = The window shows this week.

d-ldaioj6y = How often
    .doc = Repeats an event how often the person typed, in the unit it repeats in: 2 for every other.
    .w-how-often-it-repeats-is-a = How often it repeats is a number: 2 for every other

d-ldaioj6y-typed = Typed

d-wka7zl52 = Times
    .doc = Repeats an event as many times as the person typed.
    .w-how-many-times-it-repeats-is = How many times it repeats is a number

d-wka7zl52-typed = Typed

d-3u4tdxfq = Export calendar
    .doc = The calendar — every event and task it holds, or only one event — as an iCalendar file, for a person to save or an agent to hand on.

d-3u4tdxfq-e = E

d-zmfdk46t = Import calendar
    .doc = Imports the events and tasks of an iCalendar file, and answers how many it took: what a person's own file offers, and what another calendar exported.
    .w-could-not-read = Could not read { $name }: { $why }
    .w-imported-events-and-tasks = Imported { $len } events and { $len2 } tasks

d-zmfdk46t-file = File

d-4hhlot2y = Color label
    .w-emerald = Emerald
    .w-amber = Amber
    .w-red = Red
    .w-blue = Blue
    .w-violet = Violet
    .w-slate = Slate
    .w-indigo = Indigo

d-i2lmojqn = Task list name
    .w-inbox = Inbox
    .w-upcoming = Upcoming
    .w-done = Done
    .w-today = Today

d-7zcpw62t = New between~present

d-dihvjrzc = Set event color~present

d-j6izcylf = Clear event color~present

d-iozwj4om = Set categories~present

d-32s7g4lk = Set zone~present

d-wuqfd7rq = Add category~present

d-2s2b3ygt = Remove category~present

d-3wxuaohk = Organize~present

d-4xtalqz5 = Remind at start~present

d-ohbh2duz = Remind before~present

d-b76dfsqm = Remind off~present

d-dulzqkmm = Repeat rule~present

d-m55bevrg = Repeat weekdays~present

d-ghy7ry76 = Repeat off~present

d-7yrte5jx = Skip date~present

d-wiqo5ox3 = Restore date~present

d-5ecna76j = Check task~present

d-4mw4lmsy = Due task clear~present

d-wdmvy4o7 = Move occurrence~present

d-iswyaeqi = Cancel occurrence~present

d-m4vjjhhy = Restore occurrence~present

d-bcgihhy5 = Move following~present

d-baljrz2g = Move to day~present

d-r6l53d5k = Change day~present

d-u2vnroei = Change start~present

d-c3a3dxxn = Change end~present

d-7cffrvhe = Repeat~present

d-v4wdiwvk = Repeat every~present

d-rfmgr4il = Repeat until~present

d-c3653wxf = Repeat count~present

d-umaixua3 = File in~present

d-v36oxy7z = Invite~present

d-3l63az42 = Answer~present

d-eapd2aju = Remind now~present

d-m2in7j2d = Repeat every text~present

d-5bfm4ii5 = Repeat count text~present

d-nd2mftjh = Window
    .w-calendar = Calendar
    .w-week = Week
    .w-month = Month
    .w-agenda = Agenda
    .w-day = Day
    .w-tasks = Tasks
    .w-task-lists = Task lists
    .w-inbox = Inbox
    .w-today = Today
    .w-upcoming = Upcoming
    .w-done = Done
    .w-projects = Projects
    .w-calendars = Calendars
    .w-all-calendars = All calendars
    .w-previous-day = Previous day
    .w-next-day = Next day
    .w-previous-month = Previous month
    .w-next-month = Next month
    .w-previous-week = Previous week
    .w-next-week = Next week
    .w-pick-a-day = Pick a day
    .w-repeats = { $title } repeats
    .w-only-this-date = Only this date
    .w-this-and-all-following = This and all following

d-uc6iwdpn = Agenda
    .w-new-event = New event
    .w-nothing-scheduled-in-the-next-three = Nothing scheduled in the next three weeks

d-nx5lspqo = Tasks
    .w-new-task-in = New task in { $name }
    .w-new-task = New task
    .w-nothing-in = Nothing in { $title }

d-u2sfuxhf = Task row
    .w-a-step-of = A step of { $title }

d-gtqqlwre = Event row

d-jy3d3ql4 = Task page
    .w-what-to-do = What to do
    .w-anything-worth-remembering-about-it = Anything worth remembering about it.
    .w-inbox = Inbox
    .w-due = Due
    .w-today = Today
    .w-tomorrow = Tomorrow
    .w-next-week = Next week
    .w-no-date = No date
    .w-priority = Priority
    .w-no-priority = No priority
    .w-low = Low
    .w-medium = Medium
    .w-high = High
    .w-steps = Steps
    .w-add-a-step = Add a step

d-jd6ftqrt = Moment page
    .w-event-title = Event title
    .w-what-is-it-about = What is it about?
    .w-day = Day
    .w-starts = Starts
    .w-ends = Ends
    .w-add-a-place = Add a place
    .w-all-day = All day
    .w-overlaps = Overlaps { $other }
    .w-my-calendar = My calendar
    .w-colour = Colour
    .w-no-project = No project
    .w-move = Move
    .w-tomorrow = Tomorrow
    .w-next-week = Next week
    .w-repeat = Repeat
    .w-every-day = Every day
    .w-every-week = Every week
    .w-every-month = Every month
    .w-every-year = Every year
    .w-does-not-repeat = Does not repeat
    .w-remind = Remind
    .w-reminder-set = Reminder set
    .w-at-the-start = At the start
    .w-5-minutes-before = 5 minutes before
    .w-10-minutes-before = 10 minutes before
    .w-30-minutes-before = 30 minutes before
    .w-an-hour-before = An hour before
    .w-a-day-before = A day before
    .w-no-reminder = No reminder
    .w-how-often = How often
    .w-until = Until
    .w-repeat-until = Repeat until
    .w-clear = Clear
    .w-no-end = No end
    .w-times = Times
    .w-occurrences = Occurrences
    .w-filed-under = Filed under
    .w-add-a-category = Add a category
    .w-organiser-s-email-address = Organiser's email address
    .w-guests = Guests
    .w-invite-by-email-address = Invite by email address

d-qy4w4m3w = Event page
    .w-event-title = Event title
    .w-what-is-it-about = What is it about?
    .w-day = Day
    .w-starts = Starts
    .w-ends = Ends
    .w-add-a-place = Add a place
    .w-all-day = All day
    .w-overlaps = Overlaps { $other }
    .w-my-calendar = My calendar
    .w-colour = Colour
    .w-no-project = No project
    .w-move = Move
    .w-tomorrow = Tomorrow
    .w-next-week = Next week
    .w-repeat = Repeat
    .w-every-day = Every day
    .w-every-week = Every week
    .w-every-month = Every month
    .w-every-year = Every year
    .w-does-not-repeat = Does not repeat
    .w-remind = Remind
    .w-reminder-set = Reminder set
    .w-at-the-start = At the start
    .w-5-minutes-before = 5 minutes before
    .w-10-minutes-before = 10 minutes before
    .w-30-minutes-before = 30 minutes before
    .w-an-hour-before = An hour before
    .w-a-day-before = A day before
    .w-no-reminder = No reminder
    .w-how-often = How often
    .w-until = Until
    .w-repeat-until = Repeat until
    .w-clear = Clear
    .w-no-end = No end
    .w-times = Times
    .w-occurrences = Occurrences
    .w-filed-under = Filed under
    .w-add-a-category = Add a category
    .w-organiser-s-email-address = Organiser's email address
    .w-guests = Guests
    .w-invite-by-email-address = Invite by email address
