//! Tube's shared library: likes, the shared watch queue, subscriptions, and comment likes
//! and replies.

use tilo_resource::{
    self, ActionCx, Collection, Edit, Id, Model, NonBlank, Path, Refusal, Set, resource,
};

/// One reply to a comment, visible to everyone sharing the library.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct Reply {
    /// Comment
    #[model(readonly)]
    pub comment: String,
    /// Reply
    #[model(label, readonly)]
    pub text: String,
}

/// The library that syncs between everyone who shares it.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Library {
    /// Liked videos
    pub likes: Set<String>,
    /// Shared watch queue
    pub queue: Set<String>,
    /// Subscribed creators
    pub subscriptions: Set<String>,
    /// Liked comments
    pub comment_likes: Set<String>,
    /// Replies to comments
    pub replies: Collection<Reply>,
}

impl Library {
    /// What the record holds: the likes, the shared watch queue, the subscriptions, and the
    /// comment likes and replies.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<tilo_resource::__private::serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "tube.library",
    read_content = Library::read_document
)]
impl Library {
    /// Reply to a comment.
    ///
    /// Visible to everyone sharing this library.
    #[action]
    pub fn reply_to_comment(
        &mut self,
        cx: &mut ActionCx,
        #[param("The comment's id: its row id after `tube.comment:`")] comment: NonBlank,
        #[param("The reply")] text: NonBlank,
    ) -> Result<(), Refusal> {
        if self
            .replies
            .values()
            .any(|reply| reply.comment == comment.as_str() && reply.text == text.as_str())
        {
            return Ok(());
        }
        self.replies.push(
            cx.id(),
            Reply {
                comment: comment.into_string(),
                text: text.into_string(),
            },
        );
        Ok(())
    }

    /// Retract a reply.
    #[action]
    pub fn retract(&mut self, reply: &Id) -> Result<(), Refusal> {
        self.replies
            .remove(reply)
            .map(drop)
            .ok_or_else(|| Refusal::new("no such reply"))
    }
}

impl Library {
    /// The edit that likes `video` or takes the like back.
    pub fn like(video: &str, liked: bool) -> Edit<Self> {
        Self::member(&Self::paths().likes, video, liked)
    }

    /// The edit that adds `video` to the shared queue or takes it out.
    pub fn enqueue(video: &str, queued: bool) -> Edit<Self> {
        Self::member(&Self::paths().queue, video, queued)
    }

    /// The edit that subscribes to the creator stored as `key`, or unsubscribes it.
    pub fn subscribe(key: &str, subscribed: bool) -> Edit<Self> {
        Self::member(&Self::paths().subscriptions, key, subscribed)
    }

    /// The edit that likes the comment `comment` or takes the like back.
    pub fn like_comment(comment: &str, liked: bool) -> Edit<Self> {
        Self::member(&Self::paths().comment_likes, comment, liked)
    }

    /// The edit that puts `value` in the set at `set`, or takes it out.
    fn member(set: &Path<Self, Set<String>>, value: &str, member: bool) -> Edit<Self> {
        if member {
            set.insert(&value.to_owned())
        } else {
            set.remove(&value.to_owned())
        }
    }
}
