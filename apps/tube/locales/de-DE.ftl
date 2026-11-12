# Tube — deutsche Sprache (de-DE).
# Anzahlen kommen doppelt: `count` (eine Zahl, die die Pluralform wählt) und
# `shown` (die Zahl, wie Tube sie abkürzt, etwa 1,2 Tsd.).

## Ziele
tube-nav-home = Start
tube-nav-trending = Im Trend
tube-nav-subscriptions = Abos
tube-nav-queue = Warteschlange
tube-nav-history = Verlauf
tube-nav-settings = Einstellungen
tube-nav-name = Tube

## Kopfzeile und Navigation
tube-clear-history = Verlauf löschen

## Anzahlen und Fakten
tube-views = { $count ->
    [one] { $shown } Aufruf
   *[other] { $shown } Aufrufe
}
tube-views-and-age = { $views }, { $age }
tube-watching = { $shown } sehen zu
tube-subscribers = { $count ->
    [one] { $shown } Abonnent
   *[other] { $shown } Abonnenten
}
tube-video-count = { $count ->
    [one] { $shown } Video
   *[other] { $shown } Videos
}
tube-dislikes = { $count ->
    [one] { $shown } Dislike
   *[other] { $shown } Dislikes
}
tube-likes = { $count ->
    [one] { $shown } Like
   *[other] { $shown } Likes
}
tube-replies = { $count ->
    [one] { $shown } Antwort
   *[other] { $shown } Antworten
}

## Start, Trend, Kanäle, Verlauf, Warteschlange
tube-home-error = Die Startseite konnte nicht neu laden
tube-offline-title = Du bist offline
tube-offline-message = Verbinde dich neu, um Videos zu laden. Deine Warteschlange und dein Verlauf bleiben hier.
tube-unreached-title = Hier lassen sich keine Videos laden
tube-sources-no-relay = Die Videoquellen lassen Webseiten nicht lesen, und diese Seite hat keinen Abruf-Proxy, der sie weiterleitet. Deine Warteschlange und dein Verlauf bleiben hier.
tube-sources-failed = Keine Videoquelle hat geantwortet. Deine Warteschlange und dein Verlauf bleiben hier.
tube-home-empty-title = Noch nichts zu zeigen
tube-home-empty-message = Suche ein Video, um mit dem Ansehen zu beginnen.
tube-search = Suchen
tube-search-for = YouTube nach „{ $query }“ durchsuchen
tube-trending-empty-title = Noch nichts im Trend
tube-trending-empty-message = Beliebte Videos erscheinen hier, sobald der Katalog geladen ist.
tube-subscriptions-empty-title = Noch keine Abos
tube-subscriptions-empty-message = Abonniere Creator, dann erscheinen ihre neuen Videos hier.
tube-latest-videos = Neueste Videos
tube-history-not-saving = Der Ansehverlauf wird nicht gespeichert
tube-history-empty-title = Noch nichts angesehen
tube-history-empty-message = Angesehene Videos erscheinen hier.
tube-queue-empty-title = Deine Warteschlange ist leer
tube-queue-empty-message = Füge Videos von ihrer Ansehseite hinzu.
tube-queue-waiting = { $count ->
    [one] Ein weiteres eingereihtes Video erscheint, sobald seine Angaben geladen sind.
   *[other] { $count } weitere eingereihte Videos erscheinen, sobald ihre Angaben geladen sind.
}

## Wiedergabelisten und Creator
tube-playlist-missing = Diese Wiedergabeliste ist nicht verfügbar
tube-play-all = Alle abspielen
tube-videos = Videos
tube-creator-missing = Dieser Creator ist nicht verfügbar
tube-subscribe = Abonnieren
tube-subscribed = Abonniert

## Ansehseite
tube-video-missing = Dieses Video ist nicht verfügbar
tube-resolution = Auflösung
tube-quality-now = Qualität, { $quality } läuft
tube-playback-speed = Wiedergabegeschwindigkeit
tube-speed-value = { $rate }×
tube-captions = Untertitel
tube-captions-named = Untertitel, { $why }
tube-captions-off = Aus
tube-captions-automatic = { $track } (automatisch erzeugt)
tube-captions-loading = werden geladen…
tube-captions-not-offered = von diesem Video nicht angeboten
tube-captions-unavailable = konnten nicht geladen werden
tube-resume-at = Weiter bei { $time }
tube-skip-segment = { $segment } überspringen
tube-like = Liken
tube-like-count = Liken, { $likes }
tube-add-to-queue = Zur Warteschlange hinzufügen
tube-remove-from-queue = Aus der Warteschlange entfernen
tube-copy-link = Link kopieren
tube-playback-settings = Wiedergabeeinstellungen
tube-comments = Kommentare
tube-comments-off = Kommentare sind in den Einstellungen ausgeschaltet.
tube-pinned = Angeheftet
tube-hearted = Vom Creator gelikt
tube-up-next = Als Nächstes
tube-autoplay = Automatisch abspielen

## Einstellungen
tube-playback = Wiedergabe
tube-autoplay-next = Nächstes Video automatisch abspielen
tube-show-comments = Kommentare zeigen
tube-show-dislikes = Dislike-Anzahlen zeigen
tube-dislikes-note = Dislike-Anzahlen sind Schätzungen von Return YouTube Dislike, beruhend auf den Stimmen seiner Nutzer.
tube-quality = Qualität
tube-preferred-quality = Bevorzugte Qualität
tube-quality-automatic = Automatisch
tube-sponsorblock = SponsorBlock
tube-skip-sponsored = Gesponserte Abschnitte überspringen
tube-sponsorblock-note = Abschnitte werden von der SponsorBlock-Community markiert und in ihrer Farbe auf der Zeitleiste gezeigt.
tube-segments = Abschnitte
tube-dearrow = DeArrow
tube-dearrow-community = Community-Titel und -Vorschaubilder verwenden
tube-dearrow-titles = Titel ersetzen
tube-dearrow-thumbnails = Vorschaubilder ersetzen

## Abschnittsarten und was Tube damit tut
tube-segment-sponsor = Sponsor
tube-segment-sponsor-about = Bezahlte Werbung, bezahlte Verweise und direkte Anzeigen.
tube-segment-selfpromo = Eigenwerbung
tube-segment-interaction = Erinnerung an Interaktion
tube-segment-interaction-about = Erinnerungen zum Liken, Abonnieren oder Folgen, ohne weiteren Inhalt.
tube-segment-intro = Intro und Pause
tube-segment-intro-about = Eine Strecke ohne Inhalt: eine Pause, eine Animation oder ein Intro.
tube-segment-outro = Endkarten und Abspann
tube-segment-outro-about = Abspann und Endkarten, nachdem das Video vorbei ist.
tube-segment-preview = Rückblick und Vorschau
tube-segment-preview-about = Ein Rückblick auf frühere Folgen oder eine Vorschau auf das, was kommt.
tube-segment-music-offtopic = Abschnitt ohne Musik
tube-segment-music-offtopic-about = Die Teile eines Musikvideos, die keine Musik sind.
tube-segment-filler = Füllstrecke
tube-segment-filler-about = Abschweifungen, die nur eingefügt wurden, um das Video länger zu machen.

## Gerätesynchronisierung
tube-device-sync = Gerätesynchronisierung
tube-sync-unavailable-now = Die Gerätesynchronisierung ist gerade nicht verfügbar.
tube-sync-not-built = Diese Ausgabe von Tube enthält die Gerätesynchronisierung von Grayjay nicht.
tube-sync-locked-title = Die Gerätesynchronisierung ist nicht verfügbar
tube-sync-locked-message = Entsperre den Geheimnisspeicher des Desktops und öffne Tube dann erneut, um Grayjay-Geräte zu paaren.
tube-sync-now = Jetzt synchronisieren
tube-pair-title = Ein Gerät paaren
tube-pair-how = Öffne in Grayjay die Synchronisierung und wähle Gerät hinzufügen, dann füge seine Einladung hier ein.
tube-pair-placeholder = Paarungseinladung von Grayjay
tube-pair-name = Paarungseinladung von Grayjay
tube-pair-device = Gerät paaren
tube-pending-title = Wartet auf Bestätigung
tube-pending-check = Bestätige, wenn Grayjay den Fingerabdruck { $fingerprint } zeigt
tube-approve-device = Gerät bestätigen
tube-approved-devices = Bestätigte Geräte
tube-no-approved-devices = Noch keine bestätigten Geräte. Paaren braucht eine Bestätigung auf beiden Geräten.
tube-not-synced = Noch nicht synchronisiert
tube-sync-failed = Synchronisieren fehlgeschlagen: { $error }
tube-last-synced = Zuletzt synchronisiert { $when }
tube-revoke-device = Gerät entziehen
tube-sync-ready = Bereit
tube-sync-pairing = Paaren
tube-sync-syncing = Synchronisieren
tube-sync-attention = Braucht Aufmerksamkeit

## Hinweise
tube-notice-link-copied = Link kopiert: { $url }
tube-notice-video-gone = Dieses Video ist nicht mehr verfügbar
tube-notice-creator-gone = Dieser Creator ist nicht mehr verfügbar
tube-notice-playlist-gone = Diese Wiedergabeliste ist nicht mehr verfügbar
tube-unplayable = Dieses Video lässt sich hier gerade nicht abspielen
tube-unplayable-youtube-said = YouTube hat die Wiedergabe verweigert: „{ $reason }“
tube-unplayable-no-source = Keine der Quellen von Tube hatte einen Datenstrom dafür.
tube-try-again = Erneut versuchen
tube-live-in-app = Livestreams laufen vorerst in der Tilo-App
tube-notice-playback-limit = Das Wiedergabelimit dieses Hosts ist erreicht. Versuche es später erneut.
tube-notice-no-downloads = Tube kann Videos nicht für das Ansehen ohne Verbindung speichern
tube-notice-pairing-busy = Das Paaren von Geräten ist belegt oder nicht verfügbar
tube-notice-sync-busy = Die Gerätesynchronisierung ist belegt oder nicht verfügbar
tube-notice-skipped-to = Zu { $time } gesprungen
tube-notice-skipped-segment = { $segment } übersprungen ({ $start } bis { $end })
tube-notice-history-cleared = Der Ansehverlauf dieses Profils wurde gelöscht
tube-notice-pair-first = Füge zuerst eine Paarungseinladung ein, oder entsperre den Geheimnisspeicher des Desktops
tube-notice-sync-unavailable = Die Gerätesynchronisierung ist nicht verfügbar

## Verlauf: was jede Änderung an der geteilten Bibliothek getan hat

tube-library-likes = { $op ->
    [remove] Ein Like zurückgenommen
   *[other] Ein Video gelikt
}
tube-library-queue = { $op ->
    [remove] Ein Video aus der geteilten Warteschlange entfernt
   *[other] Ein Video zur geteilten Warteschlange hinzugefügt
}
tube-library-subscriptions = { $op ->
    [remove] Ein Abo eines Creators beendet
   *[other] Einen Creator abonniert
}
tube-library-comment-likes = { $op ->
    [remove] Das Like eines Kommentars zurückgenommen
   *[other] Einen Kommentar gelikt
}
tube-library-replies = Die Antworten geändert
tube-reply-to-comment = Auf { $text } geantwortet
tube-retract = Die Antwort { $item } zurückgezogen

## Verlauf: der eigene Ansehverlauf der Person

tube-record = Ein Video angesehen
tube-import = Auf einem anderen Gerät angesehene Videos übernommen
tube-forget = Ein Video aus dem Ansehverlauf entfernt
tube-clear = Den Ansehverlauf gelöscht

## Fakten, Eigenschaftsnamen und Bereichstitel
tube-search-heading = Suche: { $query }

tube-queue-shared = Gemeinsame Warteschlange
tube-video = Video
tube-creator = Kanal
tube-playlist = Playlist

## What the segment categories and the search types are called
tube-category-sponsor = Werbung
tube-category-self-promo = Eigenwerbung
tube-category-interaction = Erinnerung zum Mitmachen
tube-category-intro = Intro
tube-category-outro = Outro
tube-category-preview = Vorschau
tube-category-music-offtopic = Abschnitt ohne Musik
tube-category-filler = Abschweifung
tube-category-sponsor-blurb = Bezahlte Werbung, bezahlte Empfehlungen und direkte Anzeigen.
tube-category-self-promo-blurb = Eigenwerbung ohne Bezahlung, Merch und Spenden.
tube-category-interaction-blurb = Bitten, zu liken und zu abonnieren, ohne weiteren Inhalt.
tube-category-intro-blurb = Ein Abschnitt ohne eigentlichen Inhalt: eine Pause, eine Animation oder ein Intro.
tube-category-outro-blurb = Abspann oder das Ende des Videos: Endkarten und Outros.
tube-category-preview-blurb = Eine kurze Zusammenfassung früherer Folgen oder eine Vorschau auf das Kommende.
tube-category-music-offtopic-blurb = Die Abschnitte eines Musikvideos ohne Musik, zum Musikhören.
tube-category-filler-blurb = Abschweifende Szenen, die das Video nur länger machen.
tube-skip-auto = Automatisch überspringen
tube-skip-show = Anzeigen
tube-skip-off = Aus
tube-type-video = Video
tube-type-playlist = Wiedergabeliste
tube-type-creator = Kanal

## Actions

tube-effect-show-screen = Ansicht anzeigen
tube-effect-toggle-sidebar = Navigation ein- oder ausblenden
tube-effect-filter-search = Suchergebnisse filtern
tube-effect-open-video = Video öffnen
tube-effect-open-creator = Creator öffnen
tube-effect-open-playlist = Playlist öffnen
tube-effect-search = Videos suchen
tube-effect-toggle-playback = Wiedergeben oder pausieren
tube-effect-seek = Springen
tube-effect-skip-segment = Abschnitt überspringen
tube-effect-share = Freigabelink kopieren
tube-effect-download = Video herunterladen
tube-effect-mark-watched = Als gesehen markieren
tube-effect-forget-watched = Aus dem Verlauf entfernen
tube-effect-clear-history = Verlauf löschen
tube-effect-pair-device = Mit Einladung koppeln
tube-effect-sync-devices = Geräte abgleichen
tube-effect-approve-device = Gerät zulassen
tube-effect-revoke-device = Gerätezugriff entziehen
tube-effect-set-preference = Wiedergabeeinstellung ändern
tube-effect-set-preferred-quality = Bevorzugte Qualität festlegen
tube-effect-set-segment-handling = Umgang mit Abschnitten festlegen
