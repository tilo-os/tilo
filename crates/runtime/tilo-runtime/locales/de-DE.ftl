# Die eigenen Texte der Laufzeit: die Einwilligung für Kamera und Mikrofon
# und die Gerätefreigaben, die sie als Aktionen anbietet. $device ist
# `camera` oder `microphone`.
device-consent-question = { $device ->
    [camera] Darf { $app } die Kamera verwenden?
   *[microphone] Darf { $app } das Mikrofon verwenden?
}
device-consent-detail = { $device ->
    [camera] Ohne deine Erlaubnis erhält { $app } nichts von der Kamera. Deine Entscheidung gilt in diesem Arbeitsbereich, bis du sie widerrufst.
   *[microphone] Ohne deine Erlaubnis erhält { $app } nichts vom Mikrofon. Deine Entscheidung gilt in diesem Arbeitsbereich, bis du sie widerrufst.
}
device-consent-allow = Erlauben
device-consent-deny = Nicht erlauben
device-grants = Zugriff auf Kamera und Mikrofon
device-grant-allowed = { $device ->
    [camera] { $app } darf die Kamera verwenden
   *[microphone] { $app } darf das Mikrofon verwenden
}
device-grant-denied = { $device ->
    [camera] { $app } darf die Kamera nicht verwenden
   *[microphone] { $app } darf das Mikrofon nicht verwenden
}
device-grant-workspace = im Arbeitsbereich { $workspace }
device-grant-revoke = Widerrufen
device-grant-revoke-description = Diese Entscheidung vergessen: eine laufende Aufnahme endet, und die App fragt beim nächsten Mal erneut.
agent-confirm-question = Darf dein Agent „{ $action }“ in { $app } verwenden?
agent-confirm-detail = Das erlaubt nur diese eine Aktion; dein Agent fragt beim nächsten Mal erneut.
agent-confirm-allow = Erlauben
agent-confirm-deny = Nicht erlauben
