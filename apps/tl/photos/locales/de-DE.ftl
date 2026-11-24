# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-photos = Fotos

d-so-tilo-photos-keywords = photo, photos, picture, pictures, image, images, album, albums, favorites, gallery, camera roll, foto, fotos, bild, bilder, alben, favoriten

d-jwejbqyn = Beschreiben
    .doc = Was ein Bild zeigt, in einem Satz, wie das Bildmodell des Geräts es liest: der Satz, den die Details der Bibliothek zeigen und die Suche findet.

d-3uzo3qzj = Tags
    .doc = Die Objekte und Tiere, die das Bildmodell des Geräts in einem Bild findet, zu dem hinzugefügt, was das Foto schon aufführt.

d-xo4knffj = Zuschneiden
    .doc = Behält den Teil des Fotos, den die Person gewählt hat: `x`, `y`, `width` und `height` sind Anteile des Bildes, wie das Fenster es zeigt, sodass jedes Mitglied und jedes Gerät den Zuschnitt liest.

d-xo4knffj-x = X

d-xo4knffj-y = Y

d-xo4knffj-width = Breite

d-xo4knffj-height = Höhe

d-vf6rmvyr = Foto
    .doc = Ein Foto in der Bibliothek: sein Name, die Adresse seines Bildes und was die Bibliothek darüber weiß.
    .noun = { $count ->
        [one] Foto
       *[other] Fotos
    }
    .untitled = Foto

d-rv4vs7su = Name

d-r55t3rg3 = Bild
    .doc = Wo die Bytes des Bildes sind: eine Adresse `tilo-blob://<reference>`, die die Bilder des Fensters aus dem Inhaltsspeicher lesen.

d-yyfm6s7t = Favorit

d-wm3tr75p = Aufgenommen am
    .doc = Wann das Foto aufgenommen wurde, wie die Kamera es schrieb: als Worte bewahrt, bis etwas eine Zeit daraus lesen muss.

d-zz3fpxbt = Ordner
    .doc = Der Ordner, aus dem es kam, wo die Bibliothek einen kennt.

d-5bhxlhcf = Tags

d-vlem53gy = Personen

d-bgo4zgpq = Ort
    .doc = Der Ort, an dem das Foto aufgenommen wurde, in Worten einer Person.

d-kko7mkgg = Breite
    .doc = Die Koordinaten, die der Ort nennt: null, wo keine gesetzt ist.

d-4xox6lum = Länge

d-brtcqh34 = Beschreibung
    .doc = Was das Bild zeigt, wie eine Person oder eine Analyse es schrieb.

d-6esqywn4 = Objekte
    .doc = Objekte und Haustiere, die das Bild zeigt, benannt.

d-7rzyj24a = Haustiere

d-vf6rmvyr-filter-favorite = Favorit

d-vf6rmvyr-filter-tags = Tags

d-vf6rmvyr-filter-people = Personen

d-ur5n6nzw = Album
    .doc = Eine Sammlung von Fotos, die jemand benannt hat: ihre Fotos, an den Ids, die sie in der Bibliothek haben.
    .noun = { $count ->
        [one] Album
       *[other] Alben
    }
    .untitled = Album

d-unujaudz = Name

d-k7fl2y27 = Fotos

d-ur5n6nzw-filter-photos = Fotos

d-hc736lwy = Person
    .doc = Eine Person, die die Bibliothek kennt: ihr Name und die Fotos, auf denen sie ist, nach Id.
    .noun = { $count ->
        [one] Person
       *[other] Personen
    }
    .untitled = Person

d-segahwtp = Name

d-vbzsnqg5 = Fotos

d-hc736lwy-filter-photos = Fotos

d-xea2av67 = Vorschlag
    .doc = Ein Gesicht, das das Gerät gefunden hat, und die Fotos, auf denen es zu sehen ist.
    .noun = { $count ->
        [one] Vorschlag
       *[other] Vorschläge
    }

d-qracadw6 = Name
    .doc = Der Name der Person, leer, solange der Vorschlag keinen hat.

d-ehc6rrhm = Fotos
    .doc = Die Fotos, auf denen dieses Gesicht zu sehen ist, nach ihren Kennungen in der Bibliothek.

d-2adnxzro = Gesendet
    .doc = Ob ein Name bestätigt wurde: ein gesendeter Vorschlag gehört dem Arbeitsbereich, nicht nur seinem Autor.

d-xea2av67-filter-photos = Fotos

d-xea2av67-filter-sent = Gesendet

d-j45kskqf = Bibliothek
    .doc = Die Fotobibliothek des Arbeitsbereichs: jedes Foto, die Alben, die Menschen daraus machten, und die Menschen, die die Fotos zeigen.
    .noun = { $count ->
        [one] Bibliothek
       *[other] Bibliotheken
    }

d-moeuycny = Fotos

d-tqa4lyow = Alben

d-xr5mtp2m = Personen

d-r7k2giak = Vorschläge
    .doc = Gesichter, die das Bildmodell gefunden hat und die noch niemand benannt hat: jedes gehört allein dem, der es gescannt hat, bis er einen Namen bestätigt und es damit in den Arbeitsbereich gibt.

d-z2bwvzat = Foto importieren
    .doc = Importiert ein Foto, das die Person auswählt: die Bibliothek behält die Adresse seiner Bytes und den Namen, mit dem es kam.
    .did = hat ein Foto hinzugefügt

d-z2bwvzat-file = Datei

d-hexbnfml = Foto mit Metadaten importieren
    .doc = Importiert ein Foto, dessen Aufnahmezeit und Ort der Aufrufer schon kennt (die Übergabe der Kamera).
    .did = hat ein Foto hinzugefügt

d-hexbnfml-file = Datei

d-hexbnfml-captured_at = Aufgenommen am

d-hexbnfml-location = Ort

d-vk76kjxc = Abgelegte importieren
    .doc = Importiert die Bilder, die eine Person auf das Fenster zieht: jede Bilddatei wird ein Foto, und eine Datei, die kein Bild ist, bleibt unberührt.
    .did = hat Fotos hinzugefügt

d-vk76kjxc-files = Dateien

d-4y5pwp2y = Favorit umschalten
    .doc = Markiert ein Foto als Favorit oder nimmt das zurück.

d-4y5pwp2y-favorite = Favorit

d-zyrmacyr = Ausgewählte favorisieren
    .doc = Markiert jedes genannte Foto als Favorit.

d-zyrmacyr-ids = Ids

d-vmzzie5o = Ausgewählte entfernen
    .doc = Nimmt jedes genannte Foto aus der Bibliothek.

d-vmzzie5o-ids = Ids

d-3gje7fzu = Zu Album hinzufügen
    .doc = Legt jedes genannte Foto in ein Album.

d-3gje7fzu-ids = Ids

d-vr7jehtb = Tag
    .doc = Gibt einem Foto einen Tag.

d-vr7jehtb-t = T

d-6asxmdwz = Tag entfernen
    .doc = Nimmt einem Foto einen Tag ab.

d-6asxmdwz-t = T

d-uyraencp = Aus der Bibliothek entfernen
    .doc = Nimmt ein Foto aus der Bibliothek.

d-uyraencp-p = F

d-myakm7gv = Neues Album
    .doc = Beginnt ein Album und legt das Foto, von dem es ausgeht, hinein.

d-myakm7gv-p = F

d-nw5ubpyd = Album umbenennen
    .doc = Benennt ein Album um.

d-nw5ubpyd-to = An

d-jd7bl7ux = Album löschen
    .doc = Löscht ein Album.

d-jd7bl7ux-a = A

d-nf3xznu3 = Album
    .doc = Legt ein Foto in ein Album oder nimmt es heraus, wenn es schon darin ist.

d-nf3xznu3-p = F

d-vtt5pvr6 = Teilen
    .doc = Der Link, der das Bild eines Fotos erreicht: die Adresse seiner Bytes, die jedes Gerät des Arbeitsbereichs aus dem Inhaltsspeicher liest.

d-mexlkmwn = Person hinzufügen
    .doc = Nennt eine Person auf einem Foto und legt das Foto auf diese Person, wenn die Bibliothek sie kennt.

d-mexlkmwn-p = F

d-mexlkmwn-person = Person

d-kmbs5gxw = Person entfernen
    .doc = Nimmt eine Person von einem Foto und das Foto von dieser Person.

d-kmbs5gxw-p = F

d-kmbs5gxw-person = Person

d-fltwmykq = Person umbenennen
    .doc = Benennt eine Person: die Fotos, auf denen sie ist, sagen auch den neuen Namen.

d-fltwmykq-person = Person

d-fltwmykq-to = An

d-qwdie7fn = Gesichter finden
    .doc = Die Gesichter, die das Gerät in `photos` findet, als Vorschläge der Person selbst: ein Gesicht hat keinen Namen, also erreicht nichts davon ein anderes Mitglied, bis ein Name bestätigt ist.

d-qwdie7fn-photos = Fotos

d-7223fwrx = Nach Gesichtern suchen
    .doc = Sucht die nächsten Fotos auf diesem Gerät, auf denen noch niemand benannt ist: das Modell nimmt zwölf auf einmal, also ist eine Suche ein Durchgang, und jedes Gesicht, das sie findet, ist ein Vorschlag des Suchenden selbst.

d-x3kp75qe = Den Namen bestätigen
    .doc = Benennt ein Gesicht: seine Fotos kommen zu der Person mit diesem Namen, und der Vorschlag wird gesendet, sodass der Arbeitsbereich ihn behält.
    .w-enter-a-name-first = Gib zuerst einen Namen ein

d-x3kp75qe-s = V

d-x3kp75qe-name = Name

d-zsaipz5s = Vorschlag zusammenführen
    .doc = Legt die Fotos eines Gesichts auf eine Person, die die Bibliothek schon kennt, und sendet den Vorschlag.

d-zsaipz5s-s = V

d-zsaipz5s-person = Person

d-zpg34mwz = Den Vorschlag verwerfen
    .doc = Nimmt einen Vorschlag weg: das Gesicht wird nicht wieder angeboten, da die Fotos nicht erneut zu einem zusammengefasst werden, solange sie nur aus freier Wahl unbenannt sind.

d-zpg34mwz-s = V

d-diaii65m = Person mit Namen
    .doc = Die Person, die die Bibliothek unter `name` kennt, angelegt, wenn sie keine kennt.

d-diaii65m-name = Name

d-fdh5jyv7 = Neue Person
    .doc = Beginnt eine Person, die die Bibliothek kennt, mit den Fotos, die sie schon von ihr hat.

d-fdh5jyv7-name = Name

d-fdh5jyv7-photos = Fotos

d-xy3z7l53 = Personen zusammenführen
    .doc = Führt eine Person in eine andere: ihre Fotos kommen unter dem behaltenen Namen zusammen, und der zusammengeführte Name verlässt jedes Foto, auf dem er war.

d-xy3z7l53-from = Von

d-xy3z7l53-into = In

d-f6pmjtlz = Von dieser Person nehmen
    .doc = Nimmt ein Foto von einer Person und die Person von diesem Foto.

d-f6pmjtlz-person = Person

d-f6pmjtlz-photo = Foto

d-s6nznpqj = Person vergessen
    .doc = Vergisst eine Person: ihr Name verlässt jedes Foto, auf dem sie war.

d-s6nznpqj-person = Person

d-ihd2ld2t = Ort setzen
    .doc = Setzt den Ort, an dem ein Foto aufgenommen wurde, aus der Stelle, auf die die Person auf der Karte zeigt: die Auswahl meldet den Punkt als Breite und dann Länge.

d-ihd2ld2t-at = Bei

d-4ufdgmft = Ort löschen
    .doc = Nimmt den Ort von einem Foto.

d-i45tq7ne = Foto speichern
    .doc = Das Originalbild als Datei, zum Speichern, wo die Person es sagt.

d-7ktbi2nd = Vorschlag bestätigen~present

d-2i7i3as5 = Bearbeitet
    .doc = Das Bild nach den Drehungen der Person, wo sie welche gemacht hat: was das Fenster zeigt und ein Speichern schreibt.

d-ktv6ci4q = Drehung
    .doc = Die Vierteldrehungen im Uhrzeigersinn, die die Person gemacht hat.

d-vwrtdas3 = Vorschau
    .doc = Die kleine Vorschau, die das Raster zeichnet, damit eine Bibliothek mit Tausenden günstig aufgeht.

d-6a6afnkj = Änderungen zurücksetzen
    .doc = Setzt das Foto zurück, wie es kam, und nimmt die Personendrehungen ab.

d-hquyf5vr = Nach links drehen
    .doc = Dreht das Foto eine Vierteldrehung gegen den Uhrzeigersinn.

d-wifovdyg = Nach rechts drehen
    .doc = Dreht das Foto eine Vierteldrehung im Uhrzeigersinn, wie seine Person es sieht: das Fenster zeichnet das Original gedreht, und ein Speichern schreibt, was es zeichnet.

d-hfgcskel = Bibliothek
    .w-photos = Fotos
    .w-select-all = Alle auswählen
    .w-clear-selection = Auswahl aufheben
    .w-favorite-selected = Ausgewählte favorisieren
    .w-remove-selected = Ausgewählte entfernen
    .w-done = Fertig
    .w-open = Öffnen
    .w-import-folder = Ordner importieren
    .w-select = Auswählen
    .w-present = Präsentieren
    .w-analyzing-of = Analysiere { $completed } von { $total }
    .w-analysis-paused = Analyse pausiert
    .w-analyze-photos = Fotos analysieren
    .w-scan-for-faces = Nach Gesichtern suchen
    .w-library = Bibliothek
    .w-favorites = Favoriten
    .w-timeline = Zeitleiste
    .w-people = Personen
    .w-tags = Tags
    .w-folders = Ordner
    .w-places = Orte
    .w-albums = Alben
    .w-previous = Vorheriges
    .w-next = Nächstes
    .w-stop = Beenden
    .w-zoom-out = Verkleinern
    .w-zoom-in = Vergrößern
    .w-actual-size = Originalgröße
    .w-back = Zurück
    .w-remove-from-favorites = Aus Favoriten entfernen
    .w-favorite = Favorit
    .w-rotate-left = Nach links drehen
    .w-rotate-right = Nach rechts drehen
    .w-crop = Zuschneiden
    .w-describe = Beschreiben
    .w-reset-edits = Änderungen zurücksetzen
    .w-share = Teilen
    .w-remove = Entfernen
    .w-take-this-photo-out-of-the = Dieses Foto aus der Bibliothek nehmen?
    .w-remove-from-library = Aus der Bibliothek entfernen
    .w-cancel = Abbrechen
    .w-drag-the-corners-to-the-part = Ziehe die Ecken auf den Teil, den du behalten willst.
    .w-apply-crop = Zuschnitt übernehmen
    .w-faces-to-name = Zu benennende Gesichter
    .w-name-this-person = Diese Person benennen
    .w-approve = Bestätigen
    .w-merge = Zusammenführen
    .w-dismiss-the-suggestion = Den Vorschlag verwerfen
    .w-photos-suggested-on-this-device-shared = { $len } Fotos auf diesem Gerät vorgeschlagen, erst mit dem Namen geteilt.
    .w-no-people-yet = Noch keine Personen
    .w-no-capture-date = Kein Aufnahmedatum
    .w-named-places = Benannte Orte
    .w-the-vision-service-proposes-the-people = Der Sehdienst schlägt die Personen in deinen Fotos vor; benenne, führe und teile sie hier.
    .w-smaller-thumbnails = Kleinere Vorschaubilder
    .w-larger-thumbnails = Größere Vorschaubilder
    .w-imported-of = { $import_done } von { $import_found } importiert
    .w-every-file-became-a-photo = Jede Datei wurde ein Foto.
    .w-not-pictures = Keine Bilder: { $join }
    .w-dismiss = Schließen
    .w-read-the-text = Text lesen
    .w-copy = Kopieren
    .w-map = Karte
    .w-list = Liste
    .w-edit-details = Details bearbeiten
    .w-details-of-photos = Details von { $len } Fotos
    .w-only-the-fields-you-fill-in = Nur die Felder, die du ausfüllst, ändern sich; jedes Foto behält den Rest. Ein Feld, in dem sich die Fotos unterscheiden, zeigt Gemischt.
    .w-organize = Organisieren
    .w-folder = Ordner: { $shared_line }
    .w-folder-to-give-every-photo = Ordner für alle Fotos
    .w-captured = Aufgenommen: { $shared_line }
    .w-captured-time-to-give-every-photo = Aufnahmezeit für alle Fotos
    .w-description = Beschreibung: { $shared_line }
    .w-description-to-give-every-photo = Beschreibung für alle Fotos
    .w-tags-2 = Tags: { $shared_names_line }
    .w-tags-to-add-separated-by-commas = Tags zum Hinzufügen, durch Kommas getrennt
    .w-tags-to-take-off-separated-by = Tags zum Entfernen, durch Kommas getrennt
    .w-people-2 = Personen: { $shared_names_line }
    .w-people-to-add-separated-by-commas = Personen zum Hinzufügen, durch Kommas getrennt
    .w-people-to-take-off-separated-by = Personen zum Entfernen, durch Kommas getrennt
    .w-place = Ort
    .w-place-2 = Ort: { $place_value }
    .w-give-every-photo-the-same-place = Allen Fotos denselben Ort geben
    .w-place-name = Ortsname
    .w-latitude = Breite
    .w-longitude = Länge
    .w-save = Speichern
    .w-clear = Leeren
    .w-folder-on-every-photo = Ordner aller Fotos
    .w-captured-time = Aufnahmezeit
    .w-description-2 = Beschreibung
    .w-all-tags = Alle Tags
    .w-everyone = Alle Personen
    .w-the-place = Den Ort
    .w-all-months = Alle Monate
    .w-every-day = Jeden Tag

d-i4ssir2r = Personenzeile
    .w-photos = { $len } Fotos

d-pwxkkjpa = Person
    .w-photos = { $len } Fotos

d-opj2blvg = Leerer Ort
    .w-no-favorites-yet = Noch keine Favoriten
    .w-star-a-photo-to-find-it = Markiere ein Foto mit einem Stern, um es hier zu finden.
    .w-no-people-yet = Noch keine Personen
    .w-name-the-people-on-a-photo = Benenne die Personen auf einem Foto in seinen Details.
    .w-no-tags-yet = Noch keine Tags
    .w-tag-a-photo-in-its-details = Vergib einem Foto in seinen Details ein Tag.
    .w-no-folders-yet = Noch keine Ordner
    .w-a-photo-s-folder-is-where = Der Ordner eines Fotos ist der, aus dem die Bibliothek es kennt.
    .w-no-places-yet = Noch keine Orte
    .w-give-a-photo-a-place-on = Gib einem Foto in seinen Details einen Ort auf der Karte.
    .w-this-album-is-empty = Dieses Album ist leer
    .w-add-photos-to-it-from-the = Füge ihm Fotos aus der Bibliothek hinzu.
    .w-your-library-is-empty = Deine Bibliothek ist leer
    .w-open-a-photo-or-drop-pictures = Öffne ein Foto oder ziehe Bilder auf das Fenster, um deine Bibliothek zu beginnen.

d-dy2z5spw = Detailfelder
    .w-captured-time = Aufnahmezeit
    .w-folder = Ordner
    .w-place = Ort
    .w-description = Beschreibung
    .w-where-it-was-taken = Wo es aufgenommen wurde
    .w-save-photograph = Foto speichern
    .w-clear-place = Ort löschen
    .w-text-in-the-picture = Text im Bild
    .w-read-the-text = Text lesen
    .w-copy = Kopieren
    .w-latitude = Breite
    .w-longitude = Länge

d-dho5ecc7 = Details
    .w-albums = Alben

d-coxhshbn = Foto
    .w-zoom-out = Verkleinern
    .w-zoom-in = Vergrößern
    .w-actual-size = Originalgröße
    .w-remove-from-favorites = Aus Favoriten entfernen
    .w-favorite = Favorit

d-vy4hppbk = Fotozeile

d-5qykqlov = Fotos
    .w-no-photos-yet = Noch keine Fotos

d-34ridqnu = Bericht schließen
    .doc = Nimmt den Bericht des letzten Imports vom Fenster.

d-3cyhe74u = Bild aufnehmen
    .doc = Fügt das Bild einer Datei der Bibliothek hinzu: das Foto, das dasselbe Bild schon ergab, auf den Stand der Datei gebracht, oder ein neues.

d-3cyhe74u-file = Datei

d-5lnp7pby = Kopieren
    .doc = Kopiert das Foto als Bild in die Zwischenablage, sodass es sich in eine andere App einfügen lässt.

d-6jdgyzo3 = Importiert

d-ybraeglq = Importprobleme

d-p4pebna2 = Erkannter Text
    .doc = Die Wörter im Bild, wie dieses Gerät sie gelesen hat: ein Schild, eine Seite, ein Schaufenster.

d-psxrvclu = Text lesen
    .doc = Die Wörter im Foto, auf diesem Gerät gelesen und beim Foto behalten, sodass die Bibliothek es an seinem Text findet.

d-tp7e7pzd = Gefundene Dateien
    .doc = Der letzte Ordnerimport: wie viele Dateien er ansah, wie viele Fotos wurden und was er nicht lesen konnte.

d-j45kskqf-filter-import_found = Gefundene Dateien

d-j45kskqf-filter-import_done = Importiert

d-r6w2zb6b = Änderung
    .doc = Was eine Bearbeitung mit einem Feld jedes ausgewählten Fotos tut: es behält, was jedes Foto hat, nimmt es allen weg, gibt allen einen Wert oder fügt Namen (Tags, Personen) hinzu oder nimmt sie weg.

d-d3saeby7 = Jedes Foto behält seinen Wert
    .doc = Jedes Foto behält seinen Wert.

d-3eritqkv = Der Wert wird jedem Foto genommen
    .doc = Der Wert wird jedem Foto genommen.

d-7klkxalf = Jedes Foto bekommt denselben Wert
    .doc = Jedes Foto bekommt denselben Wert.

d-hvmemend = Jedes Foto bekommt diese Namen; was es schon hat, bleibt
    .doc = Jedes Foto bekommt diese Namen; was es schon hat, bleibt.

d-crqeyfqu = Diese Namen werden jedem Foto genommen; der Rest bleibt
    .doc = Diese Namen werden jedem Foto genommen; der Rest bleibt.

d-smn7av4u = Details bearbeiten
    .doc = Ändert die Details jedes genannten Fotos als eine Änderung: ein Feld auf `keep` behält, was jedes Foto hat, `clear` nimmt es allen, `set` gibt allen einen Wert, und Tags und Personen werden über die Auswahl hinzugefügt oder weggenommen, sodass jedes Foto den Rest seines eigenen behält.
    .did = hat die Details von Fotos geändert
    .w-fill-in-a-field-to-change = Füll ein Feld aus, um etwas zu ändern

d-smn7av4u-ids = IDs

d-smn7av4u-tags = Tags

d-smn7av4u-tag_names = Tag-Namen

d-smn7av4u-tags_off = Tags entfernen

d-smn7av4u-people = Personen

d-smn7av4u-person_names = Personennamen

d-smn7av4u-people_off = Personen entfernen

d-smn7av4u-folder = Ordner

d-smn7av4u-folder_text = Ordnertext

d-smn7av4u-captured_at = Aufgenommen am

d-smn7av4u-captured_text = Aufnahmezeit

d-smn7av4u-description = Beschreibung

d-smn7av4u-description_text = Beschreibungstext

d-smn7av4u-place = Ort

d-smn7av4u-place_name = Ortsname

d-smn7av4u-latitude = Breite

d-smn7av4u-longitude = Länge

d-nu2l23l6 = Geteilte Zeile
    .w-not-set = Nicht gesetzt
    .w-mixed = Gemischt

d-emafkrvf = Geteilte Namenszeile
    .w-none = Keine
    .w-mixed = Gemischt

d-v2p3fpqt = Auswahl bearbeiten~present
