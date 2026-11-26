# Was der Ankunftsbildschirm sagt, wenn ein Schritt langsam ist oder die Seite
# nicht weiterkommt; Aufbau und Regeln in en-US.ftl.

## Statt einer Zeile, wenn ein Schritt des Hereinkommens langsam ist
arrival-slow-loading = Tilo lädt noch.
arrival-slow-connecting = Verbindet sich noch mit dem Arbeitsbereich.
arrival-slow-starting = Tilo startet noch.
arrival-slow-opening = Der Arbeitsbereich öffnet sich noch.
arrival-slow-drawing = Die Grafik startet noch. Ohne Grafikkarte dauert das länger.
# Steht mit „Noch einmal versuchen“ daneben, wenn ein Schritt lange nicht
# weiterkommt; die Seite wartet währenddessen weiter.
arrival-stalled = Das dauert länger, als es sollte. Warte weiter oder versuch es noch einmal.

## Statt einer Zeile, wenn die Seite nicht hineinkommt oder nicht weiter kann
# { $workspace } ist der Name des Arbeitsbereichs; eine Nachricht auf
# -unnamed steht, wo die Seite ihn nicht kennt.
arrival-retry = Noch einmal versuchen
arrival-failed-offline = Du bist offline. Änderungen bleiben auf diesem Gerät und gehen raus, sobald die Verbindung wieder da ist.
arrival-failed-disconnected = Dieser Browser ist nicht mit dem Arbeitsbereich verbunden. Änderungen bleiben auf diesem Gerät. Lade die Seite neu, um dich wieder zu verbinden.
arrival-failed-admission = Der Arbeitsbereich kann die Änderungen dieses Browsers gerade nicht annehmen. Die Änderungen bleiben auf diesem Gerät.
arrival-failed-saving = Deine Änderungen sind noch nicht auf diesem Gerät gespeichert. Lass diesen Tab offen und versuch es noch einmal.
# Ein privates Fenster oder eines, in dem Website-Daten aus sind: der Browser
# gibt der Seite keinen Dateispeicher, also zeichnet der Desktop hier, aber
# seine Apps behalten keine eigenen Dateien.
arrival-failed-files = Dieses Fenster behält keine Dateien, deshalb können Apps hier keine eigenen Dateien speichern. Öffne Tilo in einem normalen Fenster, um mit ihnen zu arbeiten.
arrival-failed-renderer = Der Desktop zeichnet nicht mehr. Versuch es noch einmal, um ihn neu zu starten, oder lade die Seite neu.
arrival-refusal-heading = { $workspace } ist im Moment nur mit Einladung offen
arrival-refusal-heading-unnamed = Dieser Arbeitsbereich ist im Moment nur mit Einladung offen
arrival-refusal-body = Bitte jemanden darin um einen Einladungslink und füge ihn hier ein. Bist du auf einem anderen Gerät schon darin, füge den Link ein, den „Pair a new device“ dort erzeugt.
arrival-refusal-sign-in-heading = Melde dich an, um { $workspace } zu öffnen
arrival-refusal-sign-in-heading-unnamed = Melde dich an, um diesen Arbeitsbereich zu öffnen
arrival-refusal-sign-in-body = Nur die Leute darin können diesen Arbeitsbereich öffnen. Melde dich als eine von ihnen an, oder füge einen Einladungslink ein, den dir jemand geschickt hat.
arrival-refusal-sign-in-wait-body = Nur die Leute darin können diesen Arbeitsbereich öffnen. Wenn du dazugehörst, melde dich an. Neu bei Tilo? Gerade geht es nur mit Einladung: Hinterlass deine E-Mail-Adresse, und wir schicken dir eine.
arrival-invite-link = Einladungs- oder Kopplungslink
arrival-invite-placeholder = Einladungs- oder Kopplungslink einfügen
arrival-invite-open = Öffnen
arrival-invite-not-a-link = Das ist kein Einladungs- oder Kopplungslink. Füge den ganzen Link ein.
arrival-refusal-wait-heading = Ein Arbeitsbereich für dich, dein Team und deine KI-Agenten
arrival-refusal-wait-body = Notizen, Aufgaben, Dateien und Chat an einem Ort, auf all deinen Geräten. Claude Code und andere Agenten arbeiten darin neben dir, und du siehst alles, was sie tun. In der frühen Alpha geht es nur mit Einladung: Hinterlass deine E-Mail-Adresse, und wir schicken dir eine.
arrival-wait-email = E-Mail-Adresse
arrival-wait-placeholder = du@beispiel.de
arrival-wait-join = Auf die Warteliste
arrival-wait-inbox = Schau in dein Postfach: Wir haben einen Link an { $email } geschickt, mit dem du bestätigst.
arrival-wait-again-prompt = Nichts angekommen?
arrival-wait-again = Noch mal senden
arrival-wait-again-sent = Erneut gesendet. Wir schicken höchstens einen Link pro Stunde, schau also auch im Spam-Ordner nach.
arrival-wait-listed = Du stehst auf der Liste ({ $email })
arrival-wait-confirmed = Du stehst auf der Liste.
arrival-wait-invited = Du bist eingeladen: Der Link ist in deinem Postfach.
arrival-wait-invited-no-mail = Du bist eingeladen: Wer dich eingeladen hat, schickt dir den Link.
arrival-wait-expired = Dieser Link gilt nicht mehr. Trag deine Adresse noch einmal ein.
arrival-wait-change = Doch nicht?
arrival-wait-leave = Nimm mich von der Liste
arrival-wait-left = Du stehst nicht mehr auf der Liste.
arrival-wait-leave-too-many = Gerade kommen zu viele Versuche aus deinem Netz. Versuch es später noch einmal.
arrival-wait-privacy = Wir nutzen deine E-Mail-Adresse nur, um dir eine Einladung zu schicken.
arrival-wait-privacy-mail = Wir schicken dir einen Link zum Bestätigen, dann deine Einladung. Sonst nichts.
arrival-wait-no-mail = Von hier kommt keine E-Mail: Wer dich einlädt, schickt dir den Link.
arrival-wait-contact = Fragen oder Austragen: { $contact }
arrival-wait-invalid = Das sieht nicht nach einer E-Mail-Adresse aus.
arrival-wait-too-many = Gerade kommen zu viele Anmeldungen aus deinem Netz. Versuch es später noch einmal.
arrival-wait-full = Die Warteliste ist im Moment voll. Versuch es in ein paar Tagen noch einmal.
arrival-wait-failed = Das hat nicht geklappt. Versuch es gleich noch einmal.
arrival-sign-in-ask = Du hast schon eine Tilo-Identität?
arrival-sign-in-agent = Mit deinem Agenten anmelden
arrival-invite-toggle = Du hast einen Einladungs- oder Kopplungslink?

## The reconnect screen: the page was not let in yet and tries again
# { $time } is how long until the next try, such as "40 s" or "2 min 05 s".
arrival-reconnect-unreachable = { $workspace } ist nicht erreichbar
arrival-reconnect-unreachable-unnamed = Der Arbeitsbereich ist nicht erreichbar
arrival-reconnect-unreachable-body = Vielleicht startet er gerade neu. Er öffnet sich hier, sobald er wieder da ist.
arrival-reconnect-unreachable-local-body = { $host } startet vielleicht gerade neu, oder dieser Browser vertraut seinem Zertifikat nicht. Öffne ihn, um zu sehen, was davon; der Arbeitsbereich öffnet sich hier, sobald er antwortet.
arrival-reconnect-refused = Noch nicht in { $workspace }
arrival-reconnect-refused-unnamed = Noch nicht im Arbeitsbereich
arrival-reconnect-refused-body = Er hat diesen Browser vorerst abgewiesen. Der nächste Versuch fragt als neues Gerät von dir.
arrival-reconnect-offline = Du bist offline
arrival-reconnect-offline-body = Der Arbeitsbereich öffnet sich, sobald du wieder online bist.
arrival-reconnect-busy = { $workspace } ist ausgelastet
arrival-reconnect-busy-unnamed = Der Arbeitsbereich ist ausgelastet
arrival-reconnect-busy-body = Gerade kommen sehr viele Leute herein. Er öffnet sich hier, sobald Platz ist.
arrival-reconnect-next = Nächster Versuch in { $time }
arrival-reconnect-trying = Wird versucht …
arrival-reconnect-waiting-online = Warte auf das Netzwerk …
arrival-reconnect-try-now = Jetzt versuchen
arrival-reconnect-details = Technische Details
arrival-reconnect-copy = Details kopieren
arrival-reconnect-copied = Kopiert
arrival-reconnect-copy-failed = Kopieren hat nicht geklappt. Markiere den Text stattdessen.

# The entry page's failure screen (failure-screen.mjs): a heading per case,
# named for the workspace where the page knows it, and a body that says the
# cause and what to do. `{ $server }` is the address of the server the page could
# not reach; `{ $step }` one of the entry-failure-step lines.
entry-failure-removed = Du wurdest aus { $workspace } entfernt
entry-failure-removed-unnamed = Du wurdest aus diesem Arbeitsbereich entfernt
entry-failure-removed-body = Um zurückzukommen, bitte jemanden darin um eine neue Einladung.
entry-failure-cooling-down = { $workspace } lässt dein Netz gerade nicht herein
entry-failure-cooling-down-unnamed = Dieser Arbeitsbereich lässt dein Netz gerade nicht herein
entry-failure-cooling-down-body = Jemand aus deinem Netz wurde daraus entfernt. Bitte jemanden darin um eine Einladung oder versuch es morgen wieder.
entry-failure-invitation-only = { $workspace } ist gerade nur mit Einladung offen
entry-failure-invitation-only-unnamed = Dieser Arbeitsbereich ist gerade nur mit Einladung offen
entry-failure-invitation-only-body = Bitte jemanden darin um einen Einladungslink und öffne ihn hier.
entry-failure-gone = { $workspace } gibt es nicht mehr
entry-failure-gone-unnamed = Diesen Arbeitsbereich gibt es nicht mehr
entry-failure-gone-body = Er wurde zurückgesetzt, seit dieser Browser ihn zuletzt geöffnet hat.
entry-failure-deleted = { $workspace } wurde gelöscht
entry-failure-deleted-unnamed = Dieser Arbeitsbereich wurde gelöscht
entry-failure-deleted-body = Die Person, der er gehört, hat ihn gelöscht. Er lässt sich nicht mehr öffnen.
entry-failure-no-access = { $workspace } hat diesen Browser nicht hereingelassen
entry-failure-no-access-unnamed = Dieser Arbeitsbereich hat diesen Browser nicht hereingelassen
entry-failure-no-access-body = Du bist nicht darin, oder es gibt ihn nicht mehr. Bitte jemanden darin um einen Einladungslink.
entry-failure-refused = { $workspace } ließ sich nicht öffnen
entry-failure-refused-unnamed = Dieser Arbeitsbereich ließ sich nicht öffnen
entry-failure-refused-body = { $reason }
entry-failure-device-key-busy = { $workspace } ist in deinen anderen Tabs offen
entry-failure-device-key-busy-unnamed = Dieser Arbeitsbereich ist in deinen anderen Tabs offen
entry-failure-device-key-busy-body = Dieser Browser kann ihn nur in einigen Tabs zugleich öffnen. Schließ einen der anderen und versuch es dann noch einmal.
entry-failure-offline = Du bist offline
entry-failure-offline-unnamed = Du bist offline
entry-failure-offline-body = Der Arbeitsbereich öffnet sich hier, sobald du wieder online bist.
entry-failure-unreachable = Keine Verbindung zu { $server }
entry-failure-unreachable-unnamed = Keine Verbindung zu { $server }
entry-failure-unreachable-body = Diese Seite lädt, deine Verbindung funktioniert also. Vielleicht startet der Host gerade neu: Versuch es gleich noch einmal.
entry-failure-unreachable-local = Keine Verbindung zu { $server }
entry-failure-unreachable-local-unnamed = Keine Verbindung zu { $server }
entry-failure-unreachable-local-body = Diese Seite lädt, deine Verbindung funktioniert also. Entweder ist der Host aus, oder dieser Browser vertraut seinem Zertifikat nicht. Öffne { $server }, um zu sehen, was davon, und versuch es dann noch einmal.
entry-failure-no-connection = Keine Verbindung zu { $server }
entry-failure-no-connection-unnamed = Keine Verbindung zu { $server }
entry-failure-no-connection-body = Auch diese Seite antwortet nicht. Prüf deine Verbindung und versuch es dann noch einmal.
entry-failure-host-refused = { $server } hat diesem Tab den Zugang zu { $workspace } verweigert
entry-failure-host-refused-unnamed = { $server } hat diesem Tab den Zugang verweigert
entry-failure-host-refused-body = Er sagte: { $reason }.
entry-failure-host-silent = { $server } hat diesen Tab nicht in { $workspace } gelassen
entry-failure-host-silent-unnamed = { $server } hat diesen Tab nicht hereingelassen
entry-failure-host-silent-body = Er hat geantwortet, diesen Tab aber warten lassen. Versuch es gleich noch einmal.
entry-failure-certificate = { $server } hat diesen Browser nicht angenommen
entry-failure-certificate-unnamed = { $server } hat diesen Browser nicht angenommen
entry-failure-certificate-body = Er hat das Zertifikat abgelehnt, mit dem sich dieser Browser anmeldet.
entry-failure-incompatible = { $server } konnte nicht lesen, was diese Seite geschickt hat
entry-failure-incompatible-unnamed = { $server } konnte nicht lesen, was diese Seite geschickt hat
entry-failure-incompatible-body = Diese Seite und { $server } laufen vielleicht mit verschiedenen Tilo-Versionen. Versuch es noch einmal, dann lädt die Seite neu.
entry-failure-join-stopped = { $server } hat das Beitreten dieses Tabs zu { $workspace } beendet
entry-failure-join-stopped-unnamed = { $server } hat das Beitreten dieses Tabs beendet
entry-failure-join-stopped-body = Er sagte: { $reason } Versuch es noch einmal.
entry-failure-host-error = { $server } konnte { $workspace } nicht öffnen
entry-failure-host-error-unnamed = { $server } konnte diesen Arbeitsbereich nicht öffnen
entry-failure-host-error-body = Er hat mit einem Fehler geantwortet. Falls er gerade neu gestartet ist, öffnet ihn ein neuer Versuch gleich.
entry-failure-desktop = { $workspace } ist nicht gestartet
entry-failure-desktop-unnamed = Dieser Arbeitsbereich ist nicht gestartet
entry-failure-desktop-body = { $step } Deine Fenster und Dokumente bleiben in diesem Browser, ein neuer Versuch macht also da weiter, wo du warst.
entry-failure-stopped = { $workspace } funktioniert nicht mehr
entry-failure-stopped-unnamed = Der Desktop funktioniert nicht mehr
entry-failure-stopped-body = Deine Fenster und Dokumente bleiben in diesem Browser, ein neuer Versuch macht also da weiter, wo du warst.
entry-failure-loading = { $workspace } ließ sich nicht öffnen
entry-failure-loading-unnamed = Tilo ließ sich nicht öffnen
entry-failure-loading-body = { $step } Versuch es noch einmal.
entry-failure-step-load = Es blieb beim Laden von Tilo stehen.
entry-failure-step-join = Es blieb beim Beitreten zum Arbeitsbereich stehen.
entry-failure-step-open = Es blieb beim Einrichten des Desktops stehen.
entry-failure-step-draw = Es blieb beim Zeichnen des ersten Bildes stehen.
entry-failure-the-host = der Host
entry-failure-refused-fallback = Dieser Link oder diese Einladung lässt sich nicht verwenden.
entry-failure-open-host = { $server } öffnen
entry-failure-others = Oder öffne einen anderen deiner Arbeitsbereiche
entry-failure-elsewhere = Oder öffne stattdessen eines davon

## Die 24 Wiederherstellungswörter, einmal gezeigt, sobald eine Person
## hereingelassen wird; Aufbau und Regeln in en-US.ftl.
recovery-words-heading = Deine 24 Wiederherstellungswörter
recovery-words-body = Diese 24 Wörter sind der einzige Weg zurück in Tilo, wenn du alle Geräte verlierst. Schreib sie der Reihe nach auf und bewahre sie sicher auf. Wer sie hat, kann sich als du ausgeben.
recovery-words-copy = Kopieren
recovery-words-copied = Kopiert
recovery-words-confirm = Ich habe sie aufgeschrieben

workspace-create-name = Name des neuen Arbeitsbereichs
workspace-create-submit = Erstellen
workspace-create-cancel = Abbrechen
workspace-create-retry = Der Host hat noch nicht geantwortet. Drücke erneut auf Erstellen, um denselben Arbeitsbereich zu öffnen.
