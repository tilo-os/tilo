# PDF Viewer — English (source locale).

pdf-viewer-history-scope = PDF and its markup

## History: what each change did, by field and by verb

pdf-page-occurrence-rotation = Turned a page to { $new } degrees
pdf-stored-annotation-page-id = Moved markup to another page
pdf-stored-annotation-mark = Reshaped markup
pdf-stored-annotation-color = Recoloured markup
pdf-stored-annotation-space = Moved markup
pdf-stored-comment-page-id = Moved { $item } to another page
pdf-stored-comment-text = Edited the comment { $item }
pdf-stored-comment-rich = Formatted the comment { $item }
pdf-stored-comment-resolved = { $new ->
    [true] Resolved { $item }
   *[false] Reopened { $item }
}
pdf-stored-comment-replies = Changed the replies to { $item }
pdf-stored-reply-text = Edited the reply { $item }
pdf-stored-reply-rich = Formatted the reply { $item }
pdf-stored-signature-page-id = Moved a signature to another page
pdf-stored-signature-rect = Moved a signature
pdf-stored-signature-space = Moved a signature
pdf-pdf-markup-pages = Changed the pages
pdf-pdf-markup-annotations = Changed the markup
pdf-pdf-markup-comments = Changed the comments
pdf-pdf-markup-signatures = Changed the signatures
pdf-pdf-markup-form-values = Filled in the form
pdf-start-page-plan = Laid out the pages
pdf-pin-source = Pinned the PDF the markup is made on
pdf-adopt-existing = Took over the markup already in the PDF
pdf-import-pages = Imported pages
pdf-duplicate-page = Duplicated a page
pdf-delete-page = Deleted a page
pdf-move-page = Moved a page
pdf-rotate-page = Turned a page
pdf-add-annotation = Marked up a page
pdf-delete-annotation = Deleted markup
pdf-create-note = Added a note
pdf-add-note = Added the note “{ $text }”
pdf-add-comment = Commented
pdf-reply = Replied “{ $text }” to { $item }
pdf-delete-comment = Deleted the comment { $item }
pdf-add-signature = Signed
pdf-add-typed-signature = Signed with a typed name
pdf-delete-signature = Removed a signature

## Status notes: what happened, and what to do next when there is something

pdf-note-opening = Opening the PDF…
pdf-note-open-failed = { $name } couldn't be opened. Check that it is a PDF and try again.
pdf-note-open-in-new-window = This window shows { $shown } and its markup. Open { $name } in a new PDF Viewer window.
pdf-note-open-in-new-window-unnamed = This window keeps its document's markup. Open { $name } in a new PDF Viewer window.
pdf-note-source-changed = This file changed after it was marked up. Check that your markup still sits where you left it.
pdf-note-existing-markup-unread = The markup already in this PDF couldn't be read. You can still add your own.
pdf-note-pages-unavailable = Pages can't be shown right now. Open the PDF again to try once more.
pdf-note-page-not-shown = Page { $page } couldn't be shown. Open the PDF again to try once more.
pdf-note-text-unavailable = Text in this PDF can't be read right now. Open the PDF again to try once more.
pdf-note-page-text-unread = Text on page { $page } can't be read, so it can't be found, selected or copied.
pdf-note-no-text = This PDF is pictures of pages, so its text can't be found, selected or copied.
pdf-note-pages-missing = Some pages aren't on this device yet. They come in once someone who has them is online.
pdf-note-pages-not-arranged = The pages couldn't be put in the order the others left them. Open the PDF again to try once more.

pdf-note-save-no-document = Open a PDF first, then save a copy with your markup.
pdf-note-save-waiting-pages = The pages are still being put in order. Save again in a moment.
pdf-note-saving = Saving a copy with your markup as { $name }…
pdf-note-saved = Saved a copy with your markup as { $name }.
pdf-note-save-failed = { $name } wasn't saved. Try another folder or name.
pdf-note-save-picker-failed = The save window couldn't open. Try saving again.

pdf-note-import-no-document = Open a PDF first, then import pages into it.
pdf-note-import-busy = Pages are already being imported. Import more once they're in.
pdf-note-import-unavailable = Pages can't be imported right now. Open the PDF again, then import.
pdf-note-import-picker-failed = The file window couldn't open. Try importing again.
pdf-note-importing = Importing pages…
pdf-note-imported = { $count ->
    [one] Added 1 page. Save a copy to keep a PDF with it.
   *[other] Added { $count } pages. Save a copy to keep a PDF with them.
}
pdf-note-import-failed = Those pages couldn't be imported. Choose a PDF or an image and try again.
pdf-note-imported-arriving = Imported pages are on their way from someone in this room.
pdf-note-imported-waiting = Imported pages come in once someone who has them is online.
pdf-note-imported-unavailable = Imported pages aren't on this device. They come in once someone who has them is online.
pdf-note-imported-unshown = Imported pages can't be shown on this device.

pdf-note-copying = Copying…
pdf-note-copied-text = { $count ->
    [one] Copied 1 character
   *[other] Copied { $count } characters
}
pdf-note-copying-area = Copying the selected area…
pdf-note-copied-area = Copied the selected area as a picture
pdf-note-area-not-copied = The selected area couldn't be copied. Try again.

pdf-note-find-result-gone = That match is no longer in the document. Search again to see what's there now.
pdf-note-item-gone = That's no longer in this document.
pdf-note-field-read-only = “{ $field }” is locked and can't be filled in.

pdf-note-signature-draw-first = Draw your signature first, then click a page.
pdf-note-signature-name-first = Type your name under Signatures, then click a page.
pdf-note-signature-not-kept = Your signature wasn't kept for next time. Change it again to keep it.

pdf-note-link-page-gone = The page this link points to is no longer in the document.
pdf-note-link-opening = Opening { $address }
pdf-note-link-failed = { $address } couldn't be opened. Try the link again.
pdf-note-link-unsupported = Links like { $address } aren't opened from a PDF.
pdf-note-link-file = Links to other files aren't opened from a PDF. Open { $path } from Files instead.
