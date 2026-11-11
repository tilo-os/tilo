## Eine abgegebene Rückmeldung, Zeile für Zeile vorgelesen
# Ein gewählter Teil des Bildschirms: sein Ort, was er war, und die App.
feedback-report-pick = { $index }. { $what } in { $app }
# Ein gewählter Teil aus einer App ohne bekannten Namen: sein Ort und was er
# war, nie die Kennung der App.
feedback-report-pick-alone = { $index }. { $what }
# Was ein gewählter Teil war, wenn die App gesagt hat, was für ein Ding er ist.
feedback-report-part-kind = { $kind }
# Was ein gewählter Teil war, nach dem eigenen Namen der App und seiner Rolle.
feedback-report-part-named = { $role ->
    [Button] Die Schaltfläche „{ $name }“
    [ToggleButton] Die Schaltfläche „{ $name }“
    [Link] Der Link „{ $name }“
    [Tab] Der Reiter „{ $name }“
    [MenuItem] Der Menüeintrag „{ $name }“
    [Checkbox] Das Kästchen „{ $name }“
    [Switch] Der Schalter „{ $name }“
    [TextInput] Das Feld „{ $name }“
    [MultilineTextInput] Das Feld „{ $name }“
    [Heading] Die Überschrift „{ $name }“
   *[other] „{ $name }“ als { $role }
}
# Was ein gewählter Teil war, nur an seiner Rolle erkannt.
feedback-report-part-role = { $role ->
    [ListItem] Ein Listeneintrag
    [Row] Eine Zeile
    [Cell] Eine Zelle
    [Button] Eine Schaltfläche
    [ToggleButton] Eine Schaltfläche
    [Link] Ein Link
    [Image] Ein Bild
    [TextInput] Ein Feld
    [MultilineTextInput] Ein Feld
    [StaticText] Ein Text
    [Paragraph] Ein Absatz
    [Heading] Eine Überschrift
    [List] Eine Liste
    [Group] Eine Gruppe
    [Window] Ein Fenster
    [Toolbar] Eine Werkzeugleiste
    [Menu] Ein Menü
    [MenuItem] Ein Menüeintrag
    [Tab] Ein Reiter
   *[other] Ein Teil
}
feedback-report-note = „{ $note }“
# Ein Text, den ein gewählter Teil zeigte, wie er geprüft wurde.
feedback-report-text = { $text }
# Eine der letzten Aktionen der meldenden Person, nach dem Namen, den ihre App
# ihr gibt; $outcome ist done, refused oder failed.
feedback-report-action = { $outcome ->
    [refused] { $label } (abgelehnt)
    [failed] { $label } (fehlgeschlagen)
   *[done] { $label }
}
# Eine Aktion, deren App ihr keinen Namen gab, nach ihrer Kennung und Art des Objekts.
feedback-report-action-unnamed = { $action } an { $resource }
feedback-report-recent = Kurz davor: { $actions }
# Die Aufzeichnung, die eine Meldung trägt: die App, deren Verlauf aufgezeichnet
# wurde, Anfang und Ende und wie viele Änderungen sie durchläuft.
feedback-report-recording = Aufzeichnung von { $app } von { $from } bis { $to }, { $steps ->
    [one] eine Änderung
   *[other] { $steps } Änderungen
}
feedback-report-slow = Langsam: { $slow } der letzten { $frames } Bilder brauchten über { $budget } ms, das langsamste { $worst } ms
# Ein ausgeblendeter Abschnitt eines gemeldeten Textes, nach dem, was er verbarg.
feedback-report-hidden = { $kind ->
    [name] ‹Name›
    [email] ‹E-Mail›
    [phone] ‹Telefon›
    [address] ‹Adresse›
    [account] ‹Kontonummer›
    [credential] ‹Zugangsdaten›
   *[personal] ‹Persönlich›
}
tilo-app-undo-description = Nimm deine letzte Änderung an deiner { $label } zurück.
tilo-app-redo-description = Stelle die Änderung an deiner { $label } wieder her, die du zuletzt zurückgenommen hast.
