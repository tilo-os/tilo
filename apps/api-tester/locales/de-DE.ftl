api-variables-resolved = { $count ->
    [one] 1 Variable aufgelöst
   *[other] { $count } Variablen aufgelöst
}
api-variables-unresolved = { $count ->
    [one] Nicht aufgelöste Variable: { $names }
   *[other] Nicht aufgelöste Variablen: { $names }
}

## Verlauf: was jede Änderung getan hat, nach Feld und Verb

api-tester-workspace-collections = Die Sammlungen geändert
api-tester-workspace-environments = Die Umgebungen geändert
api-tester-workspace-active-environment = Die aktive Umgebung geändert
api-tester-workspace-request = Die Anfrage im Editor ersetzt
api-tester-request-record-name = Eine Anfrage umbenannt
api-tester-request-record-auth = Die Anmeldung einer Anfrage geändert
api-tester-request-record-method = Die Methode auf { $new ->
    [get] GET
    [post] POST
    [put] PUT
    [patch] PATCH
    [delete] DELETE
    [head] HEAD
    [options] OPTIONS
   *[other] { $new }
} gesetzt
api-tester-request-record-url = Die URL einer Anfrage bearbeitet
api-tester-request-record-params = Die Abfrageparameter einer Anfrage geändert
api-tester-request-record-headers = Die Kopfzeilen einer Anfrage geändert
api-tester-request-record-body-mode = Den Inhalt geändert, jetzt { $new ->
    [none] ohne
    [json] JSON
    [form] ein Formular
    [raw] Rohtext
   *[other] { $new }
}
api-tester-request-record-body = Den Inhalt einer Anfrage bearbeitet
api-tester-request-record-form = Die Formularfelder einer Anfrage geändert
api-tester-credentials-scheme = Die Anmeldung geändert: { $new ->
    [none] keine
    [bearer] ein Bearer-Token
    [basic] Basic
    [api_key] ein API-Schlüssel
   *[other] { $new }
}
api-tester-credentials-username = Den Benutzernamen einer Anfrage geändert
api-tester-credentials-header = Die Schlüssel-Kopfzeile einer Anfrage geändert
api-tester-row-key = Den Schlüssel einer Zeile geändert
api-tester-row-value = Den Wert einer Zeile geändert
api-tester-row-enabled = { $new ->
    [true] Eine Zeile eingeschlossen
   *[false] Eine Zeile ausgeschlossen
}
api-tester-saved-collection-name = Die Sammlung { $item } umbenannt
api-tester-saved-collection-requests = Die Anfragen von { $item } geändert
api-tester-environment-name = Die Umgebung { $item } umbenannt
api-tester-environment-variables = Die Variablen von { $item } geändert
api-tester-variable-key = Eine Variable von { $item } umbenannt
api-tester-variable-value = Eine Variable von { $item } geändert
api-tester-variable-enabled = { $new ->
    [true] Eine Variable von { $item } eingeschlossen
   *[false] Eine Variable von { $item } ausgeschlossen
}
api-tester-new-request = Eine neue Anfrage gestartet
api-tester-new-collection = Die Sammlung { $name } angelegt
api-tester-delete-collection = Die Sammlung { $item } gelöscht
api-tester-add-request = { $name } zu { $item } hinzugefügt
api-tester-save-request = Eine Anfrage in { $item } gespeichert
api-tester-open-request = Eine Anfrage aus { $item } geöffnet
api-tester-delete-request = Eine Anfrage aus { $item } gelöscht
api-tester-new-environment = Die Umgebung { $name } angelegt
api-tester-delete-environment = Die Umgebung { $item } gelöscht
api-tester-use-environment = Zur Umgebung { $item } gewechselt
api-tester-add-variable = Die Variable { $key } zu { $item } hinzugefügt
api-tester-open-sent = { $item } erneut geöffnet
api-tester-record-sent = Eine Anfrage gesendet, die Status { $status } zurückgab

## Barrierefreiheit
api-tester-row-actions = Zeilenaktionen
api-tester-url-placeholder = URL eingeben

## Fenster- und Barrierefreiheits-Texte
api-tester-environment = Umgebung
api-tester-a11y-use-row = Diese Zeile verwenden
api-tester-request-body = Anfragetext
api-tester-request-url = Anfrage-URL
api-tester-method = Methode
api-tester-response-headers = Antwort-Header
api-tester-requests = Anfragen
api-tester-add-row = { $field } hinzufügen

## What the window shows
api-tester-name = Name
api-tester-variable-key-field = Variable
api-tester-variable-value-field = Wert der Variable
api-tester-key = Schlüssel
api-tester-value = Wert
api-tester-add-key = Schlüssel hinzufügen
api-tester-add-value = Wert hinzufügen
api-tester-remove-row = Zeile entfernen
api-tester-save-request-button = Anfrage speichern
api-tester-cancel-request = Anfrage abbrechen
api-tester-send = Senden
api-tester-query-parameters = Abfrageparameter
api-tester-headers = Kopfzeilen
api-tester-token = Token
api-tester-username = Benutzername
api-tester-password = Passwort
api-tester-header-name = Name der Kopfzeile
api-tester-copy-body = Inhalt kopieren
api-tester-copy-headers = Kopfzeilen kopieren
api-tester-copy-request = Anfrage kopieren
api-tester-new-collection-button = Neue Sammlung
api-tester-reset-area = Anfragebereich zurücksetzen
api-tester-size-more-than = Mehr als { $size }

## What the request builder's selectors are called
api-tester-auth-none = Ohne Authentifizierung
api-tester-auth-bearer = Bearer-Token
api-tester-auth-basic = Basic-Authentifizierung
api-tester-auth-api-key = API-Schlüssel
api-tester-auth-stored = Gespeicherte Authentifizierung
api-tester-body-none = Keiner
api-tester-body-json = JSON
api-tester-body-form = Formular
api-tester-body-raw = Rohtext
api-tester-tab-params = Parameter
api-tester-tab-headers = Kopfzeilen
api-tester-tab-auth = Anmeldung
api-tester-tab-body = Inhalt
api-tester-view-pretty = Formatiert
api-tester-view-raw = Rohtext
api-tester-view-headers = Kopfzeilen
api-tester-split-label = Anfrage und Antwort in der Größe ändern
