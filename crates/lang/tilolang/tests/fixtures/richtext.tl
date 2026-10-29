app so.tilo.lang.richtext "Board" (icon: .message_circle, category: .network)

/// Something to do, which messages refer to.
type Task {
    title: Text (title)
    done: Bool = false
}

/// A message: rich text that can refer to people and tasks.
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

row view task_row for Task

/// Adds a task.
action add_task(b: Board, title: Text) -> Task {
    b.tasks.insert(Task(title: title))
}

/// Renames a task.
action rename(t: Task, title: Text) {
    t.title = title
}

/// Posts a message, when it says something.
action post(b: Board, body: RichText) {
    if not body.is_empty() {
        b.posts.insert(Post(body: body))
    }
}

/// How many posts the board holds.
action post_count(b: Board) -> Int {
    b.posts.len()
}

/// Posts plain text.
action say(b: Board, text: Text) -> Post {
    b.posts.insert(Post(body: RichText(text)))
}

/// Posts a message saying who will look at a task: the invoker.
action take(b: Board, t: Task) -> Post {
    b.posts.insert(Post(body: RichText(invoker, " will look at ", t, ".")))
}

/// Changes what a post says.
action edit(p: Post, body: RichText) {
    p.body = body
}

/// Adds a line after what a post says, keeping it.
action append(p: Post, line: Text) {
    p.body = RichText(p.body, "\n", line)
}

/// Adds a reference to a task at the end of what a post says.
action refer(p: Post, t: Task) {
    p.body = RichText(p.body, " see ", t)
}

/// What a post says in a line.
action excerpt_of(p: Post) -> Text {
    p.body.excerpt
}

/// How many tasks a post refers to.
action task_count(p: Post) -> Int {
    p.body.references.of(Task).len()
}

/// How many people a post mentions.
action people_count(p: Post) -> Int {
    p.body.people.len()
}

/// The board: its posts.
view board(b: Board) {
    std.list(b, b.posts, empty: "No posts yet")
}

/// A post: what it says, each reference as the reader's inline view of it.
view post_view(p: Post) {
    rich_text(p.body)
}

/// A post in a list: its first line.
view post_row(p: Post) {
    row(p.body.excerpt)
}

/// A task in a list: its title.
view task_row(t: Task) {
    row(t.title)
}
