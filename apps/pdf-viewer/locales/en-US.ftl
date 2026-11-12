# PDF Viewer — English (source locale).

pdf-viewer-history-scope = PDF and its markup

## History: what each change did, by field and by verb

pdf-viewer-page-occurrence-rotation = Turned a page to { $new } degrees
pdf-viewer-annotation-page-id = Moved markup to another page
pdf-viewer-annotation-mark = Reshaped markup
pdf-viewer-annotation-color = Recoloured markup
pdf-viewer-annotation-space = Moved markup
pdf-viewer-pdf-anchor-page-id = Moved a comment to another page
pdf-viewer-comment-text = Edited the comment { $item }
pdf-viewer-comment-body = Formatted the comment { $item }
pdf-viewer-comment-resolved = { $new ->
    [true] Resolved { $item }
   *[false] Reopened { $item }
}
pdf-viewer-comment-replies = Changed the replies to { $item }
pdf-viewer-reply-text = Edited the reply { $item }
pdf-viewer-reply-body = Formatted the reply { $item }
pdf-viewer-stored-signature-page-id = Moved a signature to another page
pdf-viewer-stored-signature-rect = Moved a signature
pdf-viewer-stored-signature-space = Moved a signature
pdf-viewer-pdf-markup-pages = Changed the pages
pdf-viewer-pdf-markup-annotations = Changed the markup
pdf-viewer-pdf-markup-comments = Changed the comments
pdf-viewer-pdf-markup-signatures = Changed the signatures
pdf-viewer-pdf-markup-form-values = Filled in the form
pdf-viewer-start-page-plan = Laid out the pages
pdf-viewer-pin-source = Pinned the PDF the markup is made on
pdf-viewer-adopt-existing = Took over the markup already in the PDF
pdf-viewer-import-pages = Imported pages
pdf-viewer-duplicate-page = Duplicated a page
pdf-viewer-delete-page = Deleted a page
pdf-viewer-move-page = Moved a page
pdf-viewer-rotate-page = Turned a page
pdf-viewer-add-annotation = Marked up a page
pdf-viewer-delete-annotation = Deleted markup
pdf-viewer-create-note = Added a note
pdf-viewer-add-note = Added the note “{ $text }”
pdf-viewer-add-comment = Commented
pdf-viewer-reply = Replied “{ $text }” to { $item }
pdf-viewer-delete-comment = Deleted the comment { $item }
pdf-viewer-add-signature = Signed
pdf-viewer-add-typed-signature = Signed with a typed name
pdf-viewer-delete-signature = Removed a signature

## Status notes: what happened, and what to do next when there is something

pdf-note-opening = Opening the PDF…
pdf-note-open-failed = { $name } couldn't be opened. Check that it is a PDF and try again.
pdf-note-document-not-opened = { $name } in this workspace didn't open. It may not be on this device yet; it opens once a device that has it is online.
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
pdf-note-imported-arriving = Imported pages are on their way from someone in this workspace.
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

## The empty window: what it says before a document is open, and why an open
## failed. Painted on the page and read out alike.

pdf-empty-heading = No document open
pdf-empty-opening = Opening PDF…
pdf-empty-open = Open PDF
pdf-empty-open-another = Open another PDF
pdf-empty-open-failed = Couldn't open this document
pdf-empty-detail-password = This PDF is password-protected, so it can't be opened.
pdf-empty-detail-picker = The file window couldn't open. Try opening the file again.
pdf-empty-detail-engine = The reader can't open PDFs on this device right now. Try again.
pdf-empty-detail-unreadable = The file is unavailable or isn't a readable PDF.
pdf-empty-detail-other = Check the file and try again.
pdf-empty-waiting-title = Waiting for the document
pdf-empty-waiting = { $name } opens when a device that has it connects.
pdf-empty-elsewhere-title = This document isn't here
pdf-empty-elsewhere = No device connected now has { $name }. It opens when one that has it does.
pdf-empty-arriving-title = Getting the document…
pdf-empty-arriving = { $name } is coming from a device in this workspace that has it.
pdf-empty-the-document = The document

## The review panel's tabs: one name per tab, the same at every width.

pdf-tab-comments = Comments
pdf-tab-contents = Contents
pdf-tab-signing = Signing

## The review panel: the comments a document's readers left, and the
## signatures they placed on it.

pdf-comments-empty-title = No comments yet
pdf-comments-empty-message = Select text to comment on it, or add a note with the Note tool.
pdf-comments-one-open = 1 comment, open
pdf-comments-one-resolved = 1 comment, resolved
pdf-comments-all-resolved = { $total } comments, all resolved
pdf-comments-open = { $total } comments, { $open } open
pdf-comment-reply = Reply
pdf-comment-reply-placeholder = Reply…
pdf-comment-resolve = Resolve
pdf-comment-reopen = Reopen
pdf-comment-edit = Edit comment
pdf-comment-delete = Delete comment
pdf-viewer-comment-placeholder = Write a comment…
pdf-comment-empty = Empty comment
pdf-author-you = You
pdf-author-someone = Someone
pdf-contents-empty = This document has no contents

pdf-viewer-signature-your = Your signature
pdf-viewer-signing-as = Signing as
pdf-signature-your-name = Your name
pdf-viewer-signature-placed = Placed
pdf-viewer-signature-placed-note = No signature is placed yet.
pdf-signature-type = Type
pdf-signature-draw = Draw
pdf-signature-draw-hint = Draw your signature here
pdf-signature-clear = Clear
pdf-signature-place = Place on page
pdf-signature-cancel = Cancel
pdf-signature-place-hint = Click on a page to place your signature.
pdf-signature-delete = Delete signature
pdf-signature-annotation = Signed by { $signer } on { $date }
pdf-signature-unnamed = Signature

## Imported pages: a page still on its way, said on the page itself.

pdf-imported-page-loading = Imported page is loading…
pdf-imported-page-waiting = The imported page arrives once someone who has it is online.
pdf-imported-page-unavailable = Imported page is unavailable

## The page and the zoom

pdf-page-line = Page { $page } of { $pages }, { $zoom }%
pdf-rail-import = Import…

## The window's bar, its tool and colour menus

pdf-bar-hide-review = Hide review panel
pdf-bar-show-review = Show review panel
pdf-bar-zoom-in = Zoom in
pdf-bar-zoom-out = Zoom out
pdf-bar-fit-page = Fit page
pdf-bar-fit-width = Fit width
pdf-bar-tools = Tools
pdf-bar-marker-colour = Marker colour
pdf-bar-zoom = Zoom
pdf-bar-previous-page = Previous page
pdf-bar-next-page = Next page
pdf-bar-save = Save annotated PDF
pdf-tool-select = Select
pdf-tool-highlight = Highlight
pdf-tool-draw = Draw
pdf-tool-note = Note
pdf-tool-text = Text
pdf-tool-sign = Sign
pdf-colour-yellow = Yellow
pdf-colour-green = Green
pdf-colour-pink = Pink
pdf-colour-blue = Blue
pdf-colour-purple = Purple

## The file windows the system shows for opening, importing and saving

pdf-chooser-open = Open a PDF
pdf-chooser-open-accept = Open
pdf-chooser-filter-pdf = PDF documents
pdf-chooser-import = Import pages from a PDF or image
pdf-chooser-import-accept = Import
pdf-chooser-filter-import = PDF or image
pdf-chooser-save-accept = Save

## Other commands and outcomes

pdf-delete = Delete
pdf-try-again = Try again

## What the app offers to the desk's search

pdf-search-open = Open PDF document
pdf-search-bookmark = Bookmark: { $title }
pdf-search-contents = Contents on page { $page }
pdf-search-result = Find result for “{ $query }”
pdf-search-annotation = { $kind } on page { $page }
pdf-search-comment = Comment by { $author }
pdf-search-signed = Page { $page } signed on { $date }
pdf-search-pages = { $count ->
    [one] 1 page
   *[other] { $count } pages
}
pdf-search-no-pages = No pages yet

## What a marking on a page is called

pdf-annotation-highlight = Highlight
pdf-annotation-underline = Underline
pdf-annotation-strikeout = Strikethrough
pdf-annotation-squiggly = Squiggly underline
pdf-annotation-drawing = Drawing
pdf-annotation-note = Note
pdf-annotation-text = Text box

## What the window and its parts are called for assistive technology

pdf-title = PDF Viewer
pdf-a11y-pages = Pages
pdf-a11y-page-markup = Contains review markup
pdf-a11y-canvas = PDF page canvas. Arrow keys scroll; Page Up and Page Down change pages.
pdf-a11y-search-results = Search results
pdf-a11y-tabs = Tabs
pdf-a11y-review-sidebar = Review sidebar
pdf-a11y-panel-resize = Review panel size
pdf-a11y-document = Document
pdf-a11y-zoom = Zoom { $zoom }%
pdf-a11y-page = Page { $page }
pdf-a11y-page-of = Page { $current } of { $pages }
pdf-a11y-match = Match { $match } on page { $page }
pdf-a11y-comment = { $author } on page { $page }; { $replies ->
    [one] one reply
   *[other] { $replies } replies
}
pdf-a11y-signature-by = Signature by { $signer }
pdf-a11y-signed = Signed { $date } on page { $page }
pdf-drawn-signed = Signed { $date }
