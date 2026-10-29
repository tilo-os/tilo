app so.tilo.lang.mentions "Mentions" (icon: .message_circle, category: .network)

/// Something to do, which posts refer to.
type Task {
    title: Text (title)
    done: Bool = false
}

/// A post: rich text that refers to people and tasks, and says when.
type Post {
    body: RichText (body)
}

/// The board: its tasks and the posts about them.
type Board {
    tasks: Collection<Task>
    posts: Collection<Post>
}

root Board

default view board for Board

default view post_view for Post

row view post_row for Post

/// The board: its posts.
view board(b: Board) {
    std.list(b, b.posts, empty: "No posts yet")
}

/// A post: what it says, each reference as the reader's inline view of it.
view post_view(p: Post) {
    std.document(p) {
        rich_text(p.body)
    }
}

/// A post in a list: its first line.
view post_row(p: Post) {
    row(p.body.excerpt)
}

/// A post referring to a person, a task, the time of a meeting, and a task the reader no
/// longer holds.
scenario "mentions" -> Post {
    let board = root Board
    let grace = person("Grace")
    let fix = board.tasks.insert(Task(title: "Fix the release build"))
    let gone = board.tasks.insert(Task(title: "Acquisition plan"))
    let p = board.posts.insert(Post(body: RichText(grace, " will look at ", fix, " before the standup at ", 2026-10-29T09:30Z, ". The notes are in ", gone, ".")))
    board.tasks.remove(gone)
    p
}
