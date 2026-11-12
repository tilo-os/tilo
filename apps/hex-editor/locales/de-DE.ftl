# Verlauf: was jede Änderung am Puffer getan hat.
hex-editor-hex-editor-bytes = { $op ->
    [insert] { $count ->
        [one] Ein Byte
       *[other] { $count } Bytes
    } bei Offset { $offset } eingefügt
    [remove] { $count ->
        [one] Ein Byte
       *[other] { $count } Bytes
    } bei Offset { $offset } gelöscht
    [set] { $count ->
        [one] Ein Byte
       *[other] { $count } Bytes
    } bei Offset { $offset } überschrieben
   *[other] Bytes bearbeitet
}
hex-editor-load = { $name } geöffnet
hex-editor-insert-bytes = Bytes bei Offset { $offset } eingefügt
hex-editor-overwrite-bytes = Bytes bei Offset { $offset } überschrieben
hex-editor-delete-bytes = { $len ->
    [one] Ein Byte
   *[other] { $len } Bytes
} bei Offset { $offset } gelöscht
hex-editor-keyboard-help = Tastatur: Strg+O öffnen, Strg+S speichern, Strg+F suchen, Strg+H ersetzen, Strg+G zum Offset springen, Strg+C kopieren, Strg+X ausschneiden, Strg+V einfügen, Strg+A alles auswählen, Strg+I Inspector zeigen oder verbergen, Strg+Z rückgängig, Strg+Umschalt+Z wiederholen, Tab wechselt Hex und ASCII, Einfg wechselt Überschreiben und Einfügen.
hex-editor-open-file = Datei öffnen
hex-editor-open-file-detail = Eine lokale Datei zum Ansehen wählen
hex-editor-new-file = Neue leere Datei
hex-editor-new-file-detail = Einen leeren Puffer zum Eintippen von Bytes beginnen
hex-editor-window-name = Hex-Editor
hex-editor-window-detail = Die Bytes einer Datei, Byte für Byte bearbeitet.

## Fenster- und Barrierefreiheits-Texte
hex-editor-inspector = Inspektor

## Fakten, Eigenschaftsnamen und Bereichstitel
hex-a11y-status = { $file }; { $bytes ->
    [one] ein Byte
   *[other] { $bytes } Byte
}; Cursor bei Hexadezimal { $cursor }; Modus { $mode }; { $endian }
hex-a11y-no-file = Keine Datei geöffnet. Öffne eine lokale Datei, um ihre Bytes zu prüfen.
hex-offset-readout = Offset 0x{ $offset } ({ $cursor })
hex-editor-empty-hint = Untersuche und bearbeite jede Datei Byte für Byte, oder starte eine leere und tippe Bytes ein.
