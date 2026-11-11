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
hex-editor-keyboard-help = Keyboard: Ctrl+O open, Ctrl+S save, Ctrl+F find, Ctrl+H replace, Ctrl+G go to offset, Ctrl+C copy, Ctrl+X cut, Ctrl+V paste, Ctrl+A select all, Ctrl+I show or hide the inspector, Ctrl+Z undo, Ctrl+Shift+Z redo, Tab switches hex and ASCII, Insert switches overwrite and insert.
hex-editor-open-file = Open file
hex-editor-open-file-detail = Choose a local file to inspect
hex-editor-new-file = New empty file
hex-editor-new-file-detail = Start an empty buffer to type bytes into
hex-editor-window-name = Hex editor
hex-editor-window-detail = The bytes of one file, edited byte by byte.

## Window and accessibility copy
hex-editor-inspector = Inspector
