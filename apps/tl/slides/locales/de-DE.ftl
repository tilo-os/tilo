# Folien auf Deutsch. Keys sind die stabilen ids der Deklarationen (app.ids); siehe locales/en-US.ftl.

d-so-tilo-slides = Folien

d-so-tilo-slides-keywords = slides, slide, presentation, presentations, deck, decks, powerpoint, pptx, odp, impress, keynote, präsentation, präsentationen, folien, vortrag, diashow

d-be43leya = Präsentation
    .doc = A deck: its slides in the order people see them, and the palette and look its surfaces take.
    .noun = { $count ->
        [one] deck
       *[other] decks
    }
    .untitled = Untitled deck

d-pwdtrcuk = Titel
    .doc = The deck's name: its window's title and an exported file's name.

d-ep3yewja = Palette

d-3xehcyrf = Erscheinungsbild

d-nhumnzoj = Folien

d-weqf5aou = Präsentiert seit
    .doc = When the deck's presentation started, for the speaker's clock; nothing while it is not being presented.

d-u6doynxu = Uhr angehalten bei
    .doc = How long the presentation had run when the speaker paused it, in seconds.

d-xwjxu7wj = Uhr pausiert
    .doc = The speaker paused the clock.

d-sy2pbelg = Hinweis
    .doc = Warum die zuletzt geöffnete Datei nicht geöffnet werden konnte; leer, sobald sie sich öffnete oder die Meldung geschlossen wurde.

d-dirubba7 = Slide
    .doc = One slide: its layout, what it stands on, its speaker notes, and the elements drawn on it, back to front.
    .noun = { $count ->
        [one] slide
       *[other] slides
    }
    .untitled = Slide

d-w3sjwjk2 = Layout

d-x3bud3c2 = Hintergrund

d-qowtxual = Notes
    .doc = What the speaker says over this slide.

d-lbme2z4w = Elements

d-dirubba7-filter-layout = Layout

d-dirubba7-filter-background = Hintergrund

d-afdw7uwv = Element
    .doc = A text box, shape, line or picture on a slide.
    .noun = { $count ->
        [one] element
       *[other] elements
    }
    .untitled = Text

d-74of62le = Kind

d-b47bgsdf = X
    .doc = Left, in slide units (the slide is 1280 × 720).

d-bh6ok4v5 = Y
    .doc = Top.

d-53kv3kr2 = W
    .doc = Width.

d-b65jrhk6 = H
    .doc = Height.

d-64oupu64 = Body
    .doc = A text box's words: what is typed, with its bold, italic and lists.

d-qysmcg6z = Src
    .doc = A picture's content address (`tilo-blob://…`).

d-3u7ptmma = Fill
    .doc = A text box's ink, or a shape's fill: a palette role (`accent`, `surface`, `deep`, `text-dim`, …), `none`, or `#RRGGBB`.

d-dsypcewy = Stroke
    .doc = Outline colour, by the same roles.

d-lsbzlmm7 = Stroke width

d-i6lk64ca = Font size

d-kk6sectq = Align

d-4g7ap5xd = Bold
    .doc = Bold over all its words, for an element that has none of its own.

d-haaxnqxs = Italic

d-pii7xmb6 = Radius
    .doc = Corner radius.

d-afdw7uwv-filter-kind = Kind

d-afdw7uwv-filter-x = X

d-afdw7uwv-filter-y = Y

d-afdw7uwv-filter-w = W

d-afdw7uwv-filter-h = H

d-afdw7uwv-filter-stroke_width = Stroke width

d-afdw7uwv-filter-font_size = Font size

d-afdw7uwv-filter-align = Align

d-afdw7uwv-filter-bold = Bold

d-afdw7uwv-filter-italic = Italic

d-afdw7uwv-filter-radius = Radius

d-azuusozp = Deck file
    .doc = A deck as a file holds it, and as the engine writes one.
    .noun = { $count ->
        [one] deck file
       *[other] deck files
    }

d-mnmdemqn = Titel

d-iayxmtjm = Palette

d-qzspviev = Erscheinungsbild

d-ajyrzs4l = Folien

d-g4vlsayp = Slide file
    .doc = A slide as a file holds it.
    .noun = { $count ->
        [one] slide file
       *[other] slide files
    }

d-dqtkr5ot = Id

d-sqsminna = Layout

d-tmaage4k = Hintergrund

d-e73d2qv7 = Notes

d-ggcmbhfn = Elements

d-jdp4at7x = Element file
    .doc = An element as a file holds it: its words as Markdown.
    .noun = { $count ->
        [one] element file
       *[other] element files
    }

d-kpm4jhpo = Id

d-n34lg7tm = Kind

d-hevssfo2 = X

d-u55ztccf = Y

d-qqtdorgx = W

d-sdpmue2g = H

d-yd6wa63n = Text

d-3jfhhhfc = Src

d-a45as5qt = Fill

d-lqeh5xoc = Stroke

d-rrx5fbi7 = Stroke width

d-cbc3umvz = Font size

d-vriuesyu = Align

d-xv57dbhp = Bold

d-zu3zrkv4 = Italic

d-g4nvvgmj = Radius

d-jdp4at7x-filter-x = X

d-jdp4at7x-filter-y = Y

d-jdp4at7x-filter-w = W

d-jdp4at7x-filter-h = H

d-jdp4at7x-filter-stroke_width = Stroke width

d-jdp4at7x-filter-font_size = Font size

d-jdp4at7x-filter-bold = Bold

d-jdp4at7x-filter-italic = Italic

d-jdp4at7x-filter-radius = Radius

d-vha2nwor = Outline
    .doc = A slide written from an outline: what an agent or a person adds in one step.
    .noun = { $count ->
        [one] outline
       *[other] outlines
    }

d-fmwlgt6m = Titel

d-3iepzseq = Body

d-tarratvg = Layout

d-uv2g5qve = Notes

d-vha2nwor-filter-layout = Layout

d-rfvtuoet = Colours
    .doc = The colours of a deck, resolved for its palette and look: what its slides are painted in.
    .noun = { $count ->
        [one] colour
       *[other] colours
    }

d-i6vzkphp = Akzent

d-qp7qmrqr = Accent soft

d-e2yv6zxc = Accent deep

d-wvovch6g = Fläche

d-3pyvsswg = Surface dim

d-mmxtpccn = Tiefe

d-ch6doguq = Text

d-fgu23gq2 = Text dim

d-ojgvaj3b = On accent

d-23vs3mco = Success

d-4ph7cxqz = Warning

d-g56ubagd = Danger

d-ne7cvyra = Info

d-eisz2her = Layout
    .doc = How a slide arranges what it holds: the shape its content boxes start in.

d-yihhbat4 = Titel

d-phboiuxr = Title content

d-lnbt2m5y = Two column

d-lgfbtptr = Section

d-7undqhje = Blank

d-byvmtrgh = Hintergrund
    .doc = What a slide stands on: the deck's surface, its accent, or its deep tone.

d-lsvs62ig = Fläche

d-ayoeze2w = Akzent

d-pqjo7cxx = Tiefe

d-s6vcspn4 = Palette
    .doc = Which hue drives the deck's accent.

d-gojwarcz = Indigo

d-x2kluj3j = Smaragd

d-pukqhwqj = Bernstein

d-rcppgpt2 = Rosé

d-pzwny465 = Himmelblau

d-efmfecqo = Erscheinungsbild
    .doc = A deck's own light or dark surfaces, so it looks the same on every screen and in a saved file.

d-t4usmyop = Hell

d-ro4jz5is = Dunkel

d-hmsrpbxa = Element kind
    .doc = What an element is: words, a shape, a line or a picture.

d-3g2xm7q5 = Text

d-rtxch3d3 = Rect

d-n4zmfwop = Ellipse

d-h75q4gxr = Linie

d-rlfh6vrg = Image

d-m4lqka7e = Align
    .doc = How a text box sets its words.

d-rddirg44 = Links

d-na53z3ss = Mitte

d-l4a27265 = Rechts

d-feb6wpdp = Blocksatz

d-57jfhctc = Start
    .doc = A deck starts with one title slide holding an empty title and subtitle, so a live launch opens on nothing that pretends to be content.

d-2g5khvi5 = Folie hinzufügen
    .doc = Adds a slide after `after`, or at the end, from the template its layout names.
    .did = hat eine Folie hinzugefügt

d-2g5khvi5-title = Titel

d-2g5khvi5-body = Body

d-2g5khvi5-layout = Layout

d-2g5khvi5-after = After

d-2la4kpcr = Add authored slide
    .doc = Adds a slide from the template its layout names, and answers it.

d-2la4kpcr-title = Titel

d-2la4kpcr-body = Body

d-2la4kpcr-layout = Layout

d-2la4kpcr-notes = Notes

d-2la4kpcr-fallback = Fallback

d-2la4kpcr-after = After

d-j7beflcx = Folien hinzufügen
    .doc = Adds several slides in one step, in the order given: a deck written from an outline, as the JSON a person or an agent hands over.
    .did = hat Folien aus einer Gliederung hinzugefügt

d-j7beflcx-outline = Outline

d-j7beflcx-after = After

d-ym5e44gl = Folie duplizieren
    .doc = Copies a slide and everything on it, right after it.
    .did = hat eine Folie dupliziert

d-ym5e44gl-s = S

d-j3ft43xp = Folie duplizieren
    .doc = Kopiert die Folie, die das Fenster zeigt, direkt danach.
    .did = hat eine Folie dupliziert

d-j3ft43xp-showing = Angezeigt

d-yovatixr = Objekt duplizieren
    .doc = Kopiert ein Objekt an seinem Platz auf der Folie, etwas weiter unten und seitlich versetzt.
    .did = hat ein Objekt dupliziert

d-yovatixr-e = E

d-5bpcbcc5 = Folie löschen
    .doc = Deletes a slide.
    .w-a-deck-keeps-at-least-one = A deck keeps at least one slide
    .did = hat eine Folie gelöscht

d-5bpcbcc5-s = S

d-rpjtkcs3 = Folie verschieben
    .doc = Moves a slide to `at`, the place among the slides a drag reports.
    .did = hat eine Folie verschoben

d-rpjtkcs3-s = S

d-rpjtkcs3-at = At

d-urjtfzun = Text der Folie setzen
    .doc = Writes a slide's title and body: the first two text boxes, as the templates make them.
    .w-give-a-title-a-body-or = Give a title, a body or both
    .did = hat den Text der Folie gesetzt

d-urjtfzun-s = S

d-urjtfzun-title = Titel

d-urjtfzun-body = Body

d-lb5rvlcb = Element hinzufügen
    .doc = Adds an element to a slide: what it is, where it stands, and any style it takes.
    .w-only-a-picture-takes-a-source = Only a picture takes a source
    .w-only-text-takes-words = Only text takes words
    .w-a-picture-needs-its-source = A picture needs its source
    .did = hat ein Objekt hinzugefügt

d-lb5rvlcb-kind = Kind

d-lb5rvlcb-x = X

d-lb5rvlcb-y = Y

d-lb5rvlcb-w = W

d-lb5rvlcb-h = H

d-lb5rvlcb-text = Text

d-lb5rvlcb-src = Src

d-lb5rvlcb-fill = Fill

d-lb5rvlcb-text_color = Text color

d-lb5rvlcb-stroke = Stroke

d-lb5rvlcb-stroke_width = Stroke width

d-lb5rvlcb-font_size = Font size

d-lb5rvlcb-align = Align

d-lb5rvlcb-bold = Bold

d-lb5rvlcb-italic = Italic

d-lb5rvlcb-radius = Radius

d-5o6heqjy = Einfügen
    .doc = Inserts an element given whole, as the JSON another deck copied: a paste from anywhere.
    .did = hat ein Objekt eingefügt

d-5o6heqjy-element = Element

d-uzu5nfqf = Löschen
    .doc = Deletes an element.
    .did = hat ein Objekt gelöscht

d-uzu5nfqf-e = E

d-keiyxwq5 = Nach vorn
    .doc = Brings an element to the front.
    .did = hat ein Objekt nach vorn geholt

d-keiyxwq5-e = E

d-g3ev3t4g = Nach hinten
    .doc = Sends an element to the back.
    .did = hat ein Objekt nach hinten geschickt

d-g3ev3t4g-e = E

d-5ezqovdl = Verschieben
    .doc = Moves an element to a place on the slide, as a drag or the arrow keys place it.
    .did = hat ein Objekt verschoben

d-5ezqovdl-e = E

d-5ezqovdl-x = X

d-5ezqovdl-y = Y

d-7kfizqqa = Verschieben
    .doc = Moves the element `target` names to `at`, as a drag on the stage reports it.
    .did = hat ein Objekt verschoben

d-7kfizqqa-target = Target

d-7kfizqqa-at = At

d-cbw5ooxc = Verschieben
    .doc = Nudges the element `target` names by `dx` and `dy` slide units.
    .did = hat ein Objekt verschoben

d-cbw5ooxc-target = Target

d-cbw5ooxc-dx = Dx

d-cbw5ooxc-dy = Dy

d-dqkpey7x = Größe ändern
    .doc = Sizes an element, as dragging its edge does.
    .did = hat ein Objekt in der Größe geändert

d-dqkpey7x-e = E

d-dqkpey7x-at = At

d-m6cvsw3h = Größe ändern
    .doc = Sizes the element `target` names, as a drag on its edge reports it.
    .did = hat ein Objekt in der Größe geändert

d-m6cvsw3h-target = Target

d-m6cvsw3h-at = At

d-egsj6wvu = Gestaltung
    .doc = Sets what an element is painted with, as the inspector's swatches do.
    .did = hat ein Objekt neu gestaltet

d-egsj6wvu-fill = Fill

d-egsj6wvu-stroke = Stroke

d-egsj6wvu-stroke_width = Stroke width

d-egsj6wvu-font_size = Font size

d-egsj6wvu-align = Align

d-egsj6wvu-bold = Bold

d-egsj6wvu-italic = Italic

d-egsj6wvu-radius = Radius

d-rvnmx3ns = Layout
    .doc = Sets a slide's layout.
    .did = hat das Layout der Folie geändert

d-rvnmx3ns-layout = Layout

d-dl2g2jid = Hintergrund
    .doc = Sets what a slide stands on.
    .did = hat den Hintergrund der Folie geändert

d-dl2g2jid-background = Hintergrund

d-qyin6x3m = Thema der Präsentation
    .doc = Sets the deck's accent hue.
    .did = hat das Farbschema des Decks geändert

d-qyin6x3m-palette = Palette

d-d5shal5e = Erscheinungsbild der Präsentation
    .doc = Sets the deck's light or dark surfaces.
    .did = hat das Erscheinungsbild des Decks geändert

d-d5shal5e-look = Erscheinungsbild

d-evess4te = Präsentation benennen
    .doc = Names the deck.
    .w-name-the-deck-something = Name the deck something
    .did = hat das Deck umbenannt

d-evess4te-title = Titel

d-3cw3tbyv = Notizen für die Sprecherin
    .doc = Writes what the speaker says over a slide.
    .did = hat die Sprechernotizen geschrieben

d-3cw3tbyv-notes = Notes

d-mll3py2e = Kopieren
    .doc = Copies the chosen element to the clipboard as the JSON another deck pastes.

d-btdk5ete = Ausschneiden
    .doc = Cuts the chosen element: its JSON to the clipboard as another deck pastes it, and the element off the slide.
    .did = hat ein Objekt ausgeschnitten

d-btdk5ete-e = E

d-pvutm3kt = Einfügen
    .doc = Pastes an element from the clipboard onto a slide, nudged clear of the one it came from.
    .did = hat ein Objekt eingefügt

d-auooxeyg = Präsentation als Text lesen
    .doc = A deck as text: its slides in order, each title, body and other elements with its id, and its speaker notes.

d-c3kzo7bd = Präsentation öffnen
    .doc = Replaces the deck with the one a presentation file holds.
    .did = hat eine Präsentation geöffnet

d-c3kzo7bd-file = File

d-flti52c5 = Präsentationsdatei lesen
    .doc = Reads the deck a presentation file holds.

d-flti52c5-file = File

d-hhgf7tl7 = Als OpenDocument exportieren
    .doc = The deck as an OpenDocument presentation.

d-bhx54qvo = Als PowerPoint exportieren
    .doc = The deck as a PowerPoint presentation.

d-sidrg2nq = Open the presentation
    .doc = Reads a deck file into this deck: its slides replace what the deck holds.
    .did = hat eine Präsentation geöffnet

d-sidrg2nq-file = File

d-zvlzo24x = Bild einfügen
    .doc = Puts a chosen picture on the slide.
    .did = hat ein Bild eingefügt

d-zvlzo24x-file = File

d-2puqgfam = Meldung schließen
    .doc = Schließt die Meldung, die ein Deck zu einer Datei zeigt, die es nicht öffnen konnte.
    .did = hat die Meldung geschlossen

d-ecnpnt3u = Präsentation importieren
    .doc = The deck a file holds, kept as this deck: its slides replace what it held.
    .did = hat eine Präsentation importiert

d-ecnpnt3u-file = File

d-c5bdebu4 = Präsentation importieren
    .doc = Opens the first presentation of a set dropped on the window.
    .did = hat eine Präsentation importiert

d-c5bdebu4-files = Files

d-2btclxzc = Präsentation starten
    .doc = Starts the presentation clock: the speaker's elapsed time runs from now.
    .did = hat die Präsentation gestartet

d-ilm2mnkd = Präsentation beenden
    .doc = Stops the presentation clock.
    .did = hat die Präsentation beendet

d-zestriau = Uhr anhalten
    .doc = Holds the clock where it stands, until the speaker starts it again.
    .did = hat die Uhr angehalten

d-26lbs3c2 = Uhr weiterlaufen lassen
    .doc = Runs the clock on from where it was held.
    .did = hat die Uhr wieder gestartet

d-drvos6bo = Open the presentation
    .doc = Keeps one slide a deck file brings.
    .did = hat eine Präsentation geöffnet

d-drvos6bo-f = F

d-xoe77wiw = Delete slide~present

d-nkj2rko2 = Set text~present

d-jl6ovaua = Add element~present

d-rxioyyxm = Set title~present

d-5al7yk7f = Deck
    .w-presentation = Präsentation
    .w-blank = Leer
    .w-ffffff = #FFFFFF
    .w-slide-of = Folie { $value } von { $len }
    .w-current = Aktuell
    .w-next = Weiter
    .w-next-slide = Nächste Folie
    .w-end-of-the-presentation = Ende der Präsentation
    .w-speaker-notes = Notizen für die Sprecherin
    .w-no-notes-for-this-slide = Keine Notizen für diese Folie.
    .w-paused-at = Angehalten bei { $clock_text }
    .w-elapsed = Verstrichen
    .w-previous = Zurück
    .w-first = Erste
    .w-last = Letzte
    .w-pause-the-clock = Uhr anhalten
    .w-reset-the-clock = Uhr zurücksetzen
    .w-notes = Notizen
    .w-presenter = Referent
    .w-external-screen = Externer Bildschirm
    .w-black = Schwarz
    .w-white = Weiß
    .w-end = Beenden
    .w-all-slides = Alle Folien
    .w-back-to-the-editor = Zurück zum Editor
    .w-slide = Folie { $value }
    .w-slides = Folien
    .w-open = Öffnen
    .w-add-slide = Folie hinzufügen
    .w-present = Präsentieren
    .w-properties = Eigenschaften
    .w-help = Hilfe
    .w-move-the-slide-up = Folie nach oben
    .w-move-the-slide-down = Folie nach unten
    .w-duplicate = Duplizieren
    .w-delete = Löschen
    .w-slide-2 = Folie
    .w-insert = Einfügen
    .w-text = Text
    .w-rectangle = Rechteck
    .w-ellipse = Ellipse
    .w-line = Linie
    .w-picture = Bild
    .w-copy = Kopieren
    .w-paste = Einfügen
    .w-no-slides-yet = Noch keine Folien
    .w-add-a-slide-to-start-the = Füge eine Folie hinzu, um das Deck zu beginnen
    .w-fill = Füllung
    .w-fill-colour = Füllfarbe
    .w-outline = Kontur
    .w-font = Schrift
    .w-font-size = Schriftgröße
    .w-pt = pt
    .w-bold = Fett
    .w-italic = Kursiv
    .w-alignment = Ausrichtung
    .w-arrange = Anordnen
    .w-bring-front = Nach vorne
    .w-send-back = Nach hinten
    .w-nudge-left = Nach links verschieben
    .w-nudge-right = Nach rechts verschieben
    .w-nudge-up = Nach oben verschieben
    .w-nudge-down = Nach unten verschieben
    .w-previous-object = Vorheriges Objekt
    .w-next-object = Nächstes Objekt
    .w-cut = Ausschneiden
    .w-layout = Layout
    .w-background = Hintergrund
    .w-deck-look = Erscheinungsbild
    .w-deck-theme = Farbschema
    .w-what-you-say-over-this-slide = Was du über dieser Folie sagst
    .w-deck = Deck
    .w-presentation-title = Titel der Präsentation
    .w-export-as-powerpoint = Als PowerPoint exportieren
    .w-export-as-opendocument = Als OpenDocument exportieren
    .w-keyboard-shortcuts = Tastenkürzel
    .w-close = Schließen
    .w-editing = Bearbeiten
    .w-n = N
    .w-add-a-slide = Folie hinzufügen
    .w-p = P
    .w-g = G
    .w-t-r-o-l = T, R, O, L
    .w-insert-a-text-box-rectangle-ellipse = Textfeld, Rechteck, Ellipse oder Linie einfügen
    .w-ctrl-o = Ctrl+O
    .w-open-a-presentation = Präsentation öffnen
    .w-ctrl-s = Ctrl+S
    .w-export-the-deck = Deck exportieren
    .w-ctrl-d = Ctrl+D
    .w-duplicate-the-slide = Folie duplizieren
    .w-duplicate-slide = Folie duplizieren
    .w-d = D
    .w-duplicate-the-chosen-object = Ausgewähltes Objekt duplizieren
    .w-delete-the-chosen-object = Ausgewähltes Objekt löschen
    .w-and = {""}[ und ]
    .w-send-back-or-bring-front = Nach hinten oder nach vorne
    .w-ctrl-c-ctrl-x-ctrl-v = Ctrl+C, Ctrl+X, Ctrl+V
    .w-copy-cut-paste-an-object = Objekt kopieren, ausschneiden oder einfügen
    .w-drag = Ziehen
    .w-move-an-object = Objekt verschieben
    .w-drag-an-edge = An einer Kante ziehen
    .w-size-an-object = Objekt skalieren
    .w-esc = Esc
    .w-close-this-reference = Diese Übersicht schließen
    .w-presenting = Präsentieren
    .w-left-right = Links, Rechts
    .w-previous-or-next-slide = Vorherige oder nächste Folie
    .w-home-end = Pos1, Ende
    .w-first-or-last-slide = Erste oder letzte Folie
    .w-1-to-9 = 1 bis 9
    .w-go-to-a-slide = Zu einer Folie springen
    .w-s = S
    .w-presenter-view = Referentenansicht
    .w-b-w = B, W
    .w-black-or-white-screen = Schwarzer oder weißer Bildschirm
    .w-k-0 = K, 0
    .w-pause-or-reset-the-clock = Uhr anhalten oder zurücksetzen
    .w-end-the-presentation = Präsentation beenden
    .w-fit = Anpassen
    .w-bring-to-front = Nach vorn
    .w-send-to-back = Nach hinten

d-jnyka7fe = Element view
    .w-add-text = Add text
    .w-picture = Bild
    .w-choose-a-picture = Choose a picture

d-bljokktq = Slide picture

d-eshxh4fk = Folienseite
    .w-slide = Folie
    .w-speaker-notes = Notizen für die Sprecherin
    .w-what-you-say-over-this-slide = Was du über dieser Folie sagst

d-34l2smm3 = Textfeld-Seite
    .w-slide = Folie
    .w-the-box-s-words = Die Worte des Textfelds
    .w-add-text = Text hinzufügen
    .w-this-box-s-slide-is-gone = Die Folie dieses Textfelds ist nicht mehr da
    .w-the-slide-it-stood-on-is = Die Folie, auf der es stand, ist nicht mehr in der Präsentation

d-7tizubwd = Folie verschieben
    .doc = Verschiebt die gezogene Folie dorthin, wo sie abgelegt wurde: die Kennung der Folie und die Kennung der Folie, auf der sie abgelegt wurde, wie sie die Sortierung der Leiste mitführt.
    .did = hat eine Folie verschoben

d-7tizubwd-ids = Kennungen

d-c2gzexjw = Element löschen
    .doc = Löscht das Element von `s`, dessen Kennung `target` ist, wie es das Löschen der Bühne benennt.
    .did = hat ein Objekt gelöscht

d-c2gzexjw-target = Ziel
