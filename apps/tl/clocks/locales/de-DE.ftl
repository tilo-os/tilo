# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-clocks = Uhren

d-so-tilo-clocks-keywords = uhr, uhren, wecker, alarm, alarme, timer, stoppuhr, weltuhr, weltzeit, zeit, countdown, clock, alarm, timer, stopwatch

d-fncbrtzl = Weltuhr
    .doc = Eine Stadt, deren Zeit jemand mit der eigenen vergleicht.
    .noun = { $count ->
        [one] Weltuhr
       *[other] Weltuhren
    }
    .untitled = Stadt

d-kbeao6ct = Stadt
    .doc = Wie die Uhr heißt: der Name der Stadt.

d-g5vsvo2p = Zeitzone
    .doc = Die IANA-Zeitzone, nach der sie die Zeit hält, etwa `Asia/Tokyo`.

d-lqrbc6rz = Wecker
    .doc = Ein Wecker: eine Tageszeit, die Tage, an denen er klingelt, und wo er klingelt.
    .noun = { $count ->
        [one] Wecker
       *[other] Wecker
    }
    .untitled = Wecker

d-e6jhyy6h = Bezeichnung

d-cb6boqqg = Stunde
    .doc = Die Stunde, zu der er klingelt, in der Zeitzone der Person.

d-bvmw7lxl = Minute
    .doc = Die Minute, zu der er klingelt.

d-s4ppnbtv = Tage
    .doc = Die Tage, an denen er sich wiederholt; keine klingelt einmal, zur nächsten solchen Zeit.

d-pmxwuu3y = Wochentag
    .doc = Ein Wochentag, an dem sich ein Wecker wiederholt, Montag zuerst.

d-ohvi4cp6 = Montag

d-lzxlz4cd = Dienstag

d-ikwb6g2v = Mittwoch

d-3imexqqa = Donnerstag

d-jcyf2yea = Freitag

d-d5t3oavd = Samstag

d-qa4o45am = Sonntag

d-adpv3jco = Aktiviert
    .doc = Ob er überhaupt klingelt.

d-fbxt6ruu = Um
    .doc = Der Zeitpunkt, zu dem er als Nächstes klingelt; leer, solange er aus ist.

d-3bdkffqt = Klingelt auf
    .doc = Das Gerät, auf dem er klingelt, nach seiner Kennung; leer klingelt er auf jedem Gerät der Person.

d-uizteftt = Gesnoozt
    .doc = Ob das Klingeln um `at` ein Snooze ist: ein wiederholter Wecker, der gesnoozt wurde, klingelt erneut und fährt mit seinen Tagen fort.

d-lqrbc6rz-filter-hour = Stunde

d-lqrbc6rz-filter-minute = Minute

d-lqrbc6rz-filter-days = Tage

d-lqrbc6rz-filter-enabled = Aktiviert

d-lqrbc6rz-filter-at = Um

d-lqrbc6rz-filter-snoozed = Gesnoozt

d-oprnqjx3 = Timer
    .doc = Ein Timer: ein Countdown, der auf null läuft und dort klingelt.
    .noun = { $count ->
        [one] Timer
       *[other] Timer
    }
    .untitled = Timer

d-nhqnhguz = Bezeichnung

d-5b7nw57j = Sekunden
    .doc = Wie lange er herunterzählt, in Sekunden.

d-6anly4z4 = Endet um
    .doc = Der Zeitpunkt, an dem er endet; leer, solange er pausiert.

d-ufpse6mi = Übrig
    .doc = Was ihm pausiert übrig bleibt, in Sekunden.

d-oprnqjx3-filter-seconds = Sekunden

d-oprnqjx3-filter-ends_at = Endet um

d-oprnqjx3-filter-left = Übrig

d-euyscib3 = Runde
    .doc = Eine Runde, die die Stoppuhr festgehalten hat: die Zeit, bei der sie stand.
    .noun = { $count ->
        [one] Runde
       *[other] Runden
    }

d-mq5diuqb = Bei
    .doc = Die Zeit, bei der sie stand, in Sekunden.

d-euyscib3-filter-at = Bei

d-3io25rgu = Stoppuhr
    .doc = Die Stoppuhr: was sie gezählt hat, der Lauf, in dem sie ist, und die Runden, die sie festgehalten hat.
    .noun = { $count ->
        [one] Stoppuhr
       *[other] Stoppuhren
    }

d-yhijhjuh = Gezählt
    .doc = Was sie vor dem Lauf, in dem sie ist, gezählt hat, in Sekunden.

d-fg2gmze6 = Seit
    .doc = Der Zeitpunkt, zu dem ihr Lauf begann; leer, solange sie pausiert.

d-ngcmcd7g = Runden
    .doc = Die Zeiten, bei denen sie stand, als eine Runde festgehalten wurde, der Reihe nach.

d-3io25rgu-filter-elapsed = Gezählt

d-3io25rgu-filter-since = Seit

d-ppcjxruc = Uhren
    .doc = Die Uhren des Arbeitsbereichs.
    .noun = { $count ->
        [one] Uhr
       *[other] Uhren
    }

d-r2amisi7 = Weltuhren
    .doc = Die Städte, mit denen jemand die Zeit vergleicht.

d-gneumngu = Wecker
    .doc = Die gestellten Wecker, jeder auf dem Gerät, auf dem er klingelt.

d-kahapraa = Timer
    .doc = Die laufenden, pausierten oder abgelaufenen Timer.

d-rjh2x76r = Stoppuhr
    .doc = Die Stoppuhr, die der Arbeitsbereich hält: eine, auf null, bis sie gestartet wird.

d-ppcjxruc-filter-stopwatch = Stoppuhr

d-ppcjxruc-filter-active = Aktiv

d-ofkzixdx = Weltuhr hinzufügen
    .doc = Fügt die Uhr einer Stadt zum Arbeitsbereich hinzu.

d-ofkzixdx-city = Stadt

d-ofkzixdx-zone = Zeitzone

d-tp56af3m = Weltuhr entfernen
    .doc = Entfernt die Uhr einer Stadt.

d-tp56af3m-w = W

d-aozz4wlf = Neuer Wecker
    .doc = Stellt einen Wecker: zur Tageszeit `when`, an den Tagen, an denen er sich wiederholt, klingelnd auf `rings_on`, auf jedem Gerät der Person, wenn nichts genannt ist.
    .w-an-alarm-rings-at-an-hour = Ein Wecker klingelt zu einer Stunde von 0 bis 23
    .w-an-alarm-rings-at-a-minute = Ein Wecker klingelt zu einer Minute von 0 bis 59

d-aozz4wlf-label = Bezeichnung

d-aozz4wlf-hour = Stunde

d-aozz4wlf-minute = Minute

d-aozz4wlf-days = Tage

d-aozz4wlf-rings_on = Klingelt auf

d-fivi3e67 = Wecker bearbeiten
    .doc = Ändert die Worte, die Zeit und die Tage eines Weckers.
    .w-an-alarm-rings-at-an-hour = Ein Wecker klingelt zu einer Stunde von 0 bis 23
    .w-an-alarm-rings-at-a-minute = Ein Wecker klingelt zu einer Minute von 0 bis 59

d-fivi3e67-label = Bezeichnung

d-fivi3e67-hour = Stunde

d-fivi3e67-minute = Minute

d-fivi3e67-days = Tage

d-x2tarjxv = Wecker hinzufügen~present

d-u4chxfdj = Wecker bearbeiten~present

d-sqirq543 = Wecker ein- oder ausschalten
    .doc = Schaltet einen Wecker ein oder aus: aus klingelt er nicht; ein klingelt er als Nächstes zu seiner Zeit.

d-sqirq543-enabled = Aktiviert

d-uxdphvkl = Gerät des Weckers
    .doc = Nennt das Gerät, auf dem ein Wecker klingelt: die Kennung eines Geräts oder nichts für jedes Gerät der Person.

d-uxdphvkl-rings_on = Klingelt auf

d-eencngbt = Tag umschalten
    .doc = Wiederholt einen Wecker an `d` oder hört damit auf: ein genannter Tag klingelt, ein nicht genannter wird übersprungen.

d-eencngbt-d = T

d-mgfcysje = Wecker entfernen
    .doc = Entfernt einen Wecker.

d-mgfcysje-a = W

d-2uk2bvmz = 5 Minuten snoozen
    .doc = Snoozt den jetzt klingelnden Wecker: er klingelt in fünf Minuten erneut.

d-7lw2tfxp = Wecker jetzt klingeln lassen
    .doc = Lässt einen Wecker klingeln, wenn sein Moment kommt, auf dem genannten Gerät, mit oder ohne Fenster.
    .w-it-is = Es ist { $hour }:{ $pad2 }

d-7anuyebc = Timer starten
    .doc = Startet einen Timer über `seconds`.
    .w-a-timer-needs-a-duration-greater = Ein Timer braucht eine Dauer größer als null

d-7anuyebc-label = Bezeichnung

d-7anuyebc-seconds = Sekunden

d-latbht2w = Timer pausieren
    .doc = Pausiert einen laufenden Timer, wo er steht.

d-2n5kbb4o = Timer fortsetzen
    .doc = Setzt einen pausierten Timer fort oder startet einen abgelaufenen neu.

d-eepdn3h5 = Minute hinzufügen
    .doc = Fügt einem Timer eine Minute hinzu, laufend oder pausiert.

d-nwtaannw = Timer entfernen
    .doc = Entfernt einen Timer.

d-nwtaannw-t = T

d-gidp5so6 = Timer klingeln lassen
    .doc = Lässt einen Timer klingeln, wenn er endet, auf jedem Gerät der Person, mit oder ohne Fenster.
    .w-the-timer-finished = Der Timer ist abgelaufen

d-knkom4sf = Stoppuhr starten
    .doc = Startet die Stoppuhr.

d-kcycaaaa = Stoppuhr pausieren
    .doc = Pausiert die Stoppuhr, wo sie steht.

d-7twb2otm = Runde
    .doc = Nimmt eine Runde: die Zeit, bei der die Stoppuhr steht.

d-qcjegi7p = Stoppuhr zurücksetzen
    .doc = Setzt die Stoppuhr zurück: sie steht auf null und ist gestoppt, ohne Runden.

d-tv3yomxx = Timer hinzufügen~present

d-qobxx5dl = Uhrenseite
    .w-clocks = Uhren
    .w-world = Welt
    .w-alarms = Wecker
    .w-timers = Timer
    .w-stopwatch = Stoppuhr

d-sm7xxyrn = Weltseite
    .w-add-a-city = Eine Stadt hinzufügen
    .w-search-a-city-or-a-zone = Eine Stadt oder Zeitzone suchen
    .w-here = Hier
    .w-no-cities-yet-add-one-to = Noch keine Städte. Füge eine hinzu, um Zeiten auf einen Blick zu vergleichen.

d-inqk2nou = Weckerseite
    .w-no-alarms-yet-set-one-for = Noch keine Wecker. Stelle einen für eine kommende Zeit.
    .w-edit = { $label } bearbeiten
    .w-what-is-this-alarm-for = Wofür ist dieser Wecker?
    .w-hour = Stunde
    .w-h = Std.
    .w-minute = Minute
    .w-min = Min.
    .w-save-changes = Änderungen speichern
    .w-cancel = Abbrechen
    .w-new-alarm = Neuer Wecker
    .w-add-alarm = Wecker hinzufügen
    .w-rings-on = Klingelt am
    .w-repeat-on = Wiederholen am { $day_name }
    .w-repeat-days = Wiederholungstage
    .w-this-device = Dieses Gerät
    .w-every-device = Jedes Gerät
    .w-all-of-yours = Alle deine

d-g2agvnl7 = Timerseite
    .w-no-timers-yet = Noch keine Timer
    .w-quick-starts = Schnellstart
    .w-tea-5-min = Tee (5 Min.)
    .w-break-10-min = Pause (10 Min.)
    .w-focus-25-min = Fokus (25 Min.)
    .w-laundry-45-min = Wäsche (45 Min.)
    .w-custom-timer = Eigener Timer
    .w-what-is-this-timer-for = Wofür ist dieser Timer?
    .w-hours = Stunden
    .w-h = Std.
    .w-minutes = Minuten
    .w-min = Min.
    .w-seconds = Sekunden
    .w-s = s
    .w-use-whole-numbers-hours-0-99 = Nutze ganze Zahlen: Stunden 0–99 und Minuten und Sekunden 0–59.
    .w-start-timer = Timer starten
    .w-paused = Angehalten
    .w-1-min = +1 Min.
    .w-resume = Fortsetzen
    .w-pause = Pause
    .w-remove = Entfernen

d-k3yetx4r = Stoppuhrseite
    .w-stopwatch = Stoppuhr
    .w-start = Start
    .w-pause = Pause
    .w-lap = Runde
    .w-reset = Zurücksetzen

d-hnwcprw3 = Aktiv
    .doc = Ob ein Timer läuft oder pausiert, oder die Stoppuhr läuft oder gezählt hat: die Uhren stehen dann in der Leiste, in den Schnelleinstellungen und auf dem Sperrbildschirm jedes Geräts, und die Person lässt sie als Karte auf den Geräten schweben, die sie wählt.

d-z3jkw2zq = Live
    .w-resume = Fortsetzen
    .w-pause = Pause
    .w-add-a-minute = Eine Minute dazu
    .w-remove = Entfernen
    .w-stopwatch = Stoppuhr
    .w-start = Start
    .w-lap = Runde
    .w-reset = Zurücksetzen
