# Kalender auf Deutsch. Keys are the declarations' stable ids (app.ids); the source's words are written from the package's sources.

d-so-tilo-calendar = Kalender

d-so-tilo-calendar-keywords = kalender, termine, zeitplan, woche, monat, agenda, verabredungen, besprechungen, einladungen, caldav, ics, gemeinsamer kalender

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

d-odmyabgk = Konto
    .doc = One of the workspace's calendar accounts, and how its server answers: the window says a server that cannot be reached, a change it will not take, and when its calendars last came.
    .noun = { $count ->
        [one] account
       *[other] accounts
    }

d-f44j5f26 = Schlüssel
    .doc = The account's id, as the workspace settings name it.

d-gxn236ef = Adresse
    .doc = The name it signs in with.

d-rfeeyuff = Name
    .doc = What it is called.

d-turzovgi = Server
    .doc = The server address the person typed.

d-vupfntcd = Schreibgeschützt
    .doc = Whether nothing is ever written to its server.

d-cgmndfzg = Störung
    .doc = What the account's server said when it could not be reached, in its words: empty while it stands well.

d-yfntdxb7 = Online
    .doc = Whether the server answers.

d-5ebgjcbp = Wird geprüft
    .doc = Whether the server is being asked now, so the window says what is under way.

d-qmpjl3oc = Abgerufen
    .doc = When the account's calendars were last taken in from its server, as the clock the person read it at: 1970 when they never were.

d-odmyabgk-filter-read_only = Schreibgeschützt

d-odmyabgk-filter-online = Online

d-odmyabgk-filter-checking = Wird geprüft

d-odmyabgk-filter-fetched = Abgerufen

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
    .doc = Was der Server dazu sagt: seine Beschreibung, als Text.

d-dmlhzgmz = Inhalt
    .doc = Was die Person dazu schreibt, als Rich Text.

d-mvt5vjkt = Location

d-gqmucsa7 = Kategorien
    .doc = Die eigenen Worte der Person dafür, was für ein Termin es ist.

d-igx7zgee = Wiederholung
    .doc = Was er wiederholt, in iCalendar-Worten (zum Beispiel wöchentlich); leer für einen, der es nicht tut.

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

d-lubytvuv = Konten
    .doc = The calendar accounts the workspace settings hold, each saying how its server answers.

d-jum24lyb = Tasks
    .doc = Every task the calendar keeps, in the order people put them.

d-japv422q = Hoch

d-i5l2mswn = Farbe
    .doc = Die Farben, die ein Kalender, ein Termin oder ein Projekt tragen kann, jede ein Platz im Thema.

d-53uahg6d-filter-anchor_at = Indigo

d-prcfqlmt = Belegt
    .doc = Eine Zeit, zu der die Kalender des Arbeitsbereichs belegt sind: wann sie beginnt und endet und ob sie den ganzen Tag einnimmt.
    .noun = { $count ->
        [one] belegte Zeit
       *[other] belegte Zeiten
    }

d-76f4x76t = Beginn

d-25wuuu2f = Ende

d-f5uwvzvm = Ganztägig

d-prcfqlmt-filter-start = Beginn

d-prcfqlmt-filter-end = Ende

d-prcfqlmt-filter-all_day = Ganztägig

d-qj3kjrcv = Belegte Zeiten
    .doc = Wann die Kalender des Arbeitsbereichs zwischen zwei Tagen belegt sind, beide eingeschlossen: jedes Vorkommen jedes nicht abgesagten Termins, wie der Kalender es zeigt, in der Reihenfolge seines Beginns.

d-qj3kjrcv-from = Von

d-qj3kjrcv-to = Bis

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
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-obvjd33k-color = Color

d-jxqvsawj = Its calendar's colour
    .doc = Takes an event's own colour off: it is drawn in its calendar's.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-2vx7zp6l = Colour
    .doc = Sets the colour a calendar is drawn in.

d-2vx7zp6l-color = Color

d-nlrpnve5 = Categories
    .doc = Sets the categories an event is filed under in its person's words.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-its-person-invites-others-to-that = Zu diesem Termin lädt seine Person andere ein, also ordnet sie ihn selbst ein: Das Einordnen schreibt ihn auf dem Server neu, und das ginge an die Eingeladenen

d-nlrpnve5-categories = Alle

d-se6shkk2 = Time zone
    .doc = Sets the zone an event's wall times are read in: `Europe/Berlin`, `UTC`, or `floating` for the same time of day everywhere.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-a-zone-is-floating-utc-or = A zone is floating, UTC, or a name such as Europe/Berlin
    .w-that-is-not-a-time-zone = That is not a time zone this build knows
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-se6shkk2-zone = Bis

d-yollo4f5 = Add a category
    .doc = Files an event under one more category.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-a-category-is-a-few-words = A category is a few words
    .w-its-person-invites-others-to-that = Zu diesem Termin lädt seine Person andere ein, also ordnet sie ihn selbst ein: Das Einordnen schreibt ihn auf dem Server neu, und das ginge an die Eingeladenen

d-yollo4f5-category = Category

d-uwnaecvd = Remove a category
    .doc = Takes a category off an event.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-its-person-invites-others-to-that = Zu diesem Termin lädt seine Person andere ein, also ordnet sie ihn selbst ein: Das Einordnen schreibt ihn auf dem Server neu, und das ginge an die Eingeladenen

d-uwnaecvd-category = Category

d-5eqjpk6o = Organiser
    .doc = Makes someone the organiser of an event.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-5eqjpk6o-who = Tag

d-qdqgaukz = Diesen Tag zurückgeben
    .doc = Gibt einen ausgelassenen Termin einer Wiederholung zurück.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-uif2dwsr = Before
    .doc = Reminds them a given number of minutes before it starts.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-a-reminder-is-before-the-start = A reminder is before the start
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-uif2dwsr-minutes = Minutes

d-vjp2n4ek = No reminder
    .doc = Tells nobody, once more.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-it-has-no-reminder = It has no reminder
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-fowc4rav = Repeat
    .doc = Repeats an event, its rule in iCalendar's words, how often, and where it ends.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-fowc4rav-how = Titel

d-fowc4rav-every = Every

d-fowc4rav-weekdays = Erledigt

d-fowc4rav-until = Until

d-fowc4rav-count = Fällig

d-q23cd64i = Every week on
    .doc = Repeats an event on chosen days of the week from the day it stands on.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-choose-the-days-it-repeats-on = Choose the days it repeats on
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-q23cd64i-days = Days

d-vk663ftr = An diesem Tag wiederholen
    .doc = Fügt einen Wochentag zu einer wöchentlichen Wiederholung hinzu oder nimmt ihn heraus: die gewählten Tage sind die Tage der Regel, und die Wiederholung behält den Tag, an dem sie endet, wenn sie an einem endet.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist schreibgeschützt, deshalb wird nichts darin geändert
    .w-an-event-repeats-on-at-least = Ein Termin wiederholt sich an mindestens einem Tag
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-vk663ftr-day = Day

d-e64m52e3 = Does not repeat
    .doc = Stops an event repeating from the day it stands on.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-it-does-not-repeat = It does not repeat
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-yeztwom7 = Skip this date
    .doc = Skips one date of a repeating event.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-it-does-not-repeat = It does not repeat
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-yeztwom7-day = Day

d-ows6cr6i = Restore this date
    .doc = Gives a skipped date of a repeating event back.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

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
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-it-does-not-repeat = It does not repeat
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-zaeu6beo-at = At

d-zaeu6beo-start = Start

d-zaeu6beo-end = End

d-bd3x7ejn = Cancel this date
    .doc = Cancels one date of a repeating event.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-it-does-not-repeat = It does not repeat
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-bd3x7ejn-at = At

d-25bxqiqq = Restore this date
    .doc = Gives a cancelled or moved date of a series back.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-25bxqiqq-at = At

d-xagcihzh = This and following events
    .doc = Moves a repeating event from one date on: the series ends the day before, and a new one begins where it was moved to, with every date after it.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-it-does-not-repeat = It does not repeat
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

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

d-hi3fbhxf = Erneut synchronisieren
    .doc = Asks one account's server for its calendars now: what "Sync again" over its notice does.

d-rox4evo5 = New calendar
    .doc = Adds a calendar of the workspace's own.
    .w-new-calendar = New calendar

d-37r6seh2 = Rename calendar
    .doc = Renames a calendar.

d-37r6seh2-name = Bis

d-ael6ndgj = Wiederholen bis
    .doc = Der letzte Tag, bis zu dem er sich wiederholt, als der Tag, den ein Zeitpunkt nennt.
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-ael6ndgj-e = Um

d-awfwvmso = Diesen Termin verwerfen
    .doc = Throws away an event nothing was written in yet: what a person's New event leaves when they decide against it.
    .w-the-event-has-something-in-it = Der Termin enthält bereits etwas
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-awfwvmso-e = E

d-qg4djklq = Move to day
    .doc = Moves an event to a day, keeping the time of day it starts and how long it lasts, and its reminder at the same distance from its start.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-it-is-on-that-day-already = It is on that day already
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-qg4djklq-day = Anzahl

d-ifyhqlkq = Change day
    .doc = Moves an event to the day a moment names, keeping the time of day it starts.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-ifyhqlkq-at = K

d-e7aimj6j = Change start
    .doc = Sets the time of day an event starts, its end keeping how long it lasts.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-e7aimj6j-at = Name

d-4fezz4oa = Change end
    .doc = Sets when an event ends.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-an-event-ends-after-it-starts = An event ends after it starts
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-4fezz4oa-at = At

d-zrdjom25 = Endtag verschieben
    .doc = Beendet einen Termin an dem Tag, den ein Zeitpunkt nennt, und behält dabei die Uhrzeit seines Endes: ein Tag nach seinem Beginn lässt ihn jeden Tag dazwischen dauern, und ein ganztägiger Termin endet um Mitternacht nach seinem letzten Tag, dem Tag, an dem er erscheint.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-zrdjom25-at = Zeitpunkt

d-674w4zdj = Endet am Tag
    .doc = Beendet einen Termin an einem Tag und behält dabei die Uhrzeit seines Endes, wie es „Endtag verschieben“ mit dem Tag tut, den ein Zeitpunkt nennt.
    .w-an-event-does-not-end-before = Ein Termin endet nicht vor seinem Beginn
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-674w4zdj-end_day = Letzter Tag

d-cbhbqh5c = Endet am Tag~present

d-lbfkyak2 = Repeat
    .doc = Repeats an event every day, week, month or year from the day it stands on, on the days chosen of a week, and no longer on the days it named before.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-lbfkyak2-how = Status

d-z7u3n5xs = How often
    .doc = How often it repeats, in the unit it repeats in: every two weeks is 2.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-it-repeats-at-least-once = It repeats at least once
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-z7u3n5xs-every = G

d-ckl3jlil = Repeat until
    .doc = The last day it repeats by, or none for a series that does not end.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-ckl3jlil-until = G

d-mm3tzg2x = Repeat until
    .doc = The last day it repeats by, as the day a moment names.

d-mm3tzg2x-at = At

d-y5e66bkb = Times
    .doc = How many times it repeats in all, or none for a series that does not end.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-y5e66bkb-count = Count

d-6tammo57 = Calendar
    .doc = Files an event under a calendar the person picks.
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert
    .w-it-is-in-that-calendar-already = It is in that calendar already
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat

d-6tammo57-k = K

d-td6wjjza = Invite
    .doc = Invites someone to an event, by the name and address the person gives.
    .w-that-is-not-an-email-address = That is not an email address
    .w-they-are-coming-already = They are coming already
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-td6wjjza-name = Name

d-td6wjjza-email = Eingetippt

d-luj3kjit = Male
    .doc = Wiederholt einen Termin so oft, wie die Person eingetippt hat.
    .w-an-answer-is-accepted-declined-or = Eine Antwort ist angenommen, abgelehnt oder vielleicht
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-luj3kjit-g = Eingetippt

d-luj3kjit-status = Status

d-kscklssi = Die Einladung beantworten
    .doc = Beantwortet die eigene Einladung der Person zu einem Termin eines Kontos und meldet es dem Organisator über den Server des Kontos: was die angefragte Antwort abschließt.
    .w-only-your-own-invitation-is-answered = Nur die eigene Einladung wird am Server beantwortet

d-kscklssi-g = G

d-kscklssi-said = Said

d-gtgyzaen = Remove guest
    .doc = Takes someone off an event's guests: nothing is sent to them.
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat
    .w-this-calendar-is-connected-for-reading = Dieser Kalender ist nur zum Lesen verbunden, darin wird nichts geändert

d-gtgyzaen-g = G

d-fesqobfx = Invite by mail
    .doc = Starts a message inviting a guest, the event's calendar invitation carried beside it: what Mail shows the person, to read and send.
    .w-add-a-mail-account-in-settings = Füge zuerst ein Mailkonto in den Einstellungen hinzu
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
    .export = iCalendar
    .doc = Der Kalender (jeder Termin und jede Aufgabe darin, oder nur ein Termin) als iCalendar-Datei, zum Speichern durch eine Person oder zum Weitergeben durch einen Agenten.

d-3u4tdxfq-e = Jede Woche am

d-va2m7dny = Eine Einladung übernehmen
    .doc = Takes an invitation into the calendar: the event it asks for is kept by the account's server where one can be written to it, so the organiser hears the answer, and by this workspace otherwise.
    .w-that-is-not-a-calendar-invitation = Das ist keine Kalendereinladung
    .w-added-to = Added to { $name }
    .w-that-entry-is-its-person-s = Dieser Termin gehört seiner Person: Ein Agent ändert nur, was er selbst eingetragen hat
    .w-only-its-sender-changes-or-calls = Nur ihr Absender ändert oder sagt eine Einladung ab, und diese nennt einen anderen

d-va2m7dny-text = Text

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

d-xzgjn4zd = Priority name
    .w-high = Hoch
    .w-medium = Mittel
    .w-low = Niedrig

d-vlba7mdp = Weekday name
    .w-monday = Montag
    .w-tuesday = Dienstag
    .w-wednesday = Mittwoch
    .w-thursday = Donnerstag
    .w-friday = Freitag
    .w-saturday = Samstag
    .w-sunday = Sonntag

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

d-cwlt5hoh = Toggle weekday~present

d-ghy7ry76 = In Kalender legen

d-7yrte5jx = Einladen

d-wiqo5ox3 = Antworten

d-5ecna76j = Jetzt erinnern

d-4mw4lmsy = Wie oft

d-wdmvy4o7 = Anzahl

d-iswyaeqi = Cancel occurrence~present

d-m4vjjhhy = Restore occurrence~present

d-bcgihhy5 = Move following~present

d-bngncbmz = Discard draft~present

d-baljrz2g = Move to day~present

d-r6l53d5k = Terminzeile

d-u2vnroei = Change start~present

d-c3a3dxxn = Change end~present

d-6pmulvk5 = Endtag verschieben~present

d-7cffrvhe = Repeat~present

d-v4wdiwvk = Repeat every~present

d-rfmgr4il = Repeat until~present

d-c3653wxf = Repeat count~present

d-umaixua3 = File in~present

d-v36oxy7z = Invite~present

d-3l63az42 = Answer~present

d-gocme73g = Answer and tell~present

d-dtektxut = Per Mail einladen~present

d-eapd2aju = Remind now~present

d-m2in7j2d = Repeat every text~present

d-5bfm4ii5 = Minuten

d-qfoc7u6b = Take invitation~present

d-nd2mftjh = Window
    .w-calendar = Calendar
    .w-sync-calendars = Kalender synchronisieren
    .w-week = Woche
    .w-month = Monat
    .w-agenda = Agenda
    .w-day = Tag
    .w-tasks = Aufgaben
    .w-task-lists = Aufgabenlisten
    .w-inbox = Eingang
    .w-today = Heute
    .w-upcoming = Demnächst
    .w-done = Erledigt
    .w-projects = Projects
    .w-calendars = Calendars
    .w-all-calendars = All calendars
    .w-show = { $name } anzeigen
    .w-hide = { $name } ausblenden
    .w-previous-day = Previous day
    .w-next-day = Next day
    .w-previous-month = Previous month
    .w-next-month = Next month
    .w-previous-week = Previous week
    .w-next-week = Next week
    .w-pick-a-day = Einen Tag wählen
    .w-new-event-such-as-lunch-with = Neuer Termin, etwa Mittagessen mit Sam Freitag um 13 Uhr
    .w-repeats = { $title } repeats
    .w-only-this-date = Only this date
    .w-this-and-all-following = This and all following
    .w-quick-add = Quick add
    .w-add-event = Add event
    .w-cancel = Cancel
    .w-what-and-when = What and when
    .w-lunch-with-sam-friday-1pm = Lunch with Sam friday 1pm
    .w-new-event = New event
    .w-keyboard-shortcuts = Keyboard shortcuts
    .w-jump = Jump
    .w-close = Close
    .w-n = N
    .w-shift-n = Shift+N
    .w-left-right = Left, Right
    .w-previous-or-next-day = Previous or next day
    .w-up-down = Up, Down
    .w-previous-or-next-week = Previous or next week
    .w-delete = Delete
    .w-delete-the-event-on-its-page = Delete the event on its page
    .w-esc = Esc
    .w-show-or-close-this-reference = Show or close this reference

d-li3ayrd7 = Woche

d-dykpzvq6 = Monat

d-clvn3fdl = Tag

d-vcuynr7i = Agenda

d-pfdg5ohd = Aufgaben

d-dpwhqftd = Alle Kalender

d-yhmmif6d = Aufgabeneingang

d-oijmpeik = Heutige Aufgaben

d-afxicbuq = Anstehende Aufgaben

d-ld4l5oag = Erledigte Aufgaben

d-kergju7m = Account notices
    .w-sync-again = Erneut synchronisieren
    .w-can-t-save-what-fetched = Can't save what { $name } fetched
    .w-can-t-reach = Keine Verbindung zu { $name }
    .w-last-fetched = Zuletzt abgerufen { $fetched }
    .w-syncing = Synchronisiere { $who } …
    .w-is-no-longer-kept = { $name } wird nicht mehr geführt
    .w-its-account-is-not-configured-any = Ihr Konto ist nicht mehr eingerichtet. Richte es in den Einstellungen wieder ein, sonst bleiben die Termine, wie sie sind.

d-uc6iwdpn = Agenda
    .w-new-event = Neuer Termin
    .w-previous-week = Vorherige Woche
    .w-today = Heute
    .w-next-week = Nächste Woche
    .w-nothing-scheduled-in-the-next-three = In den nächsten drei Wochen ist nichts geplant
    .w-all-day = Ganztägig

d-nx5lspqo = Tasks
    .w-new-task-in = New task in { $name }
    .w-new-task = New task
    .w-nothing-in = Nothing in { $title }
    .w-what-to-do = What to do
    .w-anything-worth-remembering-about-it = Alles, was dazu zu merken ist.
    .w-inbox = Eingang
    .w-due = Fällig
    .w-today = Heute
    .w-tomorrow = Verschieben
    .w-next-week = Next week
    .w-no-date = No date
    .w-priority = Priorität
    .w-no-priority = Jeden Tag
    .w-low = Jede Woche
    .w-medium = Jeden Monat
    .w-high = Jedes Jahr
    .w-steps = Steps
    .w-add-a-step = Add a step

d-hmue7ipj = Agenda task

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
    .w-this-event-is-still-empty = Dieser Termin ist noch leer
    .w-fill-it-in-or-discard-it = Fülle ihn aus, oder verwirf ihn, dann bleibt nichts zurück.
    .w-discard = Verwerfen
    .w-event-title = Event title
    .w-what-is-it-about = What is it about?
    .w-day = Day
    .w-last-day = Letzter Tag
    .w-starts = Starts
    .w-ends = Ends
    .w-add-a-place = Add a place
    .w-all-day = All day
    .w-overlaps = Overlaps { $other }
    .w-my-calendar = My calendar
    .w-colour = Colour
    .w-its-calendar-s-colour = Its calendar's colour
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
    .w-15-minutes-before = 15 Minuten vorher
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
    .w-cancelled = Ursprünglicher Anfang
    .w-this-date = Dieses Datum
    .w-restore-this-date = Restore this date
    .w-cancel-this-date = Cancel this date
    .w-skip-this-date = Skip this date
    .w-filed-under = Filed under
    .w-remove-a-category = Remove a category
    .w-add-a-category = Add a category
    .w-organiser-s-email-address = Organiser's email address
    .w-guests = Guests
    .w-accepted = Zugesagt
    .w-maybe = Vielleicht
    .w-declined = Abgesagt
    .w-awaiting-response = Antwort ausstehend
    .w-answer = Male
    .w-invite-by-mail = Invite by mail
    .w-remove-guest = Remove guest
    .w-from-contacts = Aus den Kontakten
    .w-invite-by-email-address = Invite by email address

d-qy4w4m3w = Event page
    .w-this-event-is-still-empty = Dieser Termin ist noch leer
    .w-fill-it-in-or-discard-it = Fülle ihn aus, oder verwirf ihn, dann bleibt nichts zurück.
    .w-discard = Verwerfen
    .w-event-title = Event title
    .w-what-is-it-about = What is it about?
    .w-day = Day
    .w-last-day = Letzter Tag
    .w-starts = Starts
    .w-ends = Ends
    .w-add-a-place = Add a place
    .w-all-day = All day
    .w-overlaps = Overlaps { $other }
    .w-my-calendar = My calendar
    .w-colour = Colour
    .w-its-calendar-s-colour = Its calendar's colour
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
    .w-15-minutes-before = 15 Minuten vorher
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
    .w-cancelled = Ursprünglicher Anfang
    .w-this-date = Dieses Datum
    .w-restore-this-date = Restore this date
    .w-cancel-this-date = Cancel this date
    .w-skip-this-date = Skip this date
    .w-filed-under = Filed under
    .w-remove-a-category = Remove a category
    .w-add-a-category = Add a category
    .w-organiser-s-email-address = Organiser's email address
    .w-guests = Guests
    .w-accepted = Zugesagt
    .w-maybe = Vielleicht
    .w-declined = Abgesagt
    .w-awaiting-response = Antwort ausstehend
    .w-answer = Male
    .w-invite-by-mail = Invite by mail
    .w-remove-guest = Remove guest
    .w-from-contacts = Aus den Kontakten
    .w-invite-by-email-address = Invite by email address

d-rpmzv4ld = Entfernen~present

d-vajspafs = Ausladen~present

d-h6y3yzla = Quick add
    .doc = Makes an event of one line a person types: "Lunch with Sam friday 1pm" starts lunch with Sam at one on Friday, "Workshop tomorrow at 10 for 90 min" runs ninety minutes from ten tomorrow, "Trip on 2026-11-03 at 8:00" lands on that day, and "Dinner tonight" is at eight.

d-h6y3yzla-line = Line

d-rbpe5fsh = Quick said
    .w-new-event = New event
