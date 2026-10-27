# Maps — German locale (de-DE).

## Verlauf: was jede Änderung an der gemeinsamen Karte getan hat

maps-place-saved = { $new ->
    [true] { $item } zu den gespeicherten Orten hinzugefügt
   *[false] { $item } aus den gespeicherten Orten entfernt
}
maps-place-name = { $item } umbenannt
maps-place-category = Geändert, was { $item } ist
maps-place-address = Adresse von { $item } geändert
maps-workspace-places = Die Orte geändert
maps-workspace-mode = Fortbewegung auf { $new ->
    [car] Auto
    [transit] öffentliche Verkehrsmittel
    [bike] Fahrrad
    [foot] zu Fuß
   *[other] { $new }
 } gesetzt
maps-workspace-transit-time = Abfahrts- oder Ankunftszeit der Verbindungen geändert
maps-accept-places = Von einer Suche gefundene Orte übernommen
maps-commit-search = Nach { $query } gesucht
maps-clear-recents = Die letzten Suchen gelöscht
maps-accept-route = Eine Route nach { $dest_name } geplant
maps-clear-route = Die Route entfernt
maps-accept-transit = Verbindungen nach { $dest_name } geplant
maps-clear-transit = Die Verbindungen entfernt

## Was schiefging, für die Person gesagt (der Text des Dienstes wird
## protokolliert, nie gezeigt)

maps-failure = { $what } konnte nicht abgeschlossen werden. { $reason }
maps-failure-unreachable = Es gibt keine Verbindung zum Kartendienst. Prüfe deine Verbindung und versuche es erneut.
maps-failure-timeout = Der Kartendienst hat zu lange gebraucht. Versuche es erneut.
maps-failure-too-many = Der Kartendienst begrenzt, wie oft dieses Gerät fragen darf. Versuche es gleich noch einmal.
maps-failure-service = Der Kartendienst hat gerade Probleme. Versuche es gleich noch einmal.
maps-failure-refused = Der Kartendienst hat diese Anfrage nicht angenommen. Versuche einen anderen Ort oder eine andere Route.
maps-failure-other = Zwischen hier und dem Kartendienst ist etwas schiefgegangen. Versuche es erneut.

## Wo du bist, was eine Suche gefunden hat und was die Karte tut

maps-use-my-location = Meinen Standort verwenden
maps-locating = Standort wird ermittelt…
maps-location-unavailable = Standort nicht verfügbar
maps-search-results = { $count } { $count ->
    [one] Ergebnis
   *[other] Ergebnisse
 } für „{ $query }“
maps-search-no-results = Keine Ergebnisse für „{ $query }“
maps-route-updated = Route aktualisiert
maps-route-choose-destination = Wähle ein Ziel, um die Route zu sehen
maps-route-another = Andere Route
maps-route-number = Route { $number } von { $count }
maps-departures-updated = Abfahrten aktualisiert
maps-no-transit = Keine Verbindungen gefunden
maps-offline-downloading = { $count } Kacheln werden für die Offline-Nutzung heruntergeladen…
maps-offline-downloading-named = { $count } Kacheln für { $name } werden heruntergeladen…
maps-offline-stopped = Download gestoppt; die bereits gespeicherten Kacheln bleiben verfügbar.

## Offline-Karten: das Blatt, das einen Kartenausschnitt für später speichert

maps-offline-title = Offline-Karten
maps-offline-area = Bereich zum Herunterladen
maps-offline-close = Offline-Karten schließen
maps-offline-search-placeholder = Nach einer Stadt oder einem Ort suchen
maps-offline-zoom-levels = Zoomstufen
maps-offline-fewer-zoom-levels = Weniger Zoomstufen
maps-offline-more-zoom-levels = Mehr Zoomstufen
maps-offline-download-view = Diese Ansicht herunterladen
maps-offline-download-named = { $name } herunterladen
maps-offline-download-area = Ausgewählten Bereich herunterladen
maps-offline-stop = Download stoppen
maps-offline-delete-area = Heruntergeladenen Bereich löschen
maps-offline-clear = Offline-Kartencache leeren
maps-offline-clear-short = Offline-Karten leeren
maps-offline-downloaded-areas = Heruntergeladene Bereiche
maps-offline-empty = Suche einen Ort oder lade herunter, was die Karte gerade zeigt.
maps-offline-progress = { $done } von { $total } Kacheln werden heruntergeladen…
maps-offline-stopped-count = Nach { $total } Kacheln gestoppt; sie bleiben offline verfügbar
maps-offline-done-failed = { $total } Kacheln geladen, { $failed } fehlgeschlagen
maps-offline-done = { $total } Kacheln geladen
maps-offline-estimate = Etwa { $size } zum Herunterladen
maps-offline-stored = { $size } auf diesem Gerät
maps-offline-refused = Angesehene Orte bleiben auf diesem Gerät, damit sie offline weiter angezeigt werden.

## Die Oberfläche der Karte

maps-app-name = Karten
maps-search-places = Orte suchen
maps-expand-panel = Bereich ausklappen
maps-collapse-panel = Bereich einklappen
maps-back-to-places = Zurück zu den Orten
maps-travel-by = Reisen mit { $mode }
maps-open-place = Ort öffnen
maps-open-named-place = { $name } öffnen
maps-open-nearby-place = Ort in der Nähe öffnen
maps-close-place-details = Ortsdetails schließen
maps-directions-to-place = Route zu diesem Ort
maps-search-again = Erneut nach { $query } suchen
maps-clear-search-history = Suchverlauf löschen
maps-hide-places = Orte ausblenden
maps-show-places = Orte einblenden
maps-directions = Route
maps-transit = Öffentliche Verkehrsmittel
maps-save-or-unsave = Ort speichern oder nicht mehr speichern
maps-save = Speichern
maps-saved = Gespeichert
maps-clear-route = Route entfernen
maps-leave-at = Abfahrt um
maps-arrive-by = Ankunft bis
maps-earlier = Früher
maps-later = Später
maps-reset-to-now = Auf jetzt zurücksetzen
maps-journey-option = Verbindung { $number }
maps-more-map-options = Weitere Kartenoptionen
maps-history = Verlauf
maps-zoom-in = Vergrößern
maps-zoom-out = Verkleinern
maps-map-layers = Kartenebenen
maps-end-navigation = Navigation beenden
maps-no-traffic-data = Keine Verkehrsdaten
maps-traffic = Verkehr
maps-menu = Menü
maps-more = Mehr
maps-active-navigation = Aktive Navigation
maps-end = Beenden
maps-continue-on-route = Auf der Route weiterfahren
maps-address = Adresse
maps-hours = Öffnungszeiten
maps-phone = Telefon
maps-website = Website
maps-close = Schließen
maps-done = Fertig
maps-place-unavailable = Der Kartenort ist nicht mehr verfügbar
maps-poi-unavailable = Der Ort in der Nähe ist nicht mehr in diesem Ausschnitt
maps-panel-empty = Suche einen Ort oder eine Adresse, um ihn hier zu sehen.
maps-panel-places = Orte
maps-results = Ergebnisse
maps-nearby = In der Nähe
maps-saved-places = Gespeicherte Orte
maps-recent-searches = Letzte Suchen
maps-clear = Leeren
maps-location-service-offline = Der Standortdienst ist in dieser Sitzung nicht verfügbar
maps-location-request-in-progress = Eine Standortanfrage läuft bereits
maps-hide-traffic = Verkehr ausblenden
maps-loading-places = Orte werden geladen…
maps-nearby-count = { $count } Orte in der Nähe
maps-location-disconnected = Der Standortdienst wurde getrennt, bevor er eine Position geliefert hat
maps-current-location = Aktueller Standort
maps-located = Standort gefunden
maps-map-centre = Kartenmitte
maps-leave-now = Jetzt losfahren
maps-routing = Route wird berechnet…
maps-finding-departures = Abfahrten werden gesucht…
maps-arrive-at = Ankunft um { $time }
maps-leave-arrive-at = Abfahrt um { $departure }, Ankunft um { $arrival }
maps-leave = Abfahrt
maps-arrive = Ankunft
maps-now = Jetzt
maps-departures-to = Abfahrten nach { $to }
maps-your-destination = dein Ziel
maps-choose-destination-departures = Wähle ein Ziel, um Abfahrten zu sehen.
maps-no-public-transport = Zu dieser Zeit fährt nichts dorthin.
maps-direct = Direkt
maps-changes = { $count } { $count ->
    [one] Umstieg
   *[other] Umstiege
 }
maps-walk-minutes = { $minutes } Min. zu Fuß
maps-walk-to = Zu Fuß nach { $place }
maps-line-towards = { $line } Richtung { $headsign }
maps-stops = { $count } { $count ->
    [one] Halt
   *[other] Halte
 }
maps-minutes = { $count } Min.
maps-live = Live
maps-scheduled = Fahrplan
maps-live-times = Live-Zeiten
maps-scheduled-times = Fahrplanzeiten
maps-times-from = { $freshness } von { $agency } über Transitous
maps-times-via = { $freshness } über Transitous
maps-attribution = Datenquellen
maps-licenses = Lizenzen
maps-offline-maps = Offline-Karten
maps-attribution-body = Kartendaten und Kartenkacheln © OpenStreetMap-Mitwirkende.
maps-licenses-body = Die Kartendaten sind © OpenStreetMap-Mitwirkende und stehen unter der Open Database License (ODbL). Die Kartenkacheln sind OpenStreetMap-Standardkacheln, genutzt nach der Kachel-Nutzungsrichtlinie der OpenStreetMap Foundation.
maps-imagery-unavailable = Kartenbilder können hier nicht geladen werden
maps-imagery-refused = Der Kachelserver hat die Anfrage abgelehnt
maps-imagery-unreachable = Der Kachelserver war nicht erreichbar. Es wird gleich erneut versucht.
maps-loading-map = Karte wird geladen…
maps-offline-ready = Offline-Bereich bereit ({ $count } Kacheln)
maps-offline-area-missing = Dieser Offline-Bereich ist nicht auf diesem Gerät
maps-offline-removed = { $name } entfernt, { $size } freigegeben
maps-offline-cleared = { $size } Offline-Kacheln geleert
maps-no-places-found = Keine Orte gefunden
maps-choose-start = Startpunkt wählen
maps-choose-destination = Ziel wählen
maps-swap = Start und Ziel tauschen
maps-category-landmark = Wahrzeichen
maps-category-cafe = Café
maps-category-restaurant = Restaurant
maps-category-bar = Bar
maps-category-museum = Museum
maps-category-park = Park
maps-category-transit = Haltestelle
maps-category-hotel = Hotel
maps-category-shop = Geschäft
maps-category-hospital = Krankenhaus
maps-category-other = Ort
maps-mode-drive = Auto
maps-mode-transit = Öffentlich
maps-mode-bike = Fahrrad
maps-mode-foot = Zu Fuß
maps-transit-walk = Zu Fuß
maps-transit-bus = Bus
maps-transit-tram = Tram
maps-transit-subway = U-Bahn
maps-transit-rail = Zug
maps-transit-ferry = Fähre
maps-panel-explore = Entdecken
maps-panel-directions = Route
maps-congestion-free = Fließend
maps-congestion-moderate = Mäßig
maps-congestion-heavy = Dicht
maps-congestion-standstill = Stillstand
maps-remaining = noch { $distance }
maps-turn-start = Start
maps-turn-head-out = Auf { $road } losfahren
maps-turn-arrive = Am Ziel ankommen
maps-turn-continue = Weiterfahren
maps-turn-continue-on = Auf { $road } weiterfahren
maps-turn-left = Links abbiegen
maps-turn-left-onto = Links auf { $road } abbiegen
maps-turn-sharp-left = Scharf links abbiegen
maps-turn-sharp-left-onto = Scharf links auf { $road } abbiegen
maps-turn-slight-left = Leicht links abbiegen
maps-turn-slight-left-onto = Leicht links auf { $road } abbiegen
maps-turn-right = Rechts abbiegen
maps-turn-right-onto = Rechts auf { $road } abbiegen
maps-turn-sharp-right = Scharf rechts abbiegen
maps-turn-sharp-right-onto = Scharf rechts auf { $road } abbiegen
maps-turn-slight-right = Leicht rechts abbiegen
maps-turn-slight-right-onto = Leicht rechts auf { $road } abbiegen
maps-turn-uturn = Wenden
maps-turn-roundabout = Im Kreisverkehr die Ausfahrt nehmen
maps-turn-roundabout-onto = Im Kreisverkehr die Ausfahrt auf { $road } nehmen
maps-what-search = Die Suche
maps-what-route = Die Routenplanung
maps-what-transit = Die Abfahrtssuche
maps-what-nearby = Das Laden naher Orte
maps-route-estimate = { $reason } Die Offline-Schätzung wird angezeigt.

maps-addresses-places = Adressen und Orte
