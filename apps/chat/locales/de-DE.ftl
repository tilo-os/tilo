# Chat — deutsche Sprache (de-DE).

chat-direct-messages-section = Direktnachrichten
chat-channels-title = Kanäle
chat-person-chat-private = Private Unterhaltung
# Die Verfasserin oder der Mitglied einer Nachricht, dessen Profilkarte sich öffnet.
chat-person-card = { $name }, Profil
chat-person-chat-placeholder = Private Nachricht schreiben
chat-assistant-composer-placeholder = Nachricht an deinen Assistenten
chat-person-chat-empty = Schreib eine Nachricht, bevor du sie sendest.

# Kennzeichnung in der `@`-Auswahl für Software statt einer Person, damit auf
# einen Blick klar ist, wer angesprochen wird.
chat-mention-agent-tag = Agent

# Bedienelemente am unteren Rand des geöffneten Anrufs.
chat-call-mute = Stummschalten
chat-call-unmute = Stummschaltung aufheben
# Das stumme Mikrofon eines Anrufs auf einem Gerät ohne Mikrofon: warum es stumm bleibt.
chat-call-no-microphone = Kein Mikrofon
chat-call-deafen = Ton aus
chat-call-undeafen = Ton an
chat-call-audio-only = Nur Audio
chat-call-collapse = Einklappen
chat-call-leave = Verlassen
chat-call-camera-on = Kamera
chat-call-camera-off = Kamera aus
chat-call-share-screen = Bildschirm teilen
chat-call-stop-sharing = Teilen beenden

# Emoji-Auswahl: die Reaktions-Schaltfläche, der Name des Suchfelds und eine
# Gruppenkarte ohne eigenen Namen.
chat-emoji-add-reaction = Reaktion hinzufügen
chat-emoji-search = Emoji suchen
chat-emoji-add = Emoji hinzufügen
chat-emoji-group = Emoji-Gruppe
# Eine Kachel der Auswahl; der Name ist der Name des Emojis in der Sprache des Desktops.
chat-emoji-choose = { $name } wählen

# Zustand der Sprachverbindung, gemeinsam für native und Browser-Geräte.
chat-call-issue-unavailable = Anrufton kann auf diesem Gerät nicht starten
chat-call-issue-denied = Du kannst in diesem Arbeitsbereich nicht an Anrufen teilnehmen
chat-call-issue-permission-denied = Mikrofon blockiert. Erlaube es in den Einstellungen und hebe dann die Stummschaltung auf.
chat-call-issue-source-lost = Mikrofon getrennt
chat-call-issue-capture-failed = Mikrofon konnte nicht starten
chat-call-issue-transport-failed = Anruf getrennt
chat-call-issue-session-ended = Anruf beendet
chat-call-issue-stale = Anrufsitzung abgelaufen
chat-call-issue-camera-denied = Kamera blockiert. Erlaube sie in den Einstellungen und schalte sie dann ein.
chat-call-issue-camera-lost = Kamera gestoppt
chat-call-issue-screen-denied = Bildschirmfreigabe wurde nicht erlaubt
chat-call-issue-screen-lost = Bildschirmfreigabe gestoppt
chat-call-issue-unsupported = Dieser Browser kann nicht an Anrufen teilnehmen. Nutze eine aktuelle Version von Chrome, Edge, Firefox oder Safari.

chat-call-title = Anruf: { $channel }
# Mit wem ein Anruf läuft, wie die Pille der Leiste und die Kopfzeile der
# Karte sie nennen: die Menschen darin, nie die Kennung des Kanals.
chat-call-people-two = { $first } und { $second }
# Ein Anruf, dem noch niemand beigetreten ist, wie die Kopfzeile der Karte ihn nennt.
# What a person without a name reads their own lines under.
chat-person-you = Du
chat-call-only-you = Nur du
chat-call-people-many = { $first }, { $second } und { $count } weitere
chat-call-open = Anrufunterhaltung öffnen
chat-call-join = Stumm beitreten
chat-call-leave-action = Anruf verlassen
chat-call-status = Anrufstatus

# Die Verbindungszeile unter dem Video des Anrufffensters.
chat-call-connected = Verbunden
chat-call-member = { $name }: Lautstärke und Stummschaltung für dich
chat-call-silence = Für mich stummschalten
chat-call-volume = Lautstärke { $percent } %
chat-call-reconnecting = Verbindung wird wiederhergestellt
chat-call-joining = Beitreten
chat-call-voice-unavailable = Sprache nicht verfügbar

# Ein Anhang, auf den die lesende Person keinen Zugriff hat: die Karte sagt, warum er nicht aufgeht.
chat-attachment-no-access = Du hast keinen Zugriff auf diese Datei
chat-attachment-close = Schließen
chat-attachment-loading = Bild wird geladen …
chat-attachment-save-title = Anhang speichern
chat-attachment-save-accept = Speichern
chat-attachment-save-failed = Die Datei konnte nicht gespeichert werden
chat-attachment-not-on-device = Diese Datei ist nicht auf diesem Gerät
chat-attachment-saved = Eine Kopie gespeichert
chat-call-unavailable = Nicht verfügbar

# Wirkungen auf die Chat-Wurzel, die einen Anruf im aktiven Arbeitsbereich
# starten (Startseite, Befehlspalette, Agents). Video wird angeboten, ist aber
# nicht verfügbar.
chat-start-call = Anruf starten
chat-start-video-call = Videoanruf starten

# Belege, die an der Antwort eines Agents hängen; die Verfügbarkeit kommt aus der Präsenz.
chat-agent-work-waiting = Wartet auf { $app }
chat-agent-work-did = { $count ->
    [one] Hat eine Sache erledigt
   *[other] Hat { $count } Sachen erledigt
}
# The one line of an agent at work: the step it is at ($what) and how many it has taken.
chat-agent-work-at-step = { $what } ({ $count ->
    [one] 1 Schritt
   *[other] { $count } Schritte
})
# A step ($step) and what it is about ($about), both in the system's own words.
chat-agent-work-step-about = { $step }: { $about }
chat-agent-work-refused = Diese Anfrage konnte nicht abgeschlossen werden
chat-agent-work-step-requested = Angefragt: { $target }
chat-agent-work-step-applied = Angewandt: { $target }
chat-agent-work-step-no-change = Keine Änderung: { $target }
chat-agent-work-step-refused = Abgelehnt: { $target }
chat-agent-work-step-refused-why = Abgelehnt: { $target }: { $why }
chat-agent-work-show-details = Aktivität anzeigen
chat-agent-work-hide-details = Aktivität ausblenden
chat-agent-provider-reasoning = Begründung des Anbieters
chat-agent-show-reasoning = Begründung anzeigen
chat-agent-hide-reasoning = Begründung ausblenden
chat-agent-working = Arbeitet
chat-agent-complete = Fertig
chat-agent-truncated = Antwort unterbrochen
chat-agent-refused = Anfrage abgelehnt
chat-agent-cancelled = Gestoppt
chat-agent-stop = Antwort stoppen
chat-agent-stopped-by-you = Von dir gestoppt
chat-agent-stopped-by = Von { $person } gestoppt
# A stop of an agent's work that Chat did not take; $reason says why.
chat-agent-not-stopped = Nicht gestoppt: { $reason }
chat-agent-failed = Konnte nicht abgeschlossen werden

# Was eine Antwort gegeben hat: das Modell ($model), sein Anbieter ($provider) und was der
# Durchlauf gekostet hat ($cost), mit den verbrauchten Tokens ($tokens), sofern gezählt.
chat-agent-answered = { $model }, { $provider }, { $cost }
# Das Modell, auf dem ein Gespräch mit einem Agenten läuft, bevor der Host eines nennt, und die
# Wahl, die wieder der Vorgabe folgt.
chat-assistant-setup = Persönlichen Assistenten einrichten
chat-open-assistant = Assistenten öffnen
chat-review-proposals = Vorschläge prüfen
chat-assistant-grant-missing = Dein Assistent braucht die Erlaubnis, in diesem Arbeitsbereich zu antworten.
chat-assistant-grant-choices = Wähle, was dein Assistent hier sechs Tage lang nutzen darf. Für weitere Aufgaben braucht er Chat, Aufgaben, Assistent und Kurzbefehle.
chat-assistant-grant-request = Nur Chat anfragen
chat-assistant-grant-work = Zugriff für weitere Aufgaben anfragen
chat-assistant-grant-pending = Öffne diesen Arbeitsbereich in dem Browser, in dem du deine Identität erstellt oder wiederhergestellt hast, um die Anfrage zu genehmigen.
chat-assistant-grant-refresh = Genehmigung prüfen
chat-assistant-grant-denied = Deine Anfrage wurde abgelehnt. Du kannst eine neue Anfrage stellen.
chat-assistant-grant-expired = Die Berechtigung deines Assistenten ist abgelaufen. Fordere den Zugriff erneut an.
chat-assistant-grant-revoked = Der Zugriff deines Assistenten wurde widerrufen. Du kannst ihn erneut anfordern.
chat-assistant-grant-connecting = Der Zugriff ist genehmigt. Der Host des Arbeitsbereichs stellt die Verbindung zum Assistenten her.
chat-assistant-grant-without-chat = Die aktuelle Berechtigung umfasst Chat nicht. Widerrufe sie, um einen anderen Zugriff anzufordern.
chat-assistant-grant-active = Dein Assistent hat Zugriff auf diesen Arbeitsbereich.
chat-assistant-grant-manage = Assistentenzugriff verwalten
chat-assistant-grant-revoke = Zugriff widerrufen
chat-assistant-grant-scope = Erlaubt den Zugriff auf { $apps }.
chat-assistant-grant-until = Gültig bis { $date }
chat-assistant-grant-all-apps = alle Apps
chat-assistant-app-tasks = Aufgaben
chat-assistant-app-assistant = Assistent
chat-assistant-app-shortcuts = Kurzbefehle
chat-assistant-grant-cancelled = Die Anfrage wurde abgebrochen.
chat-agent-model-settings = Modellanbieter verbinden
chat-assistant-needs-person = Melde dich mit deinem Konto an, um deinen Assistenten einzurichten.
chat-agent-model = Modell
chat-agent-model-default = Dem Standardmodell folgen
# Was die Antworten dieses Gesprächs bisher gekostet haben ($cost), die zweite Zeile des
# Modellwechslers.
chat-agent-model-cost = Bisherige Antworten: { $cost }
chat-agent-answered-tokens = { $model }, { $provider }, { $cost }, { $tokens ->
    [one] 1 Token
   *[other] { $tokens } Token
}
chat-model-ask = Assistent fragen
chat-model-stop = Stoppen
chat-model-retry = Erneut versuchen
chat-model-continue = Fortsetzen
chat-model-author = Assistent
chat-model-attribution = { $model }, angefragt von { $person }
chat-bot-attribution = { $name } (Bot: { $bot })
chat-via-attribution = { $name }, über { $service }
chat-bridge-name = { $service }-Brücke
chat-bot-name = Bot
chat-thread-start = Beginn des Threads
chat-thread-end = Neueste Antwort
chat-thread-title = Thread
chat-thread-close = Thread schließen
chat-thread-replies = { $count ->
    [one] 1 Antwort
   *[other] { $count } Antworten
    }

chat-context-sharing = Teilt den Live-Kontext von Chat
chat-context-following = Folgt dem Live-Kontext von Chat
chat-context-stop = Teilen beenden
chat-header-call-open = Anruf öffnen

chat-identity-available = Verfügbar
chat-identity-in-voice = Im Sprachraum
chat-member-in-channel = In { $channel }

chat-resource-copy-link = Link kopieren
chat-resource-not-opened = Konnte nicht geöffnet werden
chat-agent-kind = Agent
chat-channel-empty-message = Sag Hallo. Alle in #{ $channel } sehen es.
chat-send = Senden
chat-voice-channel = Sprachkanal
chat-quote-caption = Antwort auf
chat-quote-earlier = Eine Nachricht weiter oben
chat-conversation-read-only = Du kannst diese Unterhaltung lesen, aber nicht darin schreiben.
chat-direct-unavailable = Diese Unterhaltung ist noch nicht erreichbar: Dieses Gerät lädt den Arbeitsbereich noch.
chat-person-unknown = In diesem Arbeitsbereich gibt es niemanden zum Anschreiben.
chat-resource-link-copied = Link kopiert
chat-resource-link-unavailable = Link nicht verfügbar

## Ein Fenster, das live in einen Kanal geteilt wird
chat-share-window = Fenster teilen
chat-share-window-detail = Eine Live-Ansicht, die andere öffnen können
chat-share-window-to = Ein Fenster in { $place } teilen
chat-share-window-button = Teilen
chat-share-your-windows = Deine Fenster
chat-share-no-windows = Öffne das Fenster, das du teilen willst, und komm dann hierher zurück.
chat-share-view-only = Nur ansehen
chat-share-already-edit = Du kannst das schon bearbeiten
chat-share-can-edit = Kann bearbeiten
chat-share-live = Live
chat-share-ended = Nicht mehr geteilt
chat-share-stop = Teilen beenden
chat-share-loading = Live-Ansicht wird geöffnet …
chat-share-open-to-see = Öffne es, um es live zu sehen
chat-share-open = { $title } öffnen, geteilt von { $name }
chat-share-work-here = { $title } hier bearbeiten, geteilt von { $name }
chat-share-read-here = { $title } hier lesen, geteilt von { $name }
chat-share-open-window = { $title } in einem Fenster öffnen
chat-share-make-smaller = { $title } verkleinern
chat-share-pick = { $title } teilen
chat-voice-unavailable = Anrufe können in diesem Arbeitsbereich noch nicht starten
chat-call-not-joined = Tritt zuerst dem Anruf bei.

# „Du“ gehört der lesenden Person; geteilte und alte Namen nennen ihre Person.
chat-person-relative-name = { $name } ({ $tag })

## Verlauf: was jede Änderung getan hat, nach Verb und nach Feld
chat-begin-chat = Hat den Chat begonnen
chat-create-category = Hat die Kategorie { $name } angelegt
chat-create-channel = Hat den Kanal #{ $name } angelegt
chat-create-channel-with-id = Hat den Kanal #{ $name } angelegt
chat-delete-category = Hat die Kategorie { $item } gelöscht
chat-move-channel = { $up ->
    [true] Hat #{ $item } nach oben verschoben
   *[false] Hat #{ $item } nach unten verschoben
}
chat-move-category = { $up ->
    [true] Hat die Kategorie { $item } nach oben verschoben
   *[false] Hat die Kategorie { $item } nach unten verschoben
}
chat-arrange-channels = Hat die Kanäle neu geordnet
chat-delete-channel = Hat den Kanal #{ $item } gelöscht
chat-rename-channel = Hat #{ $item } in #{ $name } umbenannt
chat-rename-category = Hat die Kategorie { $item } in { $name } umbenannt
chat-set-channel-topic = Hat das Thema von #{ $item } auf { $topic } gesetzt
chat-set-channel-category = Hat #{ $item } in eine andere Kategorie verschoben
chat-archive-channel = { $archived ->
    [true] Hat #{ $item } archiviert
   *[false] Hat #{ $item } wiederhergestellt
}
chat-start-draft = Hat einen Entwurf begonnen
## Verlauf: ein Beitrag wird mit der einen Zeile zitiert, die sein Element festhält.
chat-post = „{ $item }“ geschrieben
chat-post-file = Hat die Datei { $name } geteilt
chat-post-view = Die Ansicht „{ $item }“ gepostet
chat-share-resource = Hat einen Link in #{ $item } geteilt
chat-deliver-message = Hat eine Nachricht von { $author } zugestellt
chat-edit-message = Hat eine Nachricht bearbeitet: { $text }
chat-delete-message = Hat eine Nachricht gelöscht
chat-mark-read = Hat #{ $item } als gelesen markiert
chat-keep-summary = Hat eine Zusammenfassung der Unterhaltung festgehalten
chat-record-work = Hat festgehalten, was es tut
chat-submit-feedback = Hat Rückmeldung in #{ $item } abgegeben
chat-triage = Hat Rückmeldungen gesichtet
chat-write-skill = Hat die Fähigkeit { $name } hinzugefügt
chat-correct-skill = Hat die Fähigkeit { $item } korrigiert
chat-keep-example = Hat ein Beispiel für { $item } festgehalten
chat-drop-example = Hat ein Beispiel aus { $item } entfernt
chat-forget-skill = Hat die Fähigkeit { $item } entfernt
chat-chat-categories = { $op ->
    [insert] Hat eine Kategorie hinzugefügt
    [remove] Hat eine Kategorie entfernt
   *[other] Hat die Kategorien neu geordnet
}
chat-chat-channels = { $op ->
    [insert] Hat einen Kanal hinzugefügt
    [remove] Hat einen Kanal entfernt
   *[other] Hat die Kanäle neu geordnet
}
chat-chat-feedback = { $op ->
    [insert] Hat eine Rückmeldung hinzugefügt
    [remove] Hat eine Rückmeldung entfernt
   *[other] Hat die Rückmeldungen geändert
}
chat-chat-read = Hat die Lesemarken geändert
chat-chat-drafts-drafts = { $op ->
    [splice] Hat in einen Entwurf getippt
    [mark] Hat einen Entwurf formatiert
    [replace] Hat einen Entwurf geleert
   *[other] Hat einen Entwurf bearbeitet
}
chat-message-pinned = { $new ->
    [true] Hat eine Nachricht angeheftet
   *[false] Hat eine Nachricht gelöst
}
chat-person-name = Hat den Namen in { $new } geändert
chat-person-initials = Hat die Initialen in { $new } geändert
chat-person-color = Hat die Farbe geändert
chat-person-status = Hat den Status auf { $new ->
    [online] online
    [idle] abwesend
    [dnd] nicht stören
    [offline] offline
   *[other] { $new }
} gesetzt

## Kanäle anlegen und ändern
chat-sheet-new-channel = Kanal anlegen
chat-sheet-new-category = Kategorie anlegen
chat-sheet-edit-channel = Kanal bearbeiten
chat-sheet-create-channel = Kanal anlegen
chat-sheet-create-category = Kategorie anlegen
chat-sheet-save = Speichern
chat-sheet-cancel = Abbrechen
chat-sheet-kind = Art des Kanals
chat-sheet-text = Text
chat-sheet-text-about = Nachrichten, Bilder, Dateien und Threads
chat-sheet-voice = Sprache
chat-sheet-voice-about = Sprechen, den Bildschirm teilen und sich sehen
chat-sheet-channel-name = Name
chat-sheet-category-name = Name der Kategorie
chat-sheet-text-placeholder = neuer-kanal
chat-sheet-voice-placeholder = Lounge
chat-sheet-category-placeholder = Projekte
chat-sheet-category = Kategorie
chat-sheet-no-category = Keine
chat-sheet-topic = Thema
chat-sheet-topic-placeholder = Wofür dieser Kanal da ist (optional)

# Was für ein Ding eine Nachrichtenzeile ist, für Hilfstechnik und die
# Rückmeldungsauswahl („Nachricht in Chat“).
chat-message-kind = Nachricht

## Die Aufzeichnung einer abgegebenen Rückmeldung
chat-feedback-replay = Aufzeichnung abspielen
chat-feedback-replay-failed = Die Aufzeichnung ging nicht auf
chat-feedback-replay-description = Die Aufzeichnung in { $app } öffnen und Schritt für Schritt durchgehen

## Die eigenen Bedienelemente und Bezeichnungen der Unterhaltung
chat-surface = Chat
chat-surface-detail = Kanäle in diesem Arbeitsbereich
chat-messages = Nachrichten
chat-earlier-arriving = Frühere Nachrichten sind unterwegs
chat-add-channel = Kanal hinzufügen
chat-create-menu = Kanal oder Kategorie anlegen
chat-composer = Nachrichtenfeld
chat-send-message = Nachricht senden
chat-undo = Rückgängig
chat-retry = Erneut versuchen
chat-failure-details = Einzelheiten
chat-close = Schließen
# Eine Nachricht, die den Arbeitsbereich nicht erreicht hat; $reason sagt, warum.
chat-not-sent = Nicht gesendet: { $reason }
chat-message-deleted = Nachricht gelöscht
chat-message-edited = Bearbeitet
# Neben der Uhrzeit einer angehefteten Nachricht.
chat-message-pinned-marker = Angeheftet
# Anstelle der Uhrzeit, solange die Unterhaltung noch auf dem Gerät ankommt.
chat-message-sending = Wird gesendet…
chat-empty-title = Noch keine Nachrichten
chat-empty-title-read-only = Hier ist noch nichts gesagt
chat-assistant-look-round = Bitte deinen Assistenten, jetzt im Arbeitsbereich nach neuer Arbeit zu schauen. Er kann dir auch deine geplanten Prüfungen zeigen, sie ein- oder ausschalten, ihren Zeitplan ändern oder ihr tägliches Ausgabenlimit senken.
chat-jump-latest-message = Zur neuesten Nachricht springen
chat-right-flap = Menschen oder Thread zeigen
chat-typing-one = { $name } schreibt …
chat-typing-two = { $first } und { $second } schreiben …
chat-typing-several = Mehrere Menschen schreiben …

## Angeheftete Nachrichten
chat-pins = Angeheftete Nachrichten
chat-pinned-message = Angeheftete Nachricht
chat-pins-empty = Noch nichts angeheftet
chat-pins-empty-detail = Hefte eine Nachricht über ihr Menü an, damit sie hier für alle bleibt.

## Threads
chat-thread-open = Nachrichten-Thread öffnen
chat-thread-composer = Antwortfeld des Threads
chat-thread-reply-placeholder = Im Thread antworten
chat-thread-last-reply = Letzte Antwort { $time }

## Eine Nachricht bearbeiten
chat-edit-field = Nachricht bearbeiten
chat-edit-save = Änderung an der Nachricht speichern
chat-edit-cancel = Änderung an der Nachricht verwerfen
chat-edit-save-button = Speichern
chat-edit-cancel-button = Abbrechen
chat-edit-history-show = Bearbeitungsverlauf anzeigen
chat-edit-history = Bearbeitungsverlauf
chat-edit-history-current = Aktuell, { $time }
chat-edit-history-earlier = Früher, { $time }

## Eine Datei teilen
chat-attach = Datei teilen
chat-attach-device = Von diesem Gerät hochladen
chat-attach-device-detail = Dateien, Bilder und Videos bis 32 MiB
chat-attach-workspace = Aus dem Arbeitsbereich
chat-attach-more = { $count } weitere im Arbeitsbereich
chat-attach-pick = { $name } anhängen
chat-attach-picker-title = Dateien teilen
chat-attach-picker-accept = Teilen

## Anrufe
chat-call-start-or-open = Anruf starten oder öffnen
chat-call-active = Laufender Anruf
# Ein Anruf, dessen Kanal keinen Namen zum Zeigen hat.
chat-call-untitled = Anruf
chat-call-open-window = Den Anruf in Chat öffnen
chat-call-expand = Vergrößern
chat-call-expand-restore = Anruf vergrößern oder zurückholen
chat-call-members-show = Teilnehmende des Anrufs zeigen
chat-call-camera-off-caption = Kamera aus
chat-call-camera-starting = Deine Kamera startet
chat-call-no-video = Kein Video
chat-call-audio-stream = Tonspur
chat-call-screen-mine = Du teilst deinen Bildschirm
chat-call-screen-mine-detail = Alle im Anruf sehen deinen Bildschirm
chat-call-screen-mine-chip = Dein Bildschirm
chat-call-screen-theirs = { $name } teilt den Bildschirm
chat-call-screen-theirs-chip = Der Bildschirm von { $name }
chat-call-screen-waiting = Wartet auf das erste Bild

## Das Menü eines Kanals, einer Kategorie oder einer Nachricht
chat-menu-rename = Umbenennen
chat-menu-rename-channel = Kanal umbenennen
chat-menu-rename-category = Kategorie umbenennen
chat-menu-edit-channel = Kanal bearbeiten
chat-menu-delete = Löschen
chat-menu-delete-channel = Kanal löschen
chat-menu-delete-category = Kategorie löschen
chat-menu-move-up = Nach oben
chat-menu-move-down = Nach unten
chat-menu-new-channel = Neuer Kanal
chat-menu-new-voice-channel = Neuer Sprachkanal
chat-menu-new-category = Neue Kategorie
chat-menu-collapse-category = Kategorie einklappen
chat-menu-expand-category = Kategorie ausklappen
chat-menu-archive-channel = Kanal archivieren
chat-menu-reply-in-thread = Im Thread antworten
chat-menu-edit-message = Nachricht bearbeiten
chat-menu-pin-message = Nachricht anheften
chat-menu-unpin-message = Nachricht lösen
chat-menu-delete-message = Nachricht löschen
chat-menu-copy-text = Text kopieren
chat-message-more = Mehr
chat-message-bar-delete-now = Nachricht jetzt löschen
chat-message-bar-remove-now = Nachricht jetzt entfernen
chat-menu-report-message = Nachricht melden
chat-moderation-record-unavailable = Die Moderation wird noch geladen. Versuche es gleich erneut.
chat-menu-report-sender = Absender melden
chat-menu-ban = Aus dem Arbeitsbereich ausschließen
chat-menu-volume = Lautstärke
chat-attach-share = Datei teilen

chat-send-empty = Schreib eine Nachricht oder häng etwas an.
chat-send-needs-recipient = Wähle, an wen die Nachricht geht.
chat-send-one-conversation = Ein Gespräch steht für sich: wähle es, oder die Personen, an die es geht.
chat-channel-composer-placeholder = Nachricht an #{ $channel }

## What the window shows
chat-writing = Schreibt…
chat-voice-channel-unnamed = Der Sprachkanal konnte nicht benannt werden

## Actions


## What the app is called in a window and in the launcher
chat-app-title = Chat
## What the resources are called, and the actions on them
chat-messages-post = Nachricht senden
chat-messages-post-file = Datei senden
chat-messages-share-resource = Live teilen
chat-messages-share-window = Fenster live teilen
chat-messages-stop-sharing = Teilen beenden
chat-messages-set-share-access = Ändern, was ein geteiltes Fenster erlaubt
chat-messages-share-open = Geteiltes Fenster als offen oder geschlossen markieren
chat-messages-deliver-message = Nachricht zustellen
chat-messages-import-message = Nachricht importieren
chat-messages-edit-message = Nachricht bearbeiten
chat-messages-delete-message = Nachricht löschen
chat-messages-remove-message = Nachricht entfernen
chat-messages-pin = Nachricht anheften
chat-messages-retry = Erneut nach einer Antwort fragen
chat-messages-ask = Die eigene Person fragen
chat-messages-answer-question = Eine Frage beantworten
chat-messages-withdraw-question = Eine Frage zurückziehen
chat-messages-settle-question = Sagen, was aus einer Antwort wurde
chat-messages-ask-agent = Einen Agenten fragen
chat-messages-stop-agent = Die Arbeit eines Agenten stoppen
chat-messages-post-view = Eine Ansicht senden
chat-messages-answer-with-view = Mit einer Ansicht antworten
chat-messages-rewrite-view = Eine Ansicht neu schreiben
# Was einen Zug gestartet hat, den niemand getippt hat: steht dort, wo sonst der Name steht.
chat-cause-reminder = Erinnerung: { $label }
chat-cause-event = Ereignis: { $label }
chat-cause-news = Neuigkeit: { $label }
chat-view-attached = Eine Ansicht ist angehängt
chat-view-attached-of = Eine Ansicht von { $apps } ist angehängt
chat-view-alone = Ansicht
chat-view-waiting = Dieser Beitrag ist noch nicht auf diesem Gerät
chat-view-gone = Dieser Beitrag hat keine Ansicht
chat-view-arriving = Ansicht wird geladen
chat-view-unavailable = Eine Ansicht kann hier nicht gezeigt werden
chat-view-unbound = Diese Ansicht gibt dich als Verfasser an. Tilo kann noch nicht sicher sagen, wer einen Beitrag geschrieben hat, deshalb wird sie hier nicht gezeigt
chat-view-refused = Du darfst { $apps } hier nicht lesen, die Ansicht zeigt nichts davon
chat-view-unread = { $apps } konnte hier nicht gelesen werden
chat-view-asks = { $who } hat diese Ansicht geschrieben. Sie möchte das in { $app } tun
chat-view-asks-given = { $name }: { $value }
chat-view-allow = Einmal erlauben
chat-view-dismiss = Verwerfen
# The question a message asks one person: what an action that asks first would
# do, its answers, and how it stands once it no longer waits.
chat-question-on = Bei „{ $resource }“
chat-question-field = { $label }: { $value }
chat-question-final = Das lässt sich nicht rückgängig machen.
chat-question-allow = Einmal erlauben
chat-question-always = Für dieses Ereignis immer erlauben
chat-question-deny = Nicht erlauben
chat-question-allowed = einmal erlaubt
chat-question-always-allowed = für dieses Ereignis immer erlaubt
chat-question-with-item = Aus deinem Tresor: { $item }
chat-question-always-item = Mit diesem Eintrag hier immer erlauben
chat-question-always-item-allowed = mit diesem Eintrag hier immer erlaubt
chat-question-denied = nicht erlaubt
chat-question-you-answered = Deine Antwort: { $answer }
chat-question-answered = Antwort von { $name }: { $answer }
chat-question-waiting = Wartet auf { $name }
chat-question-waiting-someone = Wartet auf eine Antwort
chat-question-someone-answered = Antwort: { $answer }
chat-question-started = Wird gerade erledigt
chat-question-done = Erledigt
chat-question-dropped = Es wurde nichts getan
chat-question-failed = Das ließ sich nicht erledigen
chat-question-withdrawn = Wird nicht mehr gefragt
chat-question-withdrawn-why = Wird nicht mehr gefragt: { $why }
chat-messages-record-work = Arbeit des Agenten festhalten
chat-messages-carry-counts = Zählstände der vorigen Teile übernehmen
chat-messages-stream-output = Antwort beim Eintreffen festhalten
# A message moved from another chat: its author there, and the tool.
chat-origin-author = { $name } in { $source }
# A question that asks for a secret, under the message that asks.
chat-secret-title-login = Passwort für { $place }
chat-secret-title-key = Schlüssel für { $place }
chat-secret-title-code-generator = Einrichtungsschlüssel für Anmeldecodes bei { $place }
chat-secret-title-ssh-key = SSH-Schlüssel für { $place }
chat-secret-title-value = Wert für { $place }
chat-secret-unreadable = Diese Anfrage nach einem Geheimnis sagt nicht, wofür es ist
chat-secret-waiting-mine = Es kommt in deinen Tresor. Wer gefragt hat, kann es dort verwenden und nicht lesen.
chat-secret-answer-in-window = Öffne diese Unterhaltung in Chat, um zu antworten
chat-secret-filled = Im Tresor aufbewahrt
chat-secret-declined = Nicht angegeben
chat-secret-field-login = Passwort
chat-secret-field-key = Schlüssel
chat-secret-field-code-generator = Einrichtungsschlüssel
chat-secret-field-ssh-key = Privater Schlüssel
chat-secret-field-value = Wert
chat-secret-save = Im Tresor speichern
chat-secret-decline = Ablehnen
chat-secret-saving = Wird in deinen Tresor gelegt…
chat-secret-not-kept = Der Tresor hat es nicht aufbewahrt. Versuch es noch einmal.
chat-secret-answer-saved = Es ist jetzt in meinem Tresor.
chat-secret-answer-declined = Das möchte ich lieber nicht angeben.

