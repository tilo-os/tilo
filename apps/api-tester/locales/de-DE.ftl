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
