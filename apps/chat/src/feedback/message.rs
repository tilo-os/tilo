//! A filed report as its channel shows it: a message by whoever filed it,
//! read out from the report's structured entries in the reader's language
//! ([`ui::feedback_read_out`]),
//! with its masked picture as the message's image and the reactions people
//! answered it with. Nothing of this is stored: the record keeps the
//! reviewed submission, and this is how it reads.

use tilo_document::ClipBlock;
use ui::tilo_resource::Id;

use crate::record::FeedbackReport;

/// `report` as the message its channel shows, `None` for a report whose
/// submission is missing.
pub(crate) fn message(report: &FeedbackReport) -> Option<crate::record::Message> {
    let submission = report.submission.as_ref()?;
    let feedback = &submission.feedback;
    let lines = ui::feedback_read_out::lines(feedback);
    let body = lines
        .iter()
        .map(|line| ClipBlock {
            kind: "paragraph".into(),
            text: line.clone(),
            marks: Vec::new(),
            level: None,
            ordered: None,
            align: None,
            attrs: std::collections::BTreeMap::new(),
        })
        .collect();
    let attachments = feedback
        .screenshot
        .as_ref()
        .map(|picture| tilo_reference::Attached::Image {
            name: ui::feedback_read_out::PICTURE_NAME.into(),
            blob: picture.hash.clone(),
            w: picture.width,
            h: picture.height,
        })
        .filter(crate::record::valid_attached)
        .into_iter()
        .collect();
    Some(crate::record::Message {
        channel: Id::from_raw(submission.destination.channel.clone()),
        author: report.author.clone(),
        ts: report.ts,
        text: lines.join("\n"),
        body,
        reactions: report.reactions.clone(),
        attachments,
        ..crate::record::Message::default()
    })
}

#[cfg(test)]
mod tests;
