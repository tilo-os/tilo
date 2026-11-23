#!/usr/bin/env python3
"""Write apps/tl/photos/locales/de-DE.ftl from the generated en-US catalogue, translating
each message's words by the dictionary below. Every English value must be in the dictionary:
the run fails (and lists what is missing) rather than ship English in the German catalogue."""
import pathlib, re, sys

DE = {
 # app
 "Photos": "Fotos",
 "Your photos, together": "Deine Fotos, zusammen",
 # types
 "Photo": "Foto",
 "Album": "Album",
 "Library": "Bibliothek",
 "A photo in the library: its name, the address of its picture, and what the library knows of it. The address names the bytes in the workspace's content store, so every member of the workspace reads the same picture wherever the library reached them.":
   "Ein Foto in der Bibliothek: sein Name, die Adresse seines Bildes und was die Bibliothek darüber weiß. Die Adresse benennt die Bytes im Inhaltsspeicher des Arbeitsbereichs, sodass jedes Mitglied des Arbeitsbereichs dasselbe Bild liest, wo die Bibliothek es auch erreicht hat.",
 "A collection of photos a person named: its photos, by the ids they have in the library.":
   "Eine Sammlung von Fotos, die jemand benannt hat: ihre Fotos, an den Ids, die sie in der Bibliothek haben.",
 "The workspace's photo library: every photo, and the albums people made of them.":
   "Die Fotobibliothek des Arbeitsbereichs: jedes Foto und die Alben, die Menschen daraus gemacht haben.",
 # fields
 "Name": "Name",
 "Picture": "Bild",
 "Favorite": "Favorit",
 "Captured at": "Aufgenommen am",
 "Folder": "Ordner",
 "Tags": "Tags",
 "People": "Personen",
 "Photos": "Fotos",
 "Albums": "Alben",
 "Where the picture's bytes are: an address `tilo-blob://<reference>` the window's pictures read from the content store. Empty where the bytes are not here.":
   "Wo die Bytes des Bildes sind: eine Adresse `tilo-blob://<reference>`, die die Bilder des Fensters aus dem Inhaltsspeicher lesen. Leer, wo die Bytes nicht hier sind.",
 "When the photo was taken, as the camera wrote it: kept as words until something needs to read a time from them.":
   "Wann das Foto aufgenommen wurde, wie die Kamera es schrieb: als Worte bewahrt, bis etwas eine Zeit daraus lesen muss.",
 "The folder it came in from, where the library knows one.":
   "Der Ordner, aus dem es kam, wo die Bibliothek einen kennt.",
 # actions: docs
 "Imports a photograph the person picks: the library keeps the address of its bytes and the name it came in with. A picture whose bytes the window already holds reads at once.":
   "Importiert ein Foto, das die Person auswählt: die Bibliothek behält die Adresse seiner Bytes und den Namen, mit dem es kam. Ein Bild, dessen Bytes das Fenster schon hält, liest sofort.",
 "Imports the pictures a person drops on the window: each picture file becomes a photo, and a file that is no picture is left alone.":
   "Importiert die Bilder, die eine Person auf das Fenster zieht: jede Bilddatei wird ein Foto, und eine Datei, die kein Bild ist, bleibt unberührt.",
 "Marks a photo as a favorite, or takes that back.":
   "Markiert ein Foto als Favorit oder nimmt das zurück.",
 "Gives a photo a tag.": "Gibt einem Foto einen Tag.",
 "Takes a tag off a photo.": "Nimmt einem Foto einen Tag ab.",
 "Takes a photo out of the library. The file it came from is not touched.":
   "Nimmt ein Foto aus der Bibliothek. Die Datei, aus der es kam, bleibt unberührt.",
 "Starts an album, and puts the photo it is started from in it.":
   "Beginnt ein Album und legt das Foto, von dem es ausgeht, hinein.",
 "Renames an album.": "Benennt ein Album um.",
 "Deletes an album. Its photos stay in the library.":
   "Löscht ein Album. Seine Fotos bleiben in der Bibliothek.",
 "Puts a photo in an album, or takes it out of one when it is already in it.":
   "Legt ein Foto in ein Album oder nimmt es heraus, wenn es schon darin ist.",
 "The link that reaches a photo's picture: the address of its bytes, which every device of the workspace reads from the content store.":
   "Der Link, der das Bild eines Fotos erreicht: die Adresse seiner Bytes, die jedes Gerät des Arbeitsbereichs aus dem Inhaltsspeicher liest.",
 # action labels / dids / groups
 "added a photo": "hat ein Foto hinzugefügt",
 "added photos": "hat Fotos hinzugefügt",
 "Untag": "Tag entfernen",
 "New album": "Neues Album",
 "Rename album": "Album umbenennen",
 "Delete album": "Album löschen",
 "Remove from library": "Aus der Bibliothek entfernen",
 # views / UI
 "Library": "Bibliothek",
 "Favorites": "Favoriten",
 "Open": "Öffnen",
 "No favorites yet": "Noch keine Favoriten",
 "Star a photo to find it here.": "Markiere ein Foto mit einem Stern, um es hier zu finden.",
 "This album is empty": "Dieses Album ist leer",
 "Add photos to it from the library.": "Füge ihm Fotos aus der Bibliothek hinzu.",
 "Your library is empty": "Deine Bibliothek ist leer",
 "Open a photo, or drop pictures on the window, to start your library.":
   "Öffne ein Foto oder ziehe Bilder auf das Fenster, um deine Bibliothek zu beginnen.",
 "Back": "Zurück",
 "Previous": "Vorheriges",
 "Next": "Nächstes",
 "Remove from favorites": "Aus Favoriten entfernen",
 "Share": "Teilen",
 "Remove": "Entfernen",
 "Take this photo out of the library?": "Dieses Foto aus der Bibliothek nehmen?",
 "Cancel": "Abbrechen",
 "No photos yet": "Noch keine Fotos",
 # nouns
 "photo": "Foto", "photos": "Fotos",
 "album": "Album", "albums": "Alben",
 "library": "Bibliothek", "libraries": "Bibliotheken",
}


DE.update({
 'A photo in the library: its name, the address of its picture, and what the library knows of it.': 'Ein Foto in der Bibliothek: sein Name, die Adresse seines Bildes und was die Bibliothek darüber weiß.',
 "Where the picture's bytes are: an address `tilo-blob://<reference>` the window's pictures read from the content store.": 'Wo die Bytes des Bildes sind: eine Adresse `tilo-blob://<reference>`, die die Bilder des Fensters aus dem Inhaltsspeicher lesen.',
 'Location': 'Ort', 'The place the photo was taken, as words a person set.': 'Der Ort, an dem das Foto aufgenommen wurde, in Worten einer Person.',
 'Latitude': 'Breite', 'The coordinates that place names: nought where none is set.': 'Die Koordinaten, die der Ort nennt: null, wo keine gesetzt ist.',
 'Longitude': 'Länge', 'Description': 'Beschreibung', 'What the picture shows, as a person or an analysis wrote it.': 'Was das Bild zeigt, wie eine Person oder eine Analyse es schrieb.',
 'Objects': 'Objekte', 'Objects and pets the picture shows, named.': 'Objekte und Haustiere, die das Bild zeigt, benannt.', 'Pets': 'Haustiere',
 'Person': 'Person', 'A person the library knows: their name and the photos they are on, by id.': 'Eine Person, die die Bibliothek kennt: ihr Name und die Fotos, auf denen sie ist, nach Id.',
 "The workspace's photo library: every photo, the albums people made of them, and the people the photos show.": 'Die Fotobibliothek des Arbeitsbereichs: jedes Foto, die Alben, die Menschen daraus machten, und die Menschen, die die Fotos zeigen.',
 'Import photo': 'Foto importieren', 'Imports a photograph the person picks: the library keeps the address of its bytes and the name it came in with.': 'Importiert ein Foto, das die Person auswählt: die Bibliothek behält die Adresse seiner Bytes und den Namen, mit dem es kam.',
 'File': 'Datei', 'Import photo meta': 'Foto mit Metadaten importieren',
 "Imports a photograph a caller already knows the capture time and place of (the camera's handoff).": 'Importiert ein Foto, dessen Aufnahmezeit und Ort der Aufrufer schon kennt (die Übergabe der Kamera).',
 'Import dropped': 'Abgelegte importieren', 'Files': 'Dateien', 'Toggle favorite': 'Favorit umschalten', 'Favorite selected': 'Ausgewählte favorisieren',
 'Marks each named photo a favorite.': 'Markiert jedes genannte Foto als Favorit.', 'Ids': 'Ids', 'Remove selected': 'Ausgewählte entfernen',
 'Takes each named photo out of the library.': 'Nimmt jedes genannte Foto aus der Bibliothek.', 'Add to album': 'Zu Album hinzufügen',
 'Puts each named photo in an album.': 'Legt jedes genannte Foto in ein Album.', 'Tag': 'Tag', 'T': 'T', 'Takes a photo out of the library.': 'Nimmt ein Foto aus der Bibliothek.', 'P': 'F', 'To': 'An',
 'Deletes an album.': 'Löscht ein Album.', 'A': 'A', 'Add person': 'Person hinzufügen',
 'Names a person on a photo, and puts the photo on that person when the library knows them.': 'Nennt eine Person auf einem Foto und legt das Foto auf diese Person, wenn die Bibliothek sie kennt.',
 'Remove person': 'Person entfernen', 'Takes a person off a photo, and the photo off that person.': 'Nimmt eine Person von einem Foto und das Foto von dieser Person.',
 'Rename person': 'Person umbenennen', 'Names a person: the photos they are on say the new name too.': 'Benennt eine Person: die Fotos, auf denen sie ist, sagen auch den neuen Namen.',
 'New person': 'Neue Person', 'Starts a person the library knows, with the photos it already has of them.': 'Beginnt eine Person, die die Bibliothek kennt, mit den Fotos, die sie schon von ihr hat.',
 'Merge people': 'Personen zusammenführen', 'Merges one person into another: their photos come together under the name kept, and the name merged away leaves every photo it was on.': 'Führt eine Person in eine andere: ihre Fotos kommen unter dem behaltenen Namen zusammen, und der zusammengeführte Name verlässt jedes Foto, auf dem er war.',
 'From': 'Von', 'Into': 'In', 'Take off this person': 'Von dieser Person nehmen', 'Takes one photo off a person, and the person off that photo.': 'Nimmt ein Foto von einer Person und die Person von diesem Foto.',
 'Forget person': 'Person vergessen', 'Forgets a person: their name leaves every photo they were on.': 'Vergisst eine Person: ihr Name verlässt jedes Foto, auf dem sie war.',
 'Set place': 'Ort setzen', 'Sets the place a photo was taken from where the person points on the map: the picker reports the point as its latitude then its longitude.': 'Setzt den Ort, an dem ein Foto aufgenommen wurde, aus der Stelle, auf die die Person auf der Karte zeigt: die Auswahl meldet den Punkt als Breite und dann Länge.',
 'At': 'Bei', 'Clear place': 'Ort löschen', 'Takes the place off a photo.': 'Nimmt den Ort von einem Foto.',
 'Save photograph': 'Foto speichern', 'The original picture as a file, to save where the person says.': 'Das Originalbild als Datei, zum Speichern, wo die Person es sagt.',
 'Select all': 'Alle auswählen', 'Clear selection': 'Auswahl aufheben', 'Done': 'Fertig', 'Select': 'Auswählen', 'Present': 'Präsentieren',
 'Timeline': 'Zeitleiste', 'Folders': 'Ordner', 'Places': 'Orte', 'Stop': 'Beenden', 'Zoom out': 'Verkleinern', 'Zoom in': 'Vergrößern', 'Actual size': 'Originalgröße',
 'No people yet': 'Noch keine Personen', 'The vision service proposes the people in your photos; name them, merge them and split them here.': 'Der Sehdienst schlägt die Personen in deinen Fotos vor; benenne, führe und teile sie hier.',
 'Person row': 'Personenzeile', '{ $len } photos': '{ $len } Fotos', 'Empty place': 'Leerer Ort', 'Name the people on a photo in its details.': 'Benenne die Personen auf einem Foto in seinen Details.',
 'No tags yet': 'Noch keine Tags', 'Tag a photo in its details.': 'Vergib einem Foto in seinen Details ein Tag.', 'No folders yet': 'Noch keine Ordner',
 "A photo's folder is where the library knows it came from.": 'Der Ordner eines Fotos ist der, aus dem die Bibliothek es kennt.',
 'No places yet': 'Noch keine Orte', 'Give a photo a place on the map in its details.': 'Gib einem Foto in seinen Details einen Ort auf der Karte.',
 'Details fields': 'Detailfelder', 'Captured time': 'Aufnahmezeit', 'Place': 'Ort', 'Where it was taken': 'Wo es aufgenommen wurde', 'Details': 'Details', 'Photo row': 'Fotozeile',
 'person': 'Person', 'persons': 'Personen', 'people': 'Personen',
})

def tr(text):
    if text in DE: return DE[text]
    raise SystemExit(f"no German for: {text!r}")

src = pathlib.Path("apps/tl/photos/locales/en-US.ftl")
lines = src.read_text().splitlines()
out = []
for line in lines:
    m = re.match(r"^([A-Za-z0-9_.-]+) = (.*)$", line)
    if m:
        out.append(f"{m.group(1)} = {tr(m.group(2))}"); continue
    m = re.match(r"^(\s+)\.([A-Za-z0-9_-]+) = (.*)$", line)
    if m and m.group(2) == "noun":
        # The plural block: its cases read German below.
        out.append(line); continue
    if m:
        out.append(f"{m.group(1)}.{m.group(2)} = {tr(m.group(3))}"); continue
    m = re.match(r"^(\s+)\[one\] (.*)$", line)
    if m:
        out.append(f"{m.group(1)}[one] {tr(m.group(2))}"); continue
    m = re.match(r"^(\s+)\*\[other\] (.*)$", line)
    if m:
        out.append(f"{m.group(1)}*[other] {tr(m.group(2))}"); continue
    out.append(line)
pathlib.Path("apps/tl/photos/locales/de-DE.ftl").write_text("\n".join(out) + "\n")
print("wrote de-DE.ftl", len(out), "lines")
