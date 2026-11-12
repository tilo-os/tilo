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

## Was das Fenster zeigt
mail-view-drawer = Ordnerleiste
mail-view-reading = Lesebereich
mail-view-list = Nachrichtenliste
mail-view-all = Ordner, Nachrichtenliste und Lesebereich

## Fakten, Eigenschaftsnamen und Bereichstitel
mail-a11y-current-view = Aktuelle Ansicht: { $view }
mail-a11y-reading = Geöffnet: { $subject }
mail-a11y-save-attachment = Anhang { $file } ({ $size }) speichern
mail-a11y-refused = Der Server hat diese Nachricht abgelehnt: { $reason }
mail-a11y-cancel-sending = Senden an { $to } abbrechen
mail-a11y-outbox-row = An { $to }: { $subject }
mail-a11y-no-messages = Keine Nachrichten in { $folder }
mail-status-send-failed = Senden fehlgeschlagen: { $error }
mail-purge-body = { $count ->
    [one] Eine Nachricht im Papierkorb wird aus diesem Postfach und auf dem Server gelöscht. Das lässt sich nicht rückgängig machen.
   *[other] { $count } Nachrichten im Papierkorb werden aus diesem Postfach und auf dem Server gelöscht. Das lässt sich nicht rückgängig machen.
}
mail-status-attach-failed = { $name } konnte nicht angehängt werden: { $error }
mail-status-attached = { $name } angehängt

mail-status-picker-failed = Die Anhangsauswahl ließ sich nicht öffnen: { $error }

mail-status-archived = { $count ->
    [one] Eine Nachricht archiviert
   *[other] { $count } Nachrichten archiviert
}
mail-status-trashed = { $count ->
    [one] Eine Nachricht in den Papierkorb verschoben
   *[other] { $count } Nachrichten in den Papierkorb verschoben
}
mail-status-marked-read = { $count ->
    [one] Eine Nachricht als gelesen markiert
   *[other] { $count } Nachrichten als gelesen markiert
}
mail-status-marked-unread = { $count ->
    [one] Eine Nachricht als ungelesen markiert
   *[other] { $count } Nachrichten als ungelesen markiert
}
mail-status-deleted = { $count ->
    [one] Eine Nachricht gelöscht
   *[other] { $count } Nachrichten gelöscht
}
mail-status-account-not-saved = Konto ausgewählt, aber die Einstellung ließ sich nicht speichern: { $error }
mail-status-purged = { $count ->
    [one] Eine Nachricht aus dem Papierkorb endgültig gelöscht
   *[other] { $count } Nachrichten aus dem Papierkorb endgültig gelöscht
}
mail-status-purge-nothing = Nichts gelöscht; die ausgewählten Nachrichten haben sich vor der Bestätigung geändert
mail-status-preferences-not-saved = Deine Mail-Einstellungen ließen sich nicht speichern: { $error }

mail-from = Von { $sender }
mail-status-copy-cancelled = Speichern des Anhangs abgebrochen
mail-status-copy-saved = Kopie gespeichert als { $file }
mail-status-selection-cancelled = Auswahl des Anhangs abgebrochen
mail-status-selection-composer-closed = Auswahl des Anhangs abgebrochen, weil das Verfassen-Fenster geschlossen wurde
mail-status-picker-disconnected = Die Anhangsauswahl wurde getrennt, bevor eine Datei gewählt war
mail-status-choose-file = Wähle eine Datei zum Anhängen

## What the window shows
mail-attachment-not-downloaded = Der Inhalt des Anhangs ist noch nicht heruntergeladen
mail-draft-subject = Entwurf: { $subject }
mail-not-sent-yet = Diese Nachricht wurde noch nicht gesendet.
mail-no-subject = Ohne Betreff
mail-to = An { $recipient }
mail-preview = Vorschau
mail-preview-unread = Vorschau, noch ungelesen

mail-row-name = { $sender }: { $subject }
mail-list-name = Nachrichten in { $folder }
mail-selected-count = { $count } ausgewählt
mail-bulk-select-all = Alle sichtbaren Nachrichten auswählen
mail-bulk-archive = Ausgewählte Nachrichten archivieren
mail-bulk-read = Ausgewählte Nachrichten als gelesen markieren
mail-bulk-unread = Ausgewählte Nachrichten als ungelesen markieren
mail-bulk-purge = Ausgewählte gelöschte Nachrichten endgültig löschen
mail-bulk-delete = Ausgewählte Nachrichten in den Papierkorb legen
mail-draft-subject-to = Entwurf: { $subject }, an { $to }

## What the folders are called
mail-folder-inbox = Posteingang
mail-folder-starred = Markiert
mail-folder-sent = Gesendet
mail-folder-drafts = Entwürfe
mail-folder-archive = Archiv
mail-folder-trash = Papierkorb
mail-folder-junk = Spam
mail-refresh-offline = { $folder } für { $account } lässt sich nicht aktualisieren, solange IMAP offline ist
mail-first-run-message = E-Mail-Konten werden in den Einstellungen unter Externe Konten hinzugefügt.

## Die Plätze der Fensterleiste
mail-bulk-done = Fertig
mail-delete = Löschen
mail-delete-permanently = Endgültig löschen
mail-reply = Antworten
mail-reply-all = Allen antworten
mail-forward = Weiterleiten
mail-move-to-inbox = In den Posteingang
mail-star = Markieren
mail-add-account = Konto hinzufügen
mail-new-message = Neue Nachricht
mail-cancel-sending = Senden abbrechen
mail-bar-discard-draft = Entwurf verwerfen
mail-close-keep-draft = Schließen und im Entwurf behalten

## Eine Datei anhängen und einen empfangenen Anhang speichern
mail-compose-attach-file = Datei anhängen
mail-save-attachment = Anhang speichern

## Actions

mail-effects-compose = Nachricht verfassen
mail-effects-contact-recipients = Empfänger wählen
mail-effects-account-settings = Kontoeinstellungen
mail-effects-prepare-contact-draft = Entwurf an einen Kontakt
mail-effects-refresh = Ordner aktualisieren
mail-effects-prepare-calendar-invitation = Kalendereinladung vorbereiten
mail-effects-open = Öffnen
mail-effects-read-message = Nachricht lesen
mail-effects-load-remote-content = Entfernte Bilder laden
mail-effects-always-load-remote-content = Entfernte Bilder immer laden
mail-effects-search-messages = Nachrichten suchen
mail-effects-draft-reply = Antwort entwerfen
mail-effects-open-attachment = Anhang öffnen
mail-effects-content = Inhalt abrufen
mail-effects-export-content = Inhalt exportieren
mail-mailbox = E-Mail
mail-mailbox-mark-read = Als gelesen markieren
mail-mailbox-trash = In den Papierkorb
mail-mailbox-move-messages = Nachrichten verschieben
mail-mailbox-ingest = Abgerufene Nachrichten übernehmen
mail-mailbox-server-applied = Server hat die Änderung übernommen
mail-mailbox-start-draft-body = Entwurfstext beginnen
mail-mailbox-create-draft = Neuer Entwurf
mail-mailbox-show-draft = Entwurf anzeigen
mail-mailbox-attach = An Entwurf anhängen
mail-mailbox-send-message = Nachricht senden
mail-mailbox-deliver-message = Nachricht zustellen
mail-mailbox-draft-message = Nachricht entwerfen
mail-trusted-senders = Vertrauenswürdige Absender
mail-trusted-senders-trust = Diesem Absender vertrauen
mail-trusted-senders-distrust = Diesem Absender nicht mehr vertrauen
