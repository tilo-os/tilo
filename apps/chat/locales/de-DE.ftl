# Chat — Texte für Menschen.

# Marker beside a participant in the `@` picker that is an agent rather than a
# person, so somebody can tell at a glance that they are addressing software.
chat-mention-agent-tag = Agent

# Status line under the user's own name in the sidebar's identity bar.
chat-identity-available = Verfügbar
chat-identity-in-voice = Im Sprachkanal

# Chip under the workspace title that adds a channel category.
chat-sidebar-add-category = ＋ Kategorie

# Control buttons along the bottom of the expanded call overlay.
chat-call-mute = Stummschalten
chat-call-unmute = Stummschaltung aufheben
chat-call-deafen = Ton aus
chat-call-undeafen = Ton an
chat-call-audio-only = Nur Audio
chat-call-collapse = Einklappen
chat-call-leave = Verlassen

# Emoji picker: its title when inserting into the composer or reacting to a
# message, the search field's placeholder and the grid's section headings.
chat-emoji-title = Emoji
chat-emoji-add-reaction = Reaktion hinzufügen
chat-emoji-search = Emoji suchen
chat-emoji-recent = Zuletzt verwendet
chat-emoji-results = Ergebnisse
chat-emoji-no-results = Keine Emoji gefunden
chat-emoji-insert = Emoji einfügen
chat-emoji-group = Emoji-Gruppe
# A picker cell; the name is the emoji's name in the desktop's language.
chat-emoji-choose = { $name } auswählen
# A reaction chip under a message, which adds or removes the user's own.
chat-reaction-add = Reaktion { $emoji } hinzufügen
chat-reaction-remove = Reaktion { $emoji } entfernen

# Right-click / long-press menu on a channel, category, workspace or one of
# the user's own messages.
chat-menu-rename = Umbenennen
chat-menu-delete = Löschen
chat-menu-move-up = Nach oben verschieben
chat-menu-move-down = Nach unten verschieben
chat-menu-new-text-channel = Neuer Textkanal
chat-menu-new-voice-channel = Neuer Sprachkanal
chat-menu-new-category = Neue Kategorie
chat-menu-toggle-collapse = Einklappen / ausklappen
chat-menu-archive = Archivieren / wiederherstellen
chat-menu-edit-message = Nachricht bearbeiten
chat-menu-pin-message = Anheften / lösen
chat-menu-delete-message = Nachricht löschen

# Placeholders of the empty message composer and the thread's reply field.
chat-composer-placeholder = Nachricht an #{ $channel }
chat-composer-thread-placeholder = Antworten …

# How participants are named on screen: the user's own lines, a native audio
# stream (the tail of its id), someone the room has not named yet (a short tag
# that tells two of them apart) and a message author that is not known.
chat-identity-you = Ich
chat-identity-audio-stream = Audiostream { $tail }
chat-identity-someone = Unbekannt ({ $tag })
chat-identity-unknown-person = Unbekannte Person
# Title of the members panel: only people, or people and agents together.
chat-identity-people = Personen
chat-identity-collaborators = Mitwirkende

# Names a new workspace and its first categories and channels start with.
chat-default-workspace = Mein Arbeitsbereich
chat-default-text-category = Kanäle
chat-default-voice-category = Sprache
chat-default-text-channel = allgemein
chat-default-voice-channel = Lounge
# Names of a channel or category created from the sidebar or its menu, before
# the user renames it, and of the voice channel the header's call button
# creates when the workspace has none.
chat-default-new-category = Neue Kategorie
chat-default-new-text-channel = neuer-kanal
chat-default-new-voice-channel = Neuer Sprachkanal
chat-default-call-channel = Sprache
# An agent taking part without a name of its own, and a Ctrl+K result for a
# message that has neither text nor an attachment.
chat-default-agent = Agent
chat-default-no-text = (kein Text)

# Screen-reader names of the sidebar's destinations.
chat-a11y-inbox =
    { $unread ->
        [0] Posteingang
        [one] Posteingang, { $unread } ungelesen
       *[other] Posteingang, { $unread } ungelesen
    }
chat-a11y-current-context = Kanäle in { $workspace }, aktueller Arbeitsbereich
chat-a11y-current-workspace = Aktueller Arbeitsbereich
# A channel row; in the inbox it also names the workspace it comes from.
chat-a11y-channel =
    { $unread ->
        [0] { $name }
        [one] { $name }, { $unread } ungelesen
       *[other] { $name }, { $unread } ungelesen
    }
chat-a11y-channel-from =
    { $unread ->
        [0] { $name }, aus { $source }
        [one] { $name }, aus { $source }, { $unread } ungelesen
       *[other] { $name }, aus { $source }, { $unread } ungelesen
    }
chat-a11y-fallback-workspace = Tilo-Arbeitsbereich
# The caret beside a category heading.
chat-a11y-category-expand = { $name } ausklappen
chat-a11y-category-collapse = { $name } einklappen
chat-a11y-category-fallback = Kategorie
chat-a11y-add-channel = Kanal hinzufügen
chat-a11y-add-category = Kategorie hinzufügen
chat-a11y-rename-field = Feld zum Umbenennen

# Screen-reader names of the conversation's controls.
chat-a11y-edit-message = Nachricht bearbeiten
chat-a11y-composer = Nachrichteneingabe
chat-a11y-thread-composer = Antwort im Thread verfassen
chat-a11y-send = Nachricht senden
chat-a11y-jump-latest = Zur neuesten Nachricht springen
chat-a11y-attach = Datei aus dem Arbeitsbereich anhängen
chat-a11y-attach-file = { $file } anhängen
chat-a11y-start-call = Anruf starten oder öffnen
chat-a11y-show-navigation = Unterhaltungsnavigation anzeigen
chat-a11y-show-details = Personen oder Thread anzeigen
chat-a11y-open-thread = Thread zur Nachricht öffnen
chat-a11y-close-thread = Thread schließen
chat-a11y-edit-history = Bearbeitungsverlauf anzeigen
chat-a11y-edit-save = Bearbeitung speichern
chat-a11y-edit-cancel = Bearbeitung abbrechen
chat-a11y-messages = Nachrichten
chat-a11y-inbox-description = Posteingang: Unterhaltungen aus allen Tilo-Arbeitsbereichen
chat-a11y-channels-description = Kanäle im aktuellen Tilo-Arbeitsbereich

# Call controls and the call actions a permission check names; the active
# call's title when it is dragged out into its own floating window.
chat-a11y-mute-microphone = Mikrofon stummschalten
chat-a11y-share-screen = Bildschirm freigeben
chat-a11y-call-participants = Teilnehmende anzeigen
chat-a11y-call-expand = Anruf vergrößern oder wiederherstellen
chat-a11y-leave-call = Anruf verlassen
chat-a11y-join-call = Anruf beitreten
chat-a11y-use-microphone = Mikrofon verwenden
chat-a11y-use-camera = Kamera verwenden
chat-a11y-active-call = Laufender Anruf

# Status line in the attachment picker. The reason is one of the
# chat-error-* attachment reasons below.
chat-status-picker-exhausted = Dateiauswahl nicht verfügbar: Anfragezähler erschöpft
chat-status-picker-cancelled = Dateiauswahl abgebrochen
chat-status-picker-failed = Dateiauswahl fehlgeschlagen: { $reason }
chat-status-import-exhausted = Anhang nicht verfügbar: Anfragezähler erschöpft
chat-status-attachment-preparing = Anhang wird vorbereitet …
chat-status-import-stale = Anhang verworfen: veraltetes Ergebnis
chat-status-import-failed = Anhang fehlgeschlagen: { $reason }
chat-status-import-permission = Anhang abgebrochen: Berechtigung geändert
chat-status-attachment-shared = Anhang geteilt
# Why an attachment could not be read or shared.
chat-error-workspace-unavailable = Arbeitsbereich ist nicht verfügbar
chat-error-file-unavailable = Datei ist nicht verfügbar
chat-error-file-not-shareable = Datei kann nicht geteilt werden oder ist größer als 32 MiB
chat-error-file-unreadable = Datei konnte nicht gelesen werden
chat-error-file-empty = Datei ist leer
chat-error-file-too-large = Datei ist größer als 32 MiB
chat-error-content-store-refused = Der gemeinsame Inhaltsspeicher hat den Anhang abgelehnt
chat-error-verification-failed = Überprüfung der geteilten Daten fehlgeschlagen

# Notice in the call area when voice is unavailable or has stopped.
chat-status-audio-only = Dieser Anruf unterstützt nur Mikrofonaudio
chat-status-voice-unavailable = Sprache ist nicht verfügbar
chat-status-call-denied = Zugriff auf den Anruf wurde verweigert
chat-status-mic-permission-denied = Mikrofonberechtigung wurde verweigert
chat-status-mic-disconnected = Mikrofon getrennt
chat-status-mic-failed = Mikrofon konnte nicht gestartet werden
chat-status-call-disconnected = Anruf getrennt
chat-status-call-ended = Anruf beendet
chat-status-call-expired = Anrufsitzung abgelaufen
chat-error-media-unsupported = Anrufe und Bildschirmfreigabe sind erst verfügbar, wenn ein Medientransport verbunden ist
chat-error-voice-channel-writer = Nur wer im Raum schreiben darf, kann einen Sprachkanal erstellen

# Errors shown when a call control, a Ctrl+K action or a link into Chat
# cannot be carried out.
chat-error-call-invalid = Ungültiger Anrufbefehl
chat-error-call-busy = Anrufsteuerung ist ausgelastet
chat-error-call-exhausted = Anrufsteuerung: Befehlsfolge erschöpft
chat-error-voice-channel-gone = Sprachkanal existiert nicht mehr
chat-error-call-not-permitted = Anrufaktion ist nicht erlaubt
chat-error-call-not-active = Dies ist nicht der laufende Anruf
chat-error-call-needs-effect = Anrufsteuerung erfordert einen gültigen lokalen Effekt
chat-error-navigation-needs-view = Navigation in Chat erfordert eine Ansichtsoperation
chat-error-workspace-gone = Arbeitsbereich existiert nicht mehr
chat-error-category-gone = Kategorie existiert nicht mehr
chat-error-conversation-gone = Unterhaltung existiert nicht mehr
chat-error-message-gone = Nachricht existiert nicht mehr
chat-error-person-gone = Person existiert nicht mehr
chat-error-attachment-unsupported = Anhänge nicht unterstützt: kein Asset-Transport des Hosts
chat-error-attachment-needs-effect = Anhang erfordert eine Effekt-Operation
chat-error-attachment-path-empty = Pfad des Anhangs ist leer
chat-error-attachment-not-accepted = Anhang wurde nicht angenommen
# Opening a Chat record that is not a Chat workspace.
chat-error-no-workspace-schema = Chat-Eintrag hat kein Arbeitsbereichsschema

# Presence state under a member in the People roster.
chat-presence-online = Online
chat-presence-idle = Abwesend
chat-presence-dnd = Nicht stören
chat-presence-offline = Offline
# Roster status of a member who is in a voice channel; the channel is its name.
chat-presence-in-voice-channel = Im Sprachkanal · { $channel }
# Pill beside the workspace title counting who is online. The collaborator
# form is used when the workspace also has agents in it.
chat-presence-people-online =
    { $count ->
        [one] { $count } Person online
       *[other] { $count } Personen online
    }
chat-presence-collaborators-online =
    { $count ->
        [one] { $count } Mitwirkender online
       *[other] { $count } Mitwirkende online
    }

# Relative day on the date divider above the first message.
chat-time-today = Heute
chat-time-yesterday = Gestern
chat-time-days-ago =
    { $count ->
        [one] Vor { $count } Tag
       *[other] Vor { $count } Tagen
    }

# Header of the cross-workspace Inbox in the sidebar.
chat-view-inbox-title = Posteingang
chat-view-inbox-subtitle = Unterhaltungen aus allen Tilo-Arbeitsbereichen
# Conversation header topic while the Inbox is open: which workspace the
# channel comes from, and the name used when that workspace is unknown.
chat-view-inbox-source = Aus { $workspace }
chat-view-inbox-source-fallback = Tilo-Arbeitsbereich
# Sidebar header above a workspace's channel list.
chat-view-workspace-channels = Kanäle in { $workspace }
# Floating button that scrolls the conversation back to the newest message.
chat-view-jump-latest = Zur neuesten Nachricht
# Empty channel: the channel is its "#name". The first line is shown to someone
# who may post, the second to someone who may only read.
chat-view-channel-start = Hier beginnt { $channel }. Die erste Nachricht schreiben, um loszulegen.
chat-view-channel-empty = In { $channel } gibt es noch keine Nachrichten.
# Link under an edited message that opens its edit history.
chat-view-edited-history = (bearbeitet) · Verlauf anzeigen
# Inline editor for a sent message: placeholder, keyboard hint and buttons.
chat-view-edit-placeholder = Nachricht bearbeiten …
chat-view-edit-hint = Eingabetaste zum Speichern · Esc zum Abbrechen
chat-view-edit-save = Speichern
chat-view-edit-cancel = Abbrechen
# Reply count on a message's thread bar and in the thread panel's divider.
chat-view-replies =
    { $count ->
        [one] { $count } Antwort
       *[other] { $count } Antworten
    }
# Right side of a message's thread bar; the time is a clock time.
chat-view-last-reply = letzte Antwort { $time }
# Banner in place of the composer for someone with read-only access.
chat-view-read-only-banner = Nur Lesezugriff auf diesen Arbeitsbereich
# Thread panel title, and the button that returns from it on a narrow window.
chat-view-thread-title = Thread
chat-view-back = Zurück
# Headcount under the People panel title, followed by the workspace name. The
# collaborator form is used when the workspace also has agents in it.
chat-view-roster-people =
    { $count ->
        [one] { $count } Person
       *[other] { $count } Personen
    }
chat-view-roster-collaborators =
    { $count ->
        [one] { $count } Mitwirkender
       *[other] { $count } Mitwirkende
    }
# A member's name in the People panel when it is the user or an agent.
chat-view-member-you = { $name } (Sie)
chat-view-member-agent = { $name } (Agent)

# Status of an image attachment while its picture is not shown yet.
chat-attachment-image-loading = Bild wird geladen …
chat-attachment-image-pending = Geteiltes Bild wird geladen …
chat-attachment-image-unavailable = Geteiltes Bild nicht verfügbar
chat-attachment-image-decoding = Bild wird dekodiert …
# Video card detail when the video's dimensions are unknown; size is a byte size.
chat-attachment-video-meta = Video · { $size }
# Attachment placeholder for someone not allowed to view the file.
chat-attachment-access-denied = Zugriff nicht gewährt
# Workspace file picker above the composer: title, loading state, empty state
# (the folder is a filesystem path) and the footer for entries not listed.
chat-attachment-picker-title = Datei teilen
chat-attachment-loading = Anhänge werden geladen …
chat-attachment-picker-empty = Keine Anhänge in { $folder }
chat-attachment-picker-more = +{ $count } weitere im Arbeitsbereich

# Conversation header button that starts a call, or shows one is running.
chat-call-start = Anrufen
chat-call-in-call = Im Anruf
# Call title when its channel cannot be found.
chat-call-title-fallback = Anruf
# Name plate on the user's own call tile, and on it as the call's host.
chat-call-you = Sie
chat-call-you-host = Sie (Gastgeber)
# Caption on a call tile with no picture: camera switched off, or no video
# reaching this device.
chat-call-camera-off = Kamera aus
chat-call-no-video = Kein Video
# Caption on a tile that carries only a verified audio stream.
chat-call-audio-stream = Audiostream
# Floating call panel: stream count and the connection state beside it.
chat-call-audio-streams =
    { $count ->
        [one] { $count } Audiostream
       *[other] { $count } Audiostreams
    }
chat-call-state-connected = Verbunden
chat-call-state-reconnecting = Verbindung wird wiederhergestellt
chat-call-state-joining = Beitreten …
chat-call-state-unavailable = Nicht verfügbar
# Floating call panel button that opens the expanded call view.
chat-call-expand = Erweitern
# Expanded call subtitle: how many people are in the call.
chat-call-headcount = { $count } im Anruf
# Who is sharing a screen, in the call subtitle and on the shared-screen tile.
chat-call-sharing-self = Ihr Bildschirm wird geteilt
chat-call-sharing-other = { $name } teilt einen Bildschirm
# Detail under the shared-screen heading: for the sharer, and for viewers while
# the first frame is on its way.
chat-call-sharing-self-detail = Alle im Anruf sehen Ihren Bildschirm
chat-call-sharing-connecting = Verbindung zum geteilten Bildschirm wird hergestellt …
# Pill on the featured shared-screen tile.
chat-call-live-own-screen = Live · Ihr Bildschirm
chat-call-live-screen = Live · Bildschirm von { $name }
# Edit history popover: its title and each version's heading (time is a clock time).
chat-call-history-title = Bearbeitungsverlauf
chat-call-history-now = Jetzt · { $time }
chat-call-history-edited = Bearbeitet · { $time }

# What a change did, as the room's history lists it (also the Ctrl+K titles of
# the matching message, channel and category actions).
chat-action-send-message = Nachricht senden
chat-action-edit-message = Nachricht bearbeiten
chat-action-delete-message = Nachricht löschen
chat-action-remove-reaction = Reaktion entfernen
chat-action-pin-message = Nachricht anheften
chat-action-unpin-message = Nachricht lösen
chat-action-update-agent-work = Agentenarbeit aktualisieren
chat-action-create-channel = Kanal erstellen
chat-action-create-category = Kategorie erstellen
chat-action-rename-category = Kategorie umbenennen
chat-action-delete-category = Kategorie löschen
chat-action-collapse-category = Kategorie einklappen
chat-action-expand-category = Kategorie ausklappen
chat-action-move-channel-up = Kanal nach oben verschieben
chat-action-move-channel-down = Kanal nach unten verschieben
chat-action-rename-channel = Kanal umbenennen
chat-action-delete-channel = Kanal löschen
chat-action-archive-channel = Kanal archivieren
chat-action-unarchive-channel = Kanal wiederherstellen
chat-action-mark-channel-read = Kanal als gelesen markieren
chat-action-join-call = Anruf beitreten
chat-action-leave-call = Anruf verlassen
chat-action-update-call-state = Anrufstatus aktualisieren
chat-action-start-presenting = Präsentation starten
chat-action-stop-presenting = Präsentation beenden
chat-action-update-call-connection = Anrufverbindung aktualisieren
# The name a history entry gives the message a change touched.
chat-action-message = Nachricht

# Name recorded beside a message from an agent that has no display name.
chat-action-agent-default-name = Agent

# Ctrl+K and search labels for a channel or workspace whose name is blank.
chat-action-unnamed-channel = Kanal
chat-action-workspace = Arbeitsbereich

# Ctrl+K actions that open or reveal something without changing it.
chat-action-open-person = Person öffnen
chat-action-open-workspace = Arbeitsbereich öffnen
chat-action-open-category = Kategorie öffnen
chat-action-open-conversation = Unterhaltung öffnen
chat-action-open-call-conversation = Unterhaltung zum Anruf öffnen
chat-action-reveal-message = Nachricht anzeigen
chat-action-reveal-shared-message = Geteilte Nachricht anzeigen

# Ctrl+K actions on a workspace that create a channel or a category, the
# prompt's field label and placeholder, and the name used when it is left blank.
chat-action-new-channel = Neuer Kanal
chat-action-new-category = Neue Kategorie
chat-action-param-name = Name
chat-action-channel-name-placeholder = Kanalname …
chat-action-category-name-placeholder = Kategoriename …
chat-action-new-channel-default-name = neuer-kanal
chat-action-new-category-default-name = Neue Kategorie

# Ctrl+K actions on a text channel: posting (with its field label and
# placeholder), attaching a file by path, and marking it read.
chat-action-post-message = Nachricht senden
chat-action-param-message = Nachricht
chat-action-post-message-placeholder = Nachricht eingeben … (@Name erwähnt ein Mitglied)
chat-action-attach-file = Datei anhängen
chat-action-param-file-path = Dateipfad
chat-action-mark-as-read = Als gelesen markieren

# Ctrl+K entry for a voice channel's call; the channel is its `🔊 name` label.
chat-action-call-label = Anruf: { $channel }
# Ctrl+K actions on a call.
chat-action-join-muted = Stummgeschaltet beitreten
chat-action-mute-microphone = Mikrofon stummschalten
chat-action-request-microphone = Mikrofon anfordern
chat-action-call-status = Anrufstatus
chat-action-select-microphone = Mikrofon auswählen
chat-action-param-input = Eingang
chat-action-select-microphone-placeholder = Eingang aus dem Anrufstatus

# Ctrl+K actions on a message and its reactions.
chat-action-edit-message-placeholder = Neuer Text …
chat-action-remove-my-reaction = Eigene Reaktion entfernen
chat-action-add-my-reaction = Eigene Reaktion hinzufügen
# Ctrl+K and search title of a message that has only an attachment.
chat-action-attachment = Anhang

# Group headings in global search.
chat-search-kind-conversations = Unterhaltungen
chat-search-kind-messages = Nachrichten
chat-search-kind-people = Personen
chat-search-kind-workspaces = Arbeitsbereiche
chat-search-kind-categories = Kategorien
# Search subtitle of a category, after its workspace's name.
chat-search-category-collapsed = eingeklappt
chat-search-category-expanded = ausgeklappt
# Search subtitle of a participant.
chat-search-subtitle-agent = Agent
chat-search-subtitle-person = Person

# Ctrl+K action that reads a conversation's messages page by page, its
# placeholder and parameter descriptions (pageSize and nextCursor are field
# names, not words), and why it is currently unavailable.
chat-read-messages = Nachrichten der Unterhaltung lesen
chat-read-messages-placeholder = pageSize und optionaler Cursor
chat-read-param-page-size = Maximale Anzahl Einträge auf dieser Seite
chat-read-param-cursor = Undurchsichtiger nextCursor der vorherigen Seite
chat-read-unavailable = Ein aktueller Lesezugriff auf das festgeschriebene Dokument ist nicht verfügbar
