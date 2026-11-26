## Einrichtung

onboarding-resource = Einrichtung
onboarding-back = Zurück
onboarding-cancel = Abbrechen
onboarding-continue = Weiter
onboarding-recovery-key-different = Diese Wörter gehören zu einer anderen Identität
onboarding-picker-import-title = Eine Tilo-Identität auswählen
onboarding-picker-import-filter = Tilo-Identität
onboarding-nearby-ticket-empty = Gib den Code ein, den dein anderes Gerät angezeigt hat.
onboarding-picker-backup-title = Verschlüsselte Identitätssicherung speichern
onboarding-picker-backup-filter = Verschlüsselte Tilo-Identität
onboarding-finish = Weiter zum Desktop

## Was bei der Einrichtung schiefging

# Koppeln mit einem Gerät in der Nähe: was fehlschlug und was als Nächstes zu tun ist
onboarding-nearby-failed-title = Dieser Rechner wurde nicht hinzugefügt
onboarding-nearby-unknown = Versuch es erneut oder erzeuge mit tilo device add auf deinem anderen Gerät einen neuen Code.
onboarding-nearby-detail = Details: { $detail }
onboarding-nearby-wrong-title = Diesen Code kann dieser Computer nicht verwenden
onboarding-nearby-wrong = Kopiere die ganze Zeile, die tilo device add auf deinem anderen Gerät angezeigt hat, beginnend mit tilo1.
onboarding-nearby-used-title = Dieser Code wurde schon verwendet
onboarding-nearby-used = Jeder Code gilt nur einmal. Erzeuge mit tilo device add auf deinem anderen Gerät einen neuen.
onboarding-nearby-expired-title = Dieser Code ist abgelaufen
onboarding-nearby-expired = Ein Code gilt zehn Minuten. Erzeuge mit tilo device add auf deinem anderen Gerät einen neuen.
onboarding-nearby-unreachable-title = Dein anderes Gerät war nicht erreichbar
onboarding-nearby-unreachable = Prüfe, ob es an und verbunden ist, und versuch es dann erneut. Wenn sein tilo device add beendet wurde, erzeuge einen neuen Code.
onboarding-nearby-storage-title = Dieser Computer konnte sich nicht als Gerät speichern
onboarding-nearby-storage = Das Gerätezertifikat dieses Computers konnte nicht geschrieben werden. Gib Speicherplatz frei oder korrigiere die Rechte seines Identitätsordners und versuch es dann erneut.
onboarding-nearby-not-started-title = Tilos Hintergrundabgleich ist nicht gestartet
onboarding-nearby-not-started = Über ihn tritt dieser Computer bei. Versuch es erneut; wenn es weiter fehlschlägt, führe tilo status in einem Terminal aus.
onboarding-nearby-unavailable-title = Einen Computer hinzuzufügen ist hier nicht verfügbar
onboarding-nearby-unavailable = Diesen Rechner mit einem Code hinzuzufügen ist in dieser Sitzung nicht verfügbar
onboarding-nearby-cancelled = Das Hinzufügen dieses Computers wurde beendet. Wähle „Beitreten“, um es erneut zu versuchen.

# Einrichtung auf diesem Computer: was fehlschlug und was als Nächstes zu tun ist
onboarding-progress-unreadable = Die gespeicherte Einrichtungsposition konnte nicht gelesen werden ({ $error }). Deine Identität ist sicher; die Einrichtung wurde beim Abschlussschritt fortgesetzt.
onboarding-progress-invalid-identity = Die Einrichtung hat unvollständigen Fortschritt gefunden, konnte die vorhandene Identität aber nicht prüfen: { $error }
onboarding-progress-save-failed = Die Einrichtung konnte ihre Neustartposition nicht speichern: { $error }. Es wurde keine Identitätsaktion wiederholt.
onboarding-progress-clear-failed = Die Einrichtung ist abgeschlossen, aber ihre Neustartmarkierung konnte nicht entfernt werden: { $error }. Wähle erneut „Fertigstellen“, damit dieser Bildschirm bei der nächsten Anmeldung nicht wieder erscheint.
onboarding-recovery-key-count = Ein Wiederherstellungsschlüssel hat 24 Wörter; dieser hat { $found }.
onboarding-recovery-key-unknown-word = Wort { $position } ist kein Wort eines Wiederherstellungsschlüssels. Prüfe die Schreibweise.
onboarding-recovery-key-typo = Wort { $position }, „{ $word }“, ist kein Wort eines Wiederherstellungsschlüssels. Prüfe die Schreibweise.
onboarding-recovery-key-suggestion = Wort { $position }, „{ $word }“, ist kein Wort eines Wiederherstellungsschlüssels. Meintest du „{ $suggestion }“?
onboarding-recovery-key-checksum = Diese Wörter ergeben keinen Wiederherstellungsschlüssel. Wahrscheinlich ist eines vertippt oder vertauscht.
onboarding-connect-recovery-message = Deine vorhandene Identität konnte nicht geprüft werden. Entsperre den Schlüsselbund des Desktops und versuche es erneut, oder stelle eine verschlüsselte Sicherung wieder her. Bis dahin bleibt deine Identität, wie sie ist.
onboarding-connect-choose-file-first = Wähle zuerst eine verschlüsselte Sicherung oder eine ältere Identitätsdatei aus.
onboarding-keyring-unavailable = Der Schlüsselbund des Desktops ist weiterhin nicht verfügbar. Entsperre ihn und versuche es erneut, oder stelle eine verschlüsselte Sicherung wieder her: { $error }
onboarding-passphrase-too-short = Verwende mindestens 12 Zeichen. Mach sie lang und einzigartig; Länge allein macht eine gängige Passphrase nicht sicher.
onboarding-passphrase-mismatch = Die beiden Passphrasen stimmen nicht überein.
onboarding-passphrase-unavailable = Die Passphrasefelder sind vorübergehend nicht verfügbar.
onboarding-codecs-installer-failed = Der Download konnte nicht gestartet werden: { $error }

## Aktionen der Einrichtung

so-tilo-onboarding-step-acknowledge = Bestätigen
so-tilo-onboarding-step-browse = Eine Datei wählen
so-tilo-onboarding-step-choose = Einen anderen Weg wählen
so-tilo-onboarding-step-enter = Einen Wert eingeben
so-tilo-onboarding-step-enter-secret = Ein Geheimnis eingeben
so-tilo-onboarding-step-retry = Erneut versuchen
so-tilo-onboarding-step-skip = Überspringen
