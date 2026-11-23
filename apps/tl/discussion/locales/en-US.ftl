# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-lang-discussion = Discussion

d-afjtpur7 = Origin
    .doc = Where it was first written, when it was moved here from another tracker or service: the person there, the service, where it reads there and its reference, so a task or a thread imported from elsewhere says where it came from.
    .noun = { $count ->
        [one] origin
       *[other] origins
    }

d-xxyuyumk = Author
    .doc = Who wrote it there, as they were named.

d-gmvkzzuz = Handle
    .doc = The name they were known by there, when it differs.

d-nfpru2aa = Source
    .doc = The service it came from.

d-wuizz4mi = Url
    .doc = Where it reads there.

d-semowww2 = Reference
    .doc = Its reference there, such as its number.

d-3wgbb6lz = Created
    .doc = When it was first written there.

d-2wr4psic = Updated
    .doc = When it last changed there.

d-afjtpur7-filter-created = Created

d-afjtpur7-filter-updated = Updated

d-kj3aantt = Comment
    .doc = What someone said about one resource: a task, a note, a mail, a day.
    .noun = { $count ->
        [one] comment
       *[other] comments
    }

d-5q4mbecw = Text

d-ng7oourf = Reply to
    .doc = The comment it answers, if it answers one: a reply stands under the remark it answers.

d-lz4a7tbk = Resolved
    .doc = Whether what was asked for is done: a remark taken care of folds away, and its author or anyone reading it may open it again.

d-c3oavkxa = By agent
    .doc = Whether an agent wrote it: its author is the agent, and a reader says so.

d-uiyftp3p = Origin
    .doc = Where it was first written, when it was moved here from another tracker.

d-f6337kvt = Step
    .doc = The step of an agent's run that wrote it: the action it called, or what it did.

d-hqpdw4ih = Lookup
    .doc = The resource the step looked up, when it read one.

d-tovzcpp5 = Incorporated
    .doc = Whether a later session's words folded it into the resource's note.

d-kj3aantt-filter-reply_to = Reply to

d-kj3aantt-filter-resolved = Resolved

d-kj3aantt-filter-by_agent = By agent

d-kj3aantt-filter-origin = Origin

d-kj3aantt-filter-incorporated = Incorporated

d-d6k3e7fz = Comments
    .doc = Every comment of the workspace, in the order they were made.
    .noun = { $count ->
        [one] comment
       *[other] comments
    }

d-3g3duquj = Items

d-l7s2czjp = Comment
    .doc = Says something about the resource the comment is made from: the thread under a task, a note or a mail says it with `.made_from(r)`, so the comment records what it is about.
    .did = commented
    .w-write-a-comment-first = Write a comment first

d-l7s2czjp-text = Text

d-l7s2czjp-origin = Origin

d-l7s2czjp-step = Step

d-l7s2czjp-lookup = Lookup

d-l7s2czjp-incorporated = Incorporated

d-iae4o5k3 = Edit
    .doc = Changes what a comment says.
    .w-only-its-author-can-edit-a = Only its author can edit a comment
    .w-write-a-comment-first = Write a comment first

d-iae4o5k3-text = Text

d-l7w5vz3v = Reply
    .doc = Answers a comment: the reply stands under the remark it answers, as a conversation's answer does, and what it is about is what the remark is about.
    .did = replied
    .w-write-a-reply-first = Write a reply first

d-l7w5vz3v-text = Text

d-5i4uizbb = Resolve
    .doc = Marks a comment as taken care of, or opens it again.
    .w-resolve = Resolve
    .w-open-again = Open again

d-5i4uizbb-resolved = Resolved

d-l3wyj42h = Delete
    .doc = Deletes a comment.
    .w-only-its-author-can-delete-a = Only its author can delete a comment

d-l3wyj42h-c = C

d-vzn4frig = Comment~present

d-4mzlhyvw = Edit~present

d-cr62rvlw = Reply~present

d-5fveadpi = Resolve~present

d-kflwfuct = Delete~present

d-upy33rnt = Thread
    .w-discussion = Discussion
    .w-edit-comment = Edit comment
    .w-cancel = Cancel
    .w-save = Save
    .w-edit = Edit
    .w-reply = Reply
    .w-open-again = Open again
    .w-resolve = Resolve
    .w-write-a-reply = Write a reply
    .w-send = Send
    .w-add-a-comment = Add a comment

d-nsv2jvpu = Written
    .w-edited = edited

d-vp5uwkal = Recently
    .w-nothing-said-yet = Nothing said yet

d-3666szuv = Comment page

d-jxt3set4 = Remark
