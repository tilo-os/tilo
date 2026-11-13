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

## Facts, property labels and section titles
hex-a11y-status = { $file }; { $bytes ->
    [one] one byte
   *[other] { $bytes } bytes
}; cursor at hexadecimal { $cursor }; { $mode } mode; { $endian }
hex-a11y-no-file = No file open. Open a local file to inspect its bytes.
hex-offset-readout = Offset 0x{ $offset } ({ $cursor })
hex-editor-empty-hint = Inspect and edit any file byte by byte, or start an empty one and type bytes in.

## The window bar's slots
hex-editor-edit-mode = Edit mode
hex-editor-hide-inspector = Hide inspector
hex-editor-show-inspector = Show inspector

## Actions

hex-editor-effects-save = Save file
hex-editor-effects-save-as = Save as
hex-editor-effects-search = Search bytes
hex-editor-effects-goto-offset = Go to offset
hex-editor-document = Hex document
hex-editor-document-load = Load file
hex-editor-document-insert-bytes = Insert bytes
hex-editor-document-overwrite-bytes = Replace bytes
hex-editor-document-delete-bytes = Delete bytes

## The endianness strip
hex-editor-little-endian = Little endian
hex-editor-big-endian = Big endian

## What the app is called in a window and in the launcher
hex-editor-app-title = Hex Editor
