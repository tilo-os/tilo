# History: what each change to the buffer did.
hex-editor-hex-editor-bytes = { $op ->
    [insert] Inserted { $count ->
        [one] one byte
       *[other] { $count } bytes
    } at offset { $offset }
    [remove] Deleted { $count ->
        [one] one byte
       *[other] { $count } bytes
    } at offset { $offset }
    [set] Overwrote { $count ->
        [one] one byte
       *[other] { $count } bytes
    } at offset { $offset }
   *[other] Edited the bytes
}
hex-editor-load = Opened { $name }
hex-editor-insert-bytes = Inserted bytes at offset { $offset }
hex-editor-overwrite-bytes = Overwrote bytes at offset { $offset }
hex-editor-delete-bytes = Deleted { $len ->
    [one] one byte
   *[other] { $len } bytes
} at offset { $offset }
