## Verlauf

print-centre-print-defaults-two-sided = { $new ->
    [true] Ab jetzt beidseitig drucken
   *[false] Ab jetzt einseitig drucken
}
print-centre-print-defaults-scan-resolution = Ab jetzt mit { $new } dpi scannen
print-centre-print-defaults-scan-color = { $new ->
    [grayscale] Ab jetzt in Graustufen scannen
    [line-art] Ab jetzt in Strichzeichnung scannen
   *[color] Ab jetzt in Farbe scannen
}
print-centre-print-defaults-scan-source = { $new ->
    [feeder] Ab jetzt vom Einzug scannen
    [feeder-duplex] Ab jetzt beidseitig vom Einzug scannen
   *[flatbed] Ab jetzt vom Vorlagenglas scannen
}
print-centre-print-defaults-ocr-language = { $new ->
    [deu] Ab jetzt deutschen Text erkennen
    [fra] Ab jetzt französischen Text erkennen
    [spa] Ab jetzt spanischen Text erkennen
   *[eng] Ab jetzt englischen Text erkennen
}

## Was das Fenster während der Arbeit sagt

print-centre-loading = Druckzentrale wird geladen
print-centre-finding-printers = Drucker in der Nähe werden gesucht
print-centre-choose-printer = Drucker wählen
print-centre-preparing-preview = Dokumentvorschau wird vorbereitet
print-centre-all-pages = Alle Seiten

## Fenster- und Barrierefreiheits-Texte
print-centre-a11y-pages = Zu druckende Seiten

## Fakten, Eigenschaftsnamen und Bereichstitel
print-centre-text-language = Textsprache
print-centre-orientation = Ausrichtung
print-centre-page-of = Seite { $page } von { $pages }
print-centre-availability = Verfügbarkeit
print-centre-state = Zustand
print-centre-section-connection-check = Verbindungsprüfung
print-centre-scanners = Verfügbare Scanner
print-centre-reporting-levels = Mit Füllstandsmeldung
print-centre-printers-responding = Antwortende Drucker
print-centre-section-tools = Werkzeuge
print-centre-section-printers = Deine Drucker

## Was das Fenster zeigt
print-centre-try-again = Erneut versuchen
print-centre-loading-detail = Drucker, Scanner und unfertige Dokumente werden gesucht.
print-centre-unavailable-printers = Drucker konnten nicht geprüft werden
print-centre-unavailable-printers-detail = Die Druckzentrale konnte die Drucker gerade nicht abrufen. Deine gespeicherten Geräte und Dokumente bleiben erhalten.
print-centre-unavailable-documents = Druckaufträge konnten nicht geprüft werden
print-centre-unavailable-documents-detail = Die Druckzentrale konnte die Druckaufträge gerade nicht abrufen. Deine gespeicherten Geräte und Dokumente bleiben erhalten.
print-centre-unavailable-scanners = Scanner konnten nicht geprüft werden
print-centre-unavailable-scanners-detail = Die Druckzentrale konnte die Scanner gerade nicht abrufen. Deine gespeicherten Geräte und Dokumente bleiben erhalten.
print-centre-unavailable-some = Einige Geräteangaben konnten nicht geprüft werden
print-centre-unavailable-some-detail = Die Druckzentrale konnte gerade nicht alle Geräteangaben abrufen. Deine gespeicherten Geräte und Dokumente bleiben erhalten.
print-centre-job-waiting = Wartet auf Druck
print-centre-job-attention = Braucht Aufmerksamkeit
print-centre-job-printing = Wird gedruckt
print-centre-sent-to = Gesendet an { $printer }
print-centre-page-progress = Seite { $page } von { $total }
print-centre-pages-progress = { $done } von { $total } Seiten
print-centre-open-queue = Warteschlange öffnen
print-centre-cancel = Abbrechen
print-centre-confirm-cancel = Dieses Dokument abbrechen
print-centre-lowest-supply = Geringster Füllstand
print-centre-supplies-unavailable = Füllstände nicht verfügbar
print-centre-preferred = Bevorzugter Drucker
print-centre-available = Für deine Apps verfügbar
print-centre-no-printers = Noch kein Drucker hinzugefügt. Suche einen in deinem Netzwerk, um aus jeder App zu drucken.
print-centre-find-printer = Drucker finden
print-centre-scan-document = Dokument scannen
print-centre-scanner-ready = { $scanner } ist bereit
print-centre-history-review = Nachsehen, was gedruckt wurde, und erneut drucken
print-centre-history-empty = Noch nichts gedruckt
print-centre-print-history = Druckverlauf
print-centre-device-help = Gerätehilfe
print-centre-device-help-detail = Verbindungen, Papier, Füllstände und Scanner prüfen
print-centre-status-printed = Gedruckt
print-centre-status-canceled = Abgebrochen
print-centre-status-not-printed = Nicht gedruckt
print-centre-status-failed = Konnte nicht drucken
print-centre-status-waiting = Wartet
print-centre-status-in-progress = Läuft
print-centre-printers-responding-count = { $responding } von { $total }
print-centre-reporting-levels-count = { $count ->
    [one] { $count } Drucker
   *[other] { $count } Drucker
}
print-centre-print-again = Erneut drucken
print-centre-nothing-completed = Noch kein fertiges Dokument.
print-centre-documents = Dokumente
print-centre-open-controls = Druckersteuerung öffnen
print-centre-no-scanner = Kein Scanner gefunden. Prüfe Kabel oder Netzwerkverbindung und suche dann erneut.
print-centre-scanner-ready-detail = Dein Scanner ist für ein neues Dokument bereit.
print-centre-start-scanning = Scannen starten
print-centre-scanner = Scanner
print-centre-nothing-waiting = Nichts in der Warteschlange.
print-centre-pause-printer = Drucker pausieren
print-centre-resume-printer = Drucker fortsetzen
print-centre-make-preferred = Bevorzugen
print-centre-view-queue = Druckwarteschlange ansehen
print-centre-check-printer = Drucker prüfen
print-centre-no-supply-report = Dieser Drucker meldet keine Füllstände.
print-centre-details = Details
print-centre-ink-toner = Tinte und Toner
print-centre-printer-controls = Druckersteuerung
print-centre-searching = Drucker in deinem Netzwerk werden gesucht.
print-centre-no-printers-found = Kein Drucker gefunden. Prüfe, ob der Drucker an ist und im selben Netzwerk hängt, und suche dann erneut.
print-centre-search-again = Erneut suchen
print-centre-nearby = In der Nähe
print-centre-add = Hinzufügen
print-centre-printers-network = Drucker in deinem Netzwerk
print-centre-search-failed = Die Suche war gerade nicht möglich. Prüfe deine Netzwerkverbindung und versuche es erneut.
print-centre-preview-unavailable = Vorschau nicht verfügbar
print-centre-original-unchanged = Das Originaldokument bleibt unverändert.
print-centre-previous = Zurück
print-centre-next = Weiter
print-centre-print = Drucken
print-centre-portrait = Hochformat
print-centre-landscape = Querformat
print-centre-page-of-document = Seite { $page } von { $pages }
print-centre-recognized-text = Erkannter Text
print-centre-rotate-right = Nach rechts drehen
print-centre-apply-crop = Zuschnitt anwenden
print-centre-updating-text = Erkannter Text wird aktualisiert…
print-centre-update-text = Erkannten Text aktualisieren
print-centre-copy-text = Erkannten Text kopieren
print-centre-page-tools = Seitenwerkzeuge
print-centre-previous-page = Vorherige Seite
print-centre-next-page = Nächste Seite
print-centre-move-earlier = Vor die vorherige Seite verschieben
print-centre-move-later = Hinter die nächste Seite verschieben
print-centre-remove-page = Diese Seite entfernen
print-centre-crop = Zuschnitt
print-centre-pages-in-document = Seiten in diesem Dokument
print-centre-pages-in-document-count = { $count ->
    [one] { $count } Seite
   *[other] { $count } Seiten
}
print-centre-new-document = Neues gescanntes Dokument
print-centre-export-page = Diese Seite exportieren
print-centre-scan-another = Weitere Seite scannen
print-centre-save-document = Dokument speichern
print-centre-no-scanner-available = Kein Scanner verfügbar. Prüfe Kabel oder Netzwerkverbindung und lade die Druckzentrale dann neu.
print-centre-quality = Qualität
print-centre-appearance = Aussehen
print-centre-paper-source = Papierzufuhr
print-centre-page-settings = Seiteneinstellungen
print-centre-pages-captured = { $count ->
    [one] { $count } Seite aufgenommen. Neue Scans kommen zu diesem Dokument.
   *[other] { $count } Seiten aufgenommen. Neue Scans kommen zu diesem Dokument.
}
print-centre-scanning-page = Seite wird gescannt…
print-centre-scan-page = Seite scannen

print-centre-job-on = { $status } an { $printer }
print-centre-queue-ready = Bereit
print-centre-queue-not-accepting = Nimmt keine neuen Dokumente an
print-centre-queue-printing = Druckt
print-centre-queue-paused = Angehalten
print-centre-queue-unreachable = Dieser Drucker ist nicht erreichbar
print-centre-reason-out-of-paper = Legen Sie Papier ein, um fortzufahren.
print-centre-reason-jam = Beheben Sie den Papierstau und setzen Sie den Drucker dann fort.
print-centre-reason-cover-open = Schließen Sie die Abdeckung des Druckers.
print-centre-reason-supply-low = Ein Betriebsmittel geht zur Neige; der Druck kann meist vorerst weiterlaufen.
print-centre-reason-supply-empty = Ersetzen Sie das leere Betriebsmittel, um fortzufahren.
print-centre-reason-offline = Prüfen Sie, ob der Drucker eingeschaltet und verbunden ist.
print-centre-reason-unknown = Der Drucker meldet einen Zustand, der Aufmerksamkeit brauchen kann.
print-centre-device-answered = Der Drucker hat die letzte Statusabfrage beantwortet.
print-centre-device-no-supplies = Dieser Drucker meldet keine Füllstände.
print-centre-device-unreachable = Live-Details sind nicht verfügbar. Der Drucker ist vielleicht im Ruhezustand oder getrennt.
print-job-document = Dokument { $number }
print-centre-cancel-pending-message = Das Dokument wird vom Drucker entfernt. Das lässt sich nicht rückgängig machen.

## A document waiting to be cancelled
print-centre-cancel-pending-title = Dieses Dokument abbrechen?
