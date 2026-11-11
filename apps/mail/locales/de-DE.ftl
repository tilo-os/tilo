# Mail — deutsche Übersetzung (de-DE).

## Fenster zum Schreiben
mail-compose-label-to = An
mail-compose-label-cc = Cc
mail-compose-label-bcc = Bcc
mail-compose-label-subject = Betreff
mail-compose-placeholder-to = Name oder E-Mail-Adresse
mail-compose-cc-bcc = Cc/Bcc
mail-compose-send = Senden
mail-compose-save-draft = Entwurf speichern
mail-compose-discard = Verwerfen
# Die Eingabe für einen Link über den Tasten.
mail-compose-prompt-link = Link
mail-compose-prompt-add = Link hinzufügen
mail-compose-prompt-cancel = Abbrechen

# Gemeinsame Auswahl aus dem Adressbuch. Die Hinweise werden ausdrücklich gewählt und belegen keine Identität.
mail-contact-recipients = Kontakte
mail-contact-write = Einen Kontakt anschreiben
mail-contact-draft-choice = Entwurf an { $person } ({ $id }) unter { $email } von { $sender } ({ $sender_account })
mail-contact-draft-prepared = Entwurf bereit
mail-contact-sender-changed = Das sendende Konto hat sich geändert
mail-contact-prepare = Entwurf vorbereiten
mail-contact-existing-draft = Es ist bereits ein Entwurf offen
mail-contact-unavailable = Freigegebene Kontakte sind nicht verfügbar
mail-contact-recipient-changed = Der Kontakt hat sich geändert; wähle ihn erneut und prüfe ihn
mail-contact-account-changed = Das sendende Konto hat sich geändert; wähle es erneut und prüfe es

## Verlauf: was jede Änderung getan hat, nach Feld und nach Aktion

mail-mark-read = Nachrichten { $read ->
    [true] als gelesen markiert
   *[false] als ungelesen markiert
}
mail-archive = Nachrichten archiviert
mail-trash = Nachrichten in den Papierkorb verschoben
mail-move-messages = Nachrichten nach { $to } verschoben
mail-purge = Nachrichten endgültig gelöscht
mail-ingest = Post abgerufen
mail-server-applied = Der Mailserver hat eine Änderung übernommen
mail-keep-copies = Abgerufene Post für deine anderen Geräte behalten
mail-forget-copies = Post verworfen, die ihr Server nicht mehr hält
mail-mail-copies-copies = Die für deine anderen Geräte behaltene Post geändert
mail-start-draft-body = Angefangen, eine Nachricht zu schreiben
mail-discard-draft = Den Entwurf verworfen
mail-send-message = Eine Nachricht von { $account } gesendet
mail-cancel-send = Das Senden einer Nachricht abgebrochen
mail-message-state-starred = { $new ->
    [true] Eine Nachricht mit einem Stern markiert
   *[false] Den Stern einer Nachricht entfernt
}
mail-message-state-folder = Eine Nachricht nach { $new } verschoben
mail-message-state-labels = Die Labels einer Nachricht geändert
mail-local-folder-name = Den Ordner { $item } umbenannt
mail-mail-folders = Die Ordner geändert
mail-mail-messages = Das Postfach geändert
mail-mail-outbox = Festgehalten, wie eine gesendete Nachricht ankam
mail-draft-state-sender = Gewählt, von wem der Entwurf gesendet wird
mail-draft-state-to = Geändert, an wen der Entwurf geht
mail-draft-state-cc = Geändert, wer den Entwurf als Kopie erhält
mail-draft-state-bcc = Geändert, wer den Entwurf als Blindkopie erhält
mail-draft-state-subject = Den Betreff des Entwurfs geändert
mail-draft-state-body = Den Entwurf bearbeitet
mail-draft-state-html-body = Den Entwurf bearbeitet
mail-draft-state-calendar-ics = Die Einladung im Entwurf geändert
mail-draft-state-attachments = Die Anhänge des Entwurfs geändert
mail-draft-state-rich-body = Den Entwurf bearbeitet
mail-save-draft = Den Entwurf gespeichert
mail-attach = { $name } an den Entwurf angehängt
mail-mail-server = Festgehalten, wie ein Mailserver eine Änderung übernommen hat
mail-mail-held = Die im Postfach gehaltene Post geändert
mail-held-message-message-id = Die Message-ID einer gehaltenen Nachricht geändert
mail-held-message-from = Den Absender einer gehaltenen Nachricht geändert
mail-held-message-to = Die Empfänger einer gehaltenen Nachricht geändert
mail-held-message-cc = Geändert, wer eine gehaltene Nachricht als Kopie erhält
mail-held-message-subject = Den Betreff einer gehaltenen Nachricht geändert
mail-held-message-body = Den Text einer gehaltenen Nachricht geändert
mail-held-message-minutes = Geändert, wann eine gehaltene Nachricht gesendet wurde
mail-held-message-in-reply-to = Geändert, welche Nachricht eine gehaltene Nachricht beantwortet
mail-held-message-conversation = Eine gehaltene Nachricht in eine andere Unterhaltung verschoben
mail-deliver-message = { $subject } zugestellt
mail-draft-message = Entwurf für { $subject } erstellt
mail-draft-reply = Eine Antwort entworfen

mail-forward-not-downloaded = { $count ->
    [one] { $files } ist noch nicht heruntergeladen und wird daher nicht angehängt
   *[other] { $files } sind noch nicht heruntergeladen und werden daher nicht angehängt
}
mail-forward-too-large = { $count ->
    [one] { $files } ist größer, als eine Nachricht fassen kann, und wird daher nicht angehängt
   *[other] { $files } sind größer, als eine Nachricht fassen kann, und werden daher nicht angehängt
}

mail-account-secret-unavailable = Das Passwort dieses Kontos konnte auf diesem Gerät nicht geöffnet werden. Prüfe das Konto in den Einstellungen unter Externe Konten.
mail-import-sign-in-required = Importierte Kontoeinstellungen verlangen eine Anmeldung. Melde dich an, bevor du verbindest.

## Leseansicht: Inhalte aus dem Netz, Links und Format einer HTML-Nachricht
mail-remote-blocked = Bilder aus dem Netz werden nicht geladen
mail-remote-load = Bilder laden
mail-remote-always = Für diesen Absender immer
mail-remote-loaded = Bilder aus dem Netz sind für diese Nachricht geladen
mail-remote-sender-loads = Bilder aus dem Netz dieses Absenders werden immer geladen
mail-remote-stop = Nicht mehr laden
mail-link-open = Link öffnen
mail-link-copy = Linkadresse kopieren
mail-copy = Kopieren
mail-select-all = Alles auswählen
mail-show-quoted = Zitierten Text anzeigen
mail-show-plain = Reinen Text anzeigen
mail-show-formatted = Formatiert anzeigen

## Wie das Fenster und seine Teile für assistive Technik heißen

mail-title = Mail
mail-a11y-queued-message = Wartende Nachricht
mail-a11y-outbox = Postausgang
mail-a11y-outbox-empty = Nichts wartet auf das Senden
mail-a11y-no-message = Keine Nachricht ausgewählt
mail-a11y-message-actions = Aktionen für die Nachricht
mail-a11y-bulk-select = Nachrichten auswählen
mail-a11y-bulk-done = Auswahl beenden
mail-a11y-thread-messages = { $count ->
    [one] { $count } Nachricht in dieser Unterhaltung
   *[other] { $count } Nachrichten in dieser Unterhaltung
}
mail-a11y-preview-unread = Vorschau, weiterhin ungelesen
mail-a11y-download-first = Lade diesen Anhang herunter, bevor du ihn speicherst
mail-a11y-syncing = Mail wird abgeglichen
mail-compose-placeholder-body = Schreib deine Nachricht
mail-compose-a11y = E-Mail schreiben
mail-compose-a11y-description = Fenster zum Schreiben einer E-Mail
mail-shortcuts-title = Tastenkürzel

## Fenster- und Barrierefreiheits-Texte
mail-a11y-mailboxes = Postfächer
