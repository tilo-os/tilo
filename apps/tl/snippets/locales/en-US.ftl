# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-snippets = Snippets

d-so-tilo-snippets-keywords = snippets, snippet, textbausteine, vorlagen, templates, code, clipboard, zwischenablage, boilerplate, schnipsel

d-jzur3ob5 = Snippet
    .doc = One reusable piece of text or code.
    .noun = { $count ->
        [one] snippet
       *[other] snippets
    }
    .untitled = Untitled snippet

d-f7zljrnp = Title

d-5k3zya5m = Body
    .doc = What is copied.

d-wziqkkiz = Description
    .doc = What the snippet is for, shown above its body.

d-rorcz6y2 = Favorite
    .doc = Starred: starred snippets stand at the top of every place.

d-b5ooyg4x = Folder
    .doc = The folder it is filed under; empty for none, which every place but its own lists.

d-26cc5hue = Language

d-te2jwa3o = Tags
    .doc = What it is about, lower case and without `#`.

d-kkfkhxfb = Uses
    .doc = How often it was copied: what "Most used" reads.

d-jzur3ob5-filter-favorites = Favorites

d-jzur3ob5-filter-tags = Tags

d-jzur3ob5-filter-language = Language

d-4eiv5c3p = Fill value
    .doc = One variable a person is filling in, and what they typed for it.
    .noun = { $count ->
        [one] fill value
       *[other] fill values
    }

d-u5ym5ttn = Name

d-h5vss6x5 = Value

d-fylf2syc = Fill
    .doc = A person's filling of one snippet's variables.
    .noun = { $count ->
        [one] fill
       *[other] fills
    }

d-qxvejddr = Snippet
    .doc = The snippet being filled; none while nothing is being filled.

d-e6uc6jtt = Values

d-fylf2syc-filter-snippet = Snippet

d-3x4ryb7i = Library
    .doc = The workspace's library: every snippet it keeps.
    .noun = { $count ->
        [one] library
       *[other] libraries
    }

d-g5k73yqh = Snippets

d-avqtjv3w = Fills
    .doc = Each person's filling here.

d-sxr42pgj = Language
    .doc = The kind of text a snippet is written in: its badge in the list and the colours its body is shown with.

d-clowkhig = Anything that is only words
    .doc = Anything that is only words.

d-4tycb6j2 = Shell

d-w4pekszm = Rust

d-dpv33ulb = Sql

d-x6c6cuyb = Git

d-zmb7xzvo = Python

d-56xzlfdj = Javascript

d-ch7z3egw = Json

d-zfvuyfaq = Yaml

d-aiadtekw = Markdown

d-a4yonxcw = Html

d-xxhzlytk = Css

d-pmoafk6y = Toml

d-sewa2633 = Dockerfile

d-mm53574g = New snippet
    .doc = Makes a snippet, filed under `folder` when it names one.
    .did = made a snippet
    .w-a-snippet-needs-a-title = A snippet needs a title

d-mm53574g-title = Title

d-mm53574g-folder = Folder

d-yne2tfly = Duplicate
    .doc = Makes a second copy of a snippet, right after it; its word count starts again from nothing.
    .did = duplicated a snippet

d-yne2tfly-snippet = Snippet

d-yne2tfly-title = Title

d-hqhhicru = Delete
    .doc = Deletes a snippet from the library.
    .did = deleted a snippet

d-hqhhicru-snippet = Snippet

d-qogfdws3 = Remove star
    .doc = Stars a snippet, or takes the star back.
    .w-remove-star = Remove star
    .w-star-it = Star it

d-qogfdws3-starred = Starred

d-nh66jgh3 = Edit tags…
    .doc = Gives a snippet the tags a person typed, comma-separated: each trimmed, lower case, without `#`, at most 24 of them.
    .did = tagged a snippet

d-nh66jgh3-tags = Tags

d-afvqqrn3 = Remove the tag
    .doc = Takes one tag off one snippet.

d-afvqqrn3-tag = Tag

d-6bqnklse = Rename folder
    .doc = Renames a folder everywhere it is used; an empty new name files its snippets under nothing.
    .did = renamed a folder
    .w-there-is-no-such-folder = There is no such folder

d-6bqnklse-folder = Folder

d-6bqnklse-to = To

d-jmxghpuf = Rename tag
    .doc = Renames a tag everywhere it is used.
    .did = renamed a tag
    .w-there-is-no-such-tag = There is no such tag
    .w-a-tag-needs-a-name = A tag needs a name

d-jmxghpuf-tag = Tag

d-jmxghpuf-to = To

d-o5bdt7vd = Remove tag
    .doc = Takes a tag off every snippet that carries it.
    .did = removed a tag
    .w-there-is-no-such-tag = There is no such tag

d-o5bdt7vd-tag = Tag

d-3lo37mfx = Copy
    .doc = Copies a snippet's body to the clipboard, and counts the use.
    .did = copied a snippet

d-4k5pl7yr = Fill in and copy
    .doc = Starts filling in a snippet's variables: one field for each variable its body holds, emptied.

d-4k5pl7yr-snippet = Snippet

d-rjqbun77 = Copy
    .doc = Copies the filled body, counts the use, and puts the filling away.
    .did = copied a snippet

d-rjqbun77-fill = Fill

d-lnul5ski = Cancel
    .doc = Puts the filling away without copying.

d-gptgbn5d = Clear fill
    .doc = Empties a filling, so the sheet closes.

d-xiimohm2 = New snippet~present

d-jysqa4ij = Star~present

d-4tqyv7to = Rename folder~present

d-idijlyp2 = Rename tag~present

d-3tbwsppb = Remove tag~present

d-2jh6hotv = Snippet row

d-xqxtrmmb = Snippet page
    .w-remove-star = Remove star
    .w-star-it = Star it
    .w-duplicate = Duplicate
    .w-delete = Delete
    .w-untitled-snippet = Untitled snippet
    .w-language = Language
    .w-no-folder = No folder
    .w-edit-tags = Edit tags…
    .w-what-it-is-for = What it is for
    .w-fill-in-and-copy = Fill in and copy
    .w-copy = Copy

d-lqt75wck = Fill sheet
    .w-fill-in-and-copy = Fill in and copy
    .w-what-each-variable-stands-for = What each variable stands for
    .w-copy = Copy
    .w-cancel = Cancel

d-aaruymww = Snippets window
    .w-snippets = Snippets
    .w-all-snippets = All snippets
    .w-favorites = Favorites
    .w-most-used = Most used
    .w-folders = Folders
    .w-tags = Tags
    .w-keep-the-text-and-code-you = Keep the text and code you paste often, with its own folders and tags.
    .w-nothing-here-yet = Nothing here yet.
    .w-new-snippet = New snippet
    .w-no-snippets-yet = No snippets yet
    .w-choose-a-snippet = Choose a snippet
