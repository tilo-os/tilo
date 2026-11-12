themes-yours = Deine Designs
themes-none = Noch keine Designs
themes-new = Neues Design
themes-untitled = Unbenanntes Design
themes-presets = Vorlagen
themes-use = Verwenden
themes-in-use = In Verwendung
themes-customize = Anpassen
themes-missing = Dieses Design wurde gelöscht.
themes-apply = Übernehmen
themes-export = Als Datei kopieren
themes-duplicate = Duplizieren
themes-delete-button = Design löschen
themes-name = Name
themes-name-label = Name
themes-palette = Farbpalette
themes-polarity = Erscheinungsbild
themes-hue = Farbton
themes-chroma = Sättigung
themes-chroma-help = Bei 0 sind die Neutraltöne reines Grau, und kein Arbeitsbereich kann sie tönen.
themes-ladder = Helligkeit
themes-rung-base = Schreibtisch
themes-rung-sidebar = Seitenleiste
themes-rung-content = Fenster
themes-rung-inspector = Inspektor
themes-rung-raised = Felder
themes-rung-overlay = Menüs und Panels
themes-rung-primary = Haupttext
themes-rung-secondary = Sekundärtext
themes-rung-tertiary = Tertiärtext
themes-emphasis = Betonung
themes-accent = Akzentfarbe
themes-emphasis-kind = Aktionen und Auswahl
themes-tint = Tönung
themes-presence = Anwesenheit
themes-presence-lightness-label = Helligkeit
themes-presence-chroma-label = Farbigkeit
themes-shape-density = Form und Dichte
themes-radius = Fensterecken
themes-density = Abstände
themes-type = Schrift
themes-face = Schriftart
themes-face-help = Standard, Hyperlegible oder der Name einer installierten Schriftfamilie.
themes-size = Textgröße
themes-distinct-letters = Ähnliche Buchstaben unterscheiden
themes-distinct-letters-help = Setzt I, l, 1 und 0, O in allen Texten auseinander. Namen, Codes und Links haben sie immer; Hyperlegible bringt sie mit.
themes-motion-material = Bewegung und Material
themes-motion = Bewegung
themes-blur = Unschärfe hinter Menüs
themes-tint-opacity = Deckkraft der Menüs
themes-tab-strip = Tableiste der Fenster
themes-tab-strip-help = Gefrostet zeichnet den Schreibtisch hinter den Tabs eines Fensters unscharf, wie es Menüs tun. Hoher Kontrast und reduzierte Transparenz machen sie immer deckend.
themes-rim-span = Licht am aktiven Tab
themes-rim-alpha = Stärke des Lichts am aktiven Tab
themes-hover-alpha = Stärke beim Überfahren eines Tabs

themes-reset-button = Zurücksetzen

## Verlauf

themes-theme-name = { $item } umbenannt
themes-theme-high-contrast = { $new ->
    [true] { $item } auf hohen Kontrast gestellt
   *[false] { $item } auf normalen Kontrast gestellt
}
themes-theme-hue = Den Farbton der Neutraltöne von { $item } auf { $new }° gesetzt
themes-theme-chroma = Die Sättigung der Neutraltöne von { $item } auf { $new } gesetzt
themes-surfaces-base = Die Helligkeit des Schreibtischs von { $item } auf { $new } gesetzt
themes-surfaces-content = Die Helligkeit der Fenster von { $item } auf { $new } gesetzt
themes-surfaces-sidebar = Die Helligkeit der Seitenleiste von { $item } auf { $new } gesetzt
themes-surfaces-inspector = Die Helligkeit des Inspektors von { $item } auf { $new } gesetzt
themes-surfaces-raised = Die Helligkeit der Felder von { $item } auf { $new } gesetzt
themes-surfaces-overlay = Die Helligkeit der schwebenden Flächen von { $item } auf { $new } gesetzt
themes-ink-primary = Den Haupttext von { $item } auf { $new } gesetzt
themes-ink-secondary = Den Sekundärtext von { $item } auf { $new } gesetzt
themes-ink-tertiary = Den Tertiärtext von { $item } auf { $new } gesetzt
themes-theme-tint = { $item } mit dem Farbton { $new }° getönt
themes-presence-lightness = Die Helligkeit der Anwesenheit von { $item } auf { $new } gesetzt
themes-presence-chroma = Die Farbsättigung der Anwesenheit von { $item } auf { $new } gesetzt
themes-theme-window-radius = Den Eckradius der Fenster von { $item } auf { $new } px gesetzt
themes-theme-density = Die Dichte von { $item } auf { $new } gesetzt
themes-theme-face = Die Schriftart von { $item } geändert
themes-theme-base-size = Die Grundschriftgröße von { $item } auf { $new } px gesetzt
themes-theme-distinct-letters = { $new ->
    [true] { $item } unterscheidet ähnliche Buchstaben immer
   *[other] { $item } überlässt ähnliche Buchstaben der Schriftart
}
themes-theme-motion = Die Bewegung von { $item } auf { $new } gesetzt
themes-theme-blur = Die Unschärfe von { $item } auf { $new } px gesetzt
themes-theme-tint-opacity = Die Deckkraft der Tönung von { $item } auf { $new } gesetzt
themes-theme-frosted-strip = { $new ->
    [true] Die Tableiste von { $item } gefrostet
   *[other] Die Tableiste von { $item } deckend gemacht
}
themes-theme-rim-span = Die Reichweite des Lichts am aktiven Tab von { $item } auf { $new } gesetzt
themes-theme-rim-alpha = Das Licht am aktiven Tab von { $item } auf { $new } gesetzt
themes-theme-hover-alpha = Die Stärke beim Überfahren eines Tabs von { $item } auf { $new } gesetzt
themes-new-theme = Ein Design aus { $base } begonnen
themes-reset = { $item } auf die Vorlage Graphit zurückgesetzt
themes-delete = { $item } gelöscht
themes-switch-polarity = { $dark ->
    [true] { $item } dunkel gemacht
   *[false] { $item } hell gemacht
}

themes-import = Design importieren
themes-import-title = Ein Design importieren
themes-import-files = Designdateien
themes-imported = { $name } importiert
themes-import-failed = { $name } konnte nicht importiert werden: { $reason }
themes-reason-not-a-document = es ist kein Theme-Dokument
themes-reason-not-a-document-detail = es ist kein Theme-Dokument: { $reason }
themes-preview = Vorschau
themes-preview-desktop = Schreibtisch
themes-preview-list = Liste
themes-preview-dialog = Dialog
themes-preview-presence = Anwesenheit

## Fakten, Eigenschaftsnamen und Bereichstitel
themes-contrast-low = Zu wenig Kontrast: { $findings }

## What the window shows
themes-contrast-every-pairing-meets = Jede Paarung erreicht ihre Kontrastgrenze.

themes-preview-label-desktop = Vorschau von { $theme }
themes-preview-label-list = Listenvorschau von { $theme }
themes-preview-label-dialog = Dialogvorschau von { $theme }
themes-preview-label-presence = Anwesenheitsfarben von { $theme }

## Actions

themes-effect-apply-theme = Als Systemdesign festlegen
themes-effect-export = Design exportieren
themes-effect-duplicate-theme = Design duplizieren
themes-studio = Designs
themes-studio-import-theme = Designdatei importieren
themes-studio-reset = Auf Voreinstellung zurücksetzen
themes-studio-switch-polarity = Zwischen Hell und Dunkel wechseln

## The preview's own sample window
themes-app-title = Theme Studio
themes-preview-inbox = Posteingang
themes-preview-move-title = In den Papierkorb legen?
themes-preview-move-body = Die Datei bleibt 30 Tage wiederherstellbar.
themes-preview-keep-copy = Kopie behalten
themes-preview-cancel = Abbrechen
themes-preview-move = Verschieben
themes-preview-document = Quartalsbericht
themes-preview-shared = Geteilt mit Ada und Bo
themes-preview-edited = Vor 2 Minuten bearbeitet
themes-preview-field = Budgetnotizen
themes-preview-share = Teilen
