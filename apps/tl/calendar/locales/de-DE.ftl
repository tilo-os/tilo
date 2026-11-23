# Kalender auf Deutsch. Keys are the declarations' stable ids (app.ids); see locales/en-US.ftl.

d-so-tilo-calendar = Kalender

d-qpfc7vua = Gast
    .doc = Jemand, der zu einem Termin eingeladen ist, und was er geantwortet hat: `needs-action`, bis er es sagt.
    .noun = { $count ->
        [one] Gast
       *[other] Gäste
    }

d-oiolhfhy = Name

d-zr5l6sfu = E-Mail
    .doc = Die E-Mail-Adresse, an die die Einladung geht, und was sie hier nennt.

d-wyfwksgq = Status
    .doc = Was sie geantwortet haben: `needs-action`, `accepted`, `declined` oder `tentative`.

d-fweteite = Rolle
    .doc = Welche Rolle sie haben: `required`, `optional` oder `chair`.

d-uvsswhpm = Projekt
    .doc = Ein Projekt: wofür eine Reihe von Terminen und Aufgaben ist und welche Farbe es trägt.
    .noun = { $count ->
        [one] Projekt
       *[other] Projekte
    }

d-iivr4dfm = Name

d-5kprlx3s = Farbe

d-fsndgah6 = Reihenfolge
    .doc = Wo es unter den Projekten steht.

d-uvsswhpm-filter-color = Farbe

d-uvsswhpm-filter-order = Reihenfolge

d-q2pjd3ck = Aufgabe
    .doc = Eine Aufgabe: was zu tun ist, bis wann und in welchem Projekt.
    .noun = { $count ->
        [one] Aufgabe
       *[other] Aufgaben
    }
    .untitled = Neue Aufgabe

d-r2vx64eh = Titel

d-s6raiajy = Erledigt

d-kibl2yut = Fällig
    .doc = Der Tag, bis zu dem sie fällig ist, falls es einen gibt.

d-2o4hdoux = Erinnern
    .doc = Wann dem, der sie tut, gesagt wird, dass ihr Zeitpunkt kam, falls eine Erinnerung gesetzt ist.

d-ivfbe5bc = Notizen
    .doc = Alles, was dazu zu merken ist.

d-whecqzmz = Priorität

d-en7w5zf7 = Projekt
    .doc = Das Projekt, zu dem sie gehört; keines, solange sie im Eingang wartet.

d-xna3jvyc = Übergeordnet
    .doc = Die Aufgabe, von der sie ein Schritt ist; keine für eine eigene Aufgabe.

d-q2pjd3ck-filter-inbox = Eingang

d-q2pjd3ck-filter-today = Heute

d-q2pjd3ck-filter-upcoming = Kommend

d-q2pjd3ck-filter-done = Erledigt

d-q2pjd3ck-filter-due = Fällig

d-q2pjd3ck-filter-project = Projekt

d-q2pjd3ck-filter-priority = Priorität

d-q2pjd3ck-filter-parent = Übergeordnet

d-blagq4dk = Termin
    .doc = Etwas, das an einem Tag geschieht: was, wann, wo und wer kommt.
    .noun = { $count ->
        [one] Termin
       *[other] Termine
    }
    .untitled = Neuer Termin

d-qqkexjof = Key
    .doc = The entry's id: its `UID`, the name its server and every client know it by.

d-yclpjnie = Anfang
    .doc = Die Wandzeit, zu der er beginnt, wie ein Kalender sie schreibt: `2026-11-24T09:00:00`.

d-lpmkddba = Ende
    .doc = Die Wandzeit, zu der er endet, auf dieselbe Weise.

d-5b3dmvgx = Title

d-f36rh6hq = Zeitzone
    .doc = Die Zone, in der seine Wandzeiten gelesen werden: leer oder `floating` für dieselbe Tageszeit überall, sonst `UTC` oder eine IANA-Zone wie `Europe/Berlin`.

d-ahyl252g = End
    .doc = The wall time it ends at, the same way.

d-yyqvqgb4 = Notizen
    .doc = Was der Server dazu sagt: seine `DESCRIPTION`, als Text.

d-dmlhzgmz = Inhalt
    .doc = Was die Person dazu schreibt, als Rich Text.

d-mvt5vjkt = Location

d-gqmucsa7 = Kategorien
    .doc = Die eigenen Worte der Person dafür, was für ein Termin es ist.

d-igx7zgee = Wiederholung
    .doc = Was er wiederholt, in iCalendar-Worten (`FREQ=WEEKLY;BYDAY=MO`); leer für einen, der es nicht tut.

d-ypsmw6x6 = Wiederholen
    .doc = Wie er sich wiederholt: in welcher Einheit die Regel zählt.

d-ayba7si7 = Wiederholung alle
    .doc = Wie oft die Wiederholung kommt, in ihrer eigenen Einheit: alle 2 Wochen ist 2.

d-phwonegh = Wiederholung bis
    .doc = Der letzte Tag, bis zu dem er sich wiederholt, falls er an einem Tag endet.

d-6oxxhsol = Anzahl der Wiederholungen
    .doc = Wie oft er sich insgesamt wiederholt, wenn er nach einer Anzahl endet.

d-zjilh6px = Wochentage der Wiederholung
    .doc = Die Wochentage, an denen er sich wiederholt, als `MO`..`SU`; leer für den Tag, an dem er steht.

d-uyipmxcj = Ausgenommene Tage
    .doc = Die Tage, die seine Reihe auslässt, jeder als Text eines Tages.

d-zzjroxh5 = Tag
    .doc = Der Tag, an dem er steht: mit seinen Zeitpunkten geführt, damit ein Fenster ihn über einen eigenen Tag findet.

d-dn2juryi = Letzter Tag
    .doc = Der Tag, an dem er endet, mit `end` geführt wie `day` mit `start`: Ein Tag nach dem ersten steht für jeden Tag, über den er läuft.

d-xgv6mdnt = Farbe
    .doc = Die Farbe, die er trägt: der Name eines Platzes der Palette, leer für die seines Kalenders.

d-js6qgkt2 = Abgelegt unter
    .doc = Das Projekt, unter dem er abgelegt ist.

d-jz3mybhh = Organisator
    .doc = Die Adresse dessen, der ihn organisiert; leer, solange dieser Arbeitsbereich es tut.

d-zwcmfzky = Abgesagt
    .doc = Ob ein Termin einer Reihe abgesagt statt verschoben ist.

d-qe2xcyxt = Kalender
    .doc = Der Kalender, zu dem er gehört: der eigene des Arbeitsbereichs, solange keiner genannt ist.

d-p2upd4ee = Erinnern
    .doc = Wann den Gästen gesagt wird, dass er beginnt, falls jemandem etwas gesagt werden soll.

d-l44x7h7l = Wiederholung von
    .doc = Der Termin, von dem dieser sich löst, wenn er ein einzeln geänderter Termin einer Reihe ist; nichts für einen eigenen Termin und für die Reihe selbst.

d-n3y6kpek = Ursprünglicher Anfang
    .doc = Der Termin der Reihe, für den dieser steht, wenn es ein einzeln geänderter Termin ist.

d-7mqdwjrk = Calendar
    .doc = The calendar it belongs to: the workspace's own while none.

d-72habrff = Remind
    .doc = When to tell the people coming that it starts, if anyone is to be told: 1970 for none.

d-2ukd2xzk = Recurrence of
    .doc = The event this one stands apart from, when it is one date of a series changed on its own; none for an event of its own and for the series itself.

d-x6t7zpds = Original start
    .doc = The date of the series this one stands in for, when it is one date changed on its own.

d-blagq4dk-filter-upcoming = Tag

d-blagq4dk-filter-coming = Kalender

d-blagq4dk-filter-today = Wiederholung von

d-blagq4dk-filter-repeating = Repeating

d-blagq4dk-filter-day = Name

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

d-xm42jvvk = Anker bei
    .doc = Der Zeitpunkt, auf dem das Fenster steht: der Tag, den es zeigt, zu dessen Mitternacht dort, wo die Person ist, damit der Anker der Zeitleiste der Tag selbst ist, in welcher Zone auch immer.

d-7lkss2cg-filter-writable = Anker bei

d-53uahg6d = Calendars
    .doc = The workspace's events and its calendars.
    .noun = { $count ->
        [one] calendar
       *[other] calendars
    }

d-g3gq3jmz = Keine Priorität

d-ozulkrgt = Niedrig

d-jum24lyb = Tasks
    .doc = Every task the calendar keeps, in the order people put them.

d-japv422q = Hoch

d-i5l2mswn = Farbe
    .doc = Die Farben, die ein Kalender, ein Termin oder ein Projekt tragen kann, jede ein Platz im Thema.

d-53uahg6d-filter-anchor_at = Indigo

d-kitpj7rj = Moment
    .doc = An event at one of its moments: the time it is shown at and the day that time is on in the event's own zone, in order with every other.
    .noun = { $count ->
        [one] moment
       *[other] moments
    }

d-kd3noj7y = Bernstein

d-ejfozpye = Rot

d-7hfwgky5 = Blau

d-kitpj7rj-filter-event = Violett

d-kitpj7rj-filter-at = Schiefer

d-kitpj7rj-filter-day = Day

d-fyjnvya3 = Priority
    .doc = How much a task matters, most urgent last.

d-7aeeltah = Jeden Tag

d-vdxowaof = Jede Woche

d-vi6ycr4b = Jeden Monat

d-vqrkky62 = Jedes Jahr

d-khekppb7 = Color
    .doc = The colours a calendar, an event or a project can take, each a place in the theme.

d-izcenp23 = Indigo

d-ztqu6rob = Emerald

d-sry2vjm5 = Amber

d-ahvlon4n = Titel

d-zxs6cm36 = Tag

d-rbvm6tti = Um

d-qw4eohq4 = Slate

d-m4xs4kw5 = Repeat
    .doc = How an event repeats: every day, week, month or year, or on chosen days of the week.

d-xj7cutos = Ende

d-xh6fjimx = Every day

d-tfsn4ro7 = Anfang

d-z6vytity = Ort

d-zivuldfk = Every year

d-h6ffk5fr = Endet um

d-2rwifk7b = Own
    .doc = What a workspace's calendar starts with: its own calendar, which holds events an account does not, so an event can be made the moment the calendar opens.
    .w-my-calendar = My calendar

d-or5ltszn = Land
    .doc = Opening the calendar shows this week.

d-rpxn5jol = New event
    .doc = Starts an event on a day the person points at, at nine in the morning for an hour, or at the moment the words name.

d-rpxn5jol-title = Farbe

d-rpxn5jol-day = Day

d-rpxn5jol-at = At

d-rpxn5jol-minutes = Farbe

d-ws3idlc6 = Kategorien
    .doc = Setzt die Kategorien, unter die ein Termin abgelegt ist, in den Worten der Person.
    .w-a-drag-goes-forward-in-time = Ein Ziehen geht in der Zeit vorwärts

d-ws3idlc6-start = Kategorien

d-ws3idlc6-end = End

d-5fpyx3ag = Move
    .doc = Moves an event where it was dropped: a day chip moves to that day keeping its time of day, and an hour moves to the hour it was dropped at.

d-5fpyx3ag-start = Start

d-5fpyx3ag-place = Kategorie

d-3fhya4jn = Resize
    .doc = Ends an event where its edge was dropped.

d-3fhya4jn-end_at = Kategorie

d-wxnfyq2o = Project
    .doc = Files an event under a project, or files it under none.

d-wxnfyq2o-p = Wer

d-obvjd33k = Zum Anfang
    .doc = Erinnert die Gäste, wenn der Termin beginnt.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-obvjd33k-color = Color

d-jxqvsawj = Its calendar's colour
    .doc = Takes an event's own colour off: it is drawn in its calendar's.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-2vx7zp6l = Colour
    .doc = Sets the colour a calendar is drawn in.

d-2vx7zp6l-color = Color

d-nlrpnve5 = Categories
    .doc = Sets the categories an event is filed under in its person's words.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-nlrpnve5-categories = Alle

d-se6shkk2 = Time zone
    .doc = Sets the zone an event's wall times are read in: `Europe/Berlin`, `UTC`, or `floating` for the same time of day everywhere.
    .w-a-zone-is-floating-utc-or = A zone is floating, UTC, or a name such as Europe/Berlin
    .w-that-is-not-a-time-zone = That is not a time zone this build knows
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-se6shkk2-zone = Bis

d-yollo4f5 = Add a category
    .doc = Files an event under one more category.
    .w-a-category-is-a-few-words = A category is a few words
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-yollo4f5-category = Category

d-uwnaecvd = Remove a category
    .doc = Takes a category off an event.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-uwnaecvd-category = Category

d-5eqjpk6o = Organiser
    .doc = Makes someone the organiser of an event.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-5eqjpk6o-who = Tag

d-qdqgaukz = Diesen Tag zurückgeben
    .doc = Gibt einen ausgelassenen Termin einer Wiederholung zurück.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-uif2dwsr = Before
    .doc = Reminds them a given number of minutes before it starts.
    .w-a-reminder-is-before-the-start = A reminder is before the start
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-uif2dwsr-minutes = Minutes

d-vjp2n4ek = No reminder
    .doc = Tells nobody, once more.
    .w-it-has-no-reminder = It has no reminder
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-fowc4rav = Repeat
    .doc = Repeats an event, its rule in iCalendar's words, how often, and where it ends.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-fowc4rav-how = Titel

d-fowc4rav-every = Every

d-fowc4rav-weekdays = Erledigt

d-fowc4rav-until = Until

d-fowc4rav-count = Fällig

d-q23cd64i = Every week on
    .doc = Repeats an event on chosen days of the week from the day it stands on.
    .w-choose-the-days-it-repeats-on = Choose the days it repeats on
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-q23cd64i-days = Days

d-e64m52e3 = Does not repeat
    .doc = Stops an event repeating from the day it stands on.
    .w-it-does-not-repeat = It does not repeat
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-yeztwom7 = Skip this date
    .doc = Skips one date of a repeating event.
    .w-it-does-not-repeat = It does not repeat
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-yeztwom7-day = Day

d-ows6cr6i = Restore this date
    .doc = Gives a skipped date of a repeating event back.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-ows6cr6i-day = Day

d-44i5zfi7 = New task
    .doc = Adds a task to the inbox or to a project.

d-44i5zfi7-p = P

d-44i5zfi7-due = T

d-44i5zfi7-title = Titel

d-pajga22i = Complete
    .doc = Checks a task off, or opens it again.
    .w-complete = Complete
    .w-reopen = Reopen

d-pajga22i-done = T

d-53vfjjzi = Pick date
    .doc = Sets the day a task is due by.

d-53vfjjzi-due = Due

d-vka4mzet = Today
    .doc = Makes a task due today.

d-cz63daqa = Verschieben
    .doc = Verschiebt ein Projekt einen Platz nach vorn oder hinten unter den Projekten.

d-hf3h2qfi = Next week
    .doc = Makes a task due at the start of next week.

d-biws2vsv = No date
    .doc = Clears the day a task is due by.
    .w-it-has-no-date = It has no date

d-qp6cix3j = Projekt umbenennen
    .doc = Benennt ein Projekt um.

d-qp6cix3j-priority = Name

d-rz4omsam = Projekt löschen
    .doc = Löscht ein Projekt; seine Termine und Aufgaben fallen zurück in keines.

d-rz4omsam-p = P

d-tvabo7ia = Add a step
    .doc = Adds a step under a task, in its project, and opens the task if it was done: there is work under it now.

d-tvabo7ia-t = Um

d-tvabo7ia-title = Anfang

d-ei6hkzlc = Delete task
    .doc = Deletes a task and the steps under it.

d-ei6hkzlc-t = T

d-aaijm6cm = New project
    .doc = Starts a project.
    .w-new-project = New project

d-4qqhxlyv = Colour
    .doc = Sets the colour a project is drawn in.

d-4qqhxlyv-color = Um

d-yivccbzw = Move
    .doc = Moves a project one place earlier or later among the projects.

d-yivccbzw-p = Um

d-yivccbzw-up = Anfang

d-o2egikzm = Rename project
    .doc = Renames a project.

d-o2egikzm-name = Name

d-xtjwddq3 = Delete project
    .doc = Deletes a project; its events and tasks go back to none.

d-xtjwddq3-p = P

d-zaeu6beo = Only this event
    .doc = Moves one date of a repeating event: an event of its own stands in its place and the series keeps every other date.
    .w-it-does-not-repeat = It does not repeat
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-zaeu6beo-at = At

d-zaeu6beo-start = Start

d-zaeu6beo-end = End

d-bd3x7ejn = Cancel this date
    .doc = Cancels one date of a repeating event.
    .w-it-does-not-repeat = It does not repeat
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-bd3x7ejn-at = At

d-25bxqiqq = Restore this date
    .doc = Gives a cancelled or moved date of a series back.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-25bxqiqq-at = At

d-xagcihzh = This and following events
    .doc = Moves a repeating event from one date on: the series ends the day before, and a new one begins where it was moved to, with every date after it.
    .w-it-does-not-repeat = It does not repeat
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-xagcihzh-at = At

d-xagcihzh-start = Um

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

d-37r6seh2-name = Bis

d-ael6ndgj = Wiederholen bis
    .doc = Der letzte Tag, bis zu dem er sich wiederholt, als der Tag, den ein Zeitpunkt nennt.

d-ael6ndgj-e = Um

d-qg4djklq = Move to day
    .doc = Moves an event to a day, keeping the time of day it starts and how long it lasts, and its reminder at the same distance from its start.
    .w-it-is-on-that-day-already = It is on that day already
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-qg4djklq-day = Anzahl

d-ifyhqlkq = Change day
    .doc = Moves an event to the day a moment names, keeping the time of day it starts.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-ifyhqlkq-at = K

d-e7aimj6j = Change start
    .doc = Sets the time of day an event starts, its end keeping how long it lasts.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-e7aimj6j-at = Name

d-4fezz4oa = Change end
    .doc = Sets when an event ends.
    .w-an-event-ends-after-it-starts = An event ends after it starts
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-4fezz4oa-at = At

d-lbfkyak2 = Repeat
    .doc = Repeats an event every day, week, month or year from the day it stands on, on the days chosen of a week, and no longer on the days it named before.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-lbfkyak2-how = Status

d-z7u3n5xs = How often
    .doc = How often it repeats, in the unit it repeats in: every two weeks is 2.
    .w-it-repeats-at-least-once = It repeats at least once
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-z7u3n5xs-every = G

d-ckl3jlil = Repeat until
    .doc = The last day it repeats by, or none for a series that does not end.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-ckl3jlil-until = G

d-mm3tzg2x = Repeat until
    .doc = The last day it repeats by, as the day a moment names.

d-mm3tzg2x-at = At

d-y5e66bkb = Times
    .doc = How many times it repeats in all, or none for a series that does not end.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-y5e66bkb-count = Count

d-6tammo57 = Calendar
    .doc = Files an event under a calendar the person picks.
    .w-it-is-in-that-calendar-already = It is in that calendar already
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-6tammo57-k = K

d-td6wjjza = Invite
    .doc = Invites someone to an event, by the name and address the person gives.
    .w-that-is-not-an-email-address = That is not an email address
    .w-they-are-coming-already = They are coming already

d-td6wjjza-name = Name

d-td6wjjza-email = Eingetippt

d-luj3kjit = Male
    .doc = Wiederholt einen Termin so oft, wie die Person eingetippt hat.
    .w-an-answer-is-accepted-declined-or = Eine Antwort ist angenommen, abgelehnt oder vielleicht

d-luj3kjit-g = Eingetippt

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

d-ldaioj6y-typed = Zum Anfang erinnern

d-wka7zl52 = Times
    .doc = Repeats an event as many times as the person typed.
    .w-how-many-times-it-repeats-is = How many times it repeats is a number

d-wka7zl52-typed = Erinnerung aus

d-3u4tdxfq = Export calendar
    .doc = The calendar — every event and task it holds, or only one event — as an iCalendar file, for a person to save or an agent to hand on.

d-3u4tdxfq-e = Jede Woche am

d-zmfdk46t = Import calendar
    .doc = Imports the events and tasks of an iCalendar file, and answers how many it took: what a person's own file offers, and what another calendar exported.
    .w-could-not-read = Could not read { $name }: { $why }
    .w-imported-events-and-tasks = Imported { $len } events and { $len2 } tasks

d-zmfdk46t-file = Diesen Tag auslassen

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

d-7zcpw62t = Fälligkeit leeren

d-dihvjrzc = Nur dieser Termin

d-j6izcylf = Diesen Termin absagen

d-iozwj4om = Diesen Termin zurückgeben

d-32s7g4lk = Dieser und alle folgenden

d-wuqfd7rq = Auf Tag verschieben

d-2s2b3ygt = Tag ändern

d-3wxuaohk = Anfang ändern

d-4xtalqz5 = Ende ändern

d-ohbh2duz = Wiederholen

d-b76dfsqm = Wie oft

d-dulzqkmm = Wiederholen bis

d-m55bevrg = Anzahl

d-ghy7ry76 = In Kalender legen

d-7yrte5jx = Einladen

d-wiqo5ox3 = Antworten

d-5ecna76j = Jetzt erinnern

d-4mw4lmsy = Wie oft

d-wdmvy4o7 = Anzahl

d-iswyaeqi = Cancel occurrence~present

d-m4vjjhhy = Restore occurrence~present

d-bcgihhy5 = Move following~present

d-baljrz2g = Move to day~present

d-r6l53d5k = Terminzeile

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

d-5bfm4ii5 = Minuten

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
    .w-repeats = { $title } repeats
    .w-only-this-date = Only this date
    .w-this-and-all-following = This and all following
    .w-pick-a-day = Einen Tag wählen

d-uc6iwdpn = Agenda
    .w-new-event = New event
    .w-nothing-scheduled-in-the-next-three = Nothing scheduled in the next three weeks

d-nx5lspqo = Tasks
    .w-new-task-in = New task in { $name }
    .w-new-task = New task
    .w-nothing-in = Nothing in { $title }

d-u2sfuxhf = Task row
    .w-a-step-of = A step of { $title }

d-gtqqlwre = Termin

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
