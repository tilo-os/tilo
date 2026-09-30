# Die eigenen Texte der Laufzeit: die Einwilligung für Kamera und Mikrofon
# und die Gerätefreigaben, die sie als Aktionen anbietet. $device ist
# `camera` oder `microphone`.
device-consent-question = { $device ->
    [camera] Darf { $app } die Kamera verwenden?
   *[microphone] Darf { $app } das Mikrofon verwenden?
}
device-consent-detail = { $device ->
    [camera] Tilo merkt sich diese Entscheidung für { $app } in diesem Arbeitsbereich, bis du sie widerrufst. Ohne deine Erlaubnis erhält die App nichts von der Kamera.
   *[microphone] Tilo merkt sich diese Entscheidung für { $app } in diesem Arbeitsbereich, bis du sie widerrufst. Ohne deine Erlaubnis erhält die App nichts vom Mikrofon.
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
device-grant-room = im Arbeitsbereich { $room }
device-grant-revoke = Widerrufen
device-grant-revoke-description = Diese Entscheidung vergessen: eine laufende Aufnahme endet, und beim nächsten Mal wird erneut gefragt.
