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
