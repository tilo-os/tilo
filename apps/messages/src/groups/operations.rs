use super::{GroupsView, Page, bind};
use crate::view::{Choice, Selection};
use crate::{CATALOG, Messages, MessagesEffect, view::Sources};
use std::rc::Rc;
use tilo_messaging::{GroupChange, GroupChangeRequest, GroupSendRequest};
use tilo_messaging::{GroupMembership, ServiceAccount};
use ui::framework::{Actions, EffectAnswer, EffectCx, Effected};
use ui::widget::TextInput;
use ui::widget::{Button, Column, Text};

pub(super) fn handles(effect: &MessagesEffect) -> bool {
    matches!(
        effect,
        MessagesEffect::ReadGroupHistory { .. }
            | MessagesEffect::SendGroupText { .. }
            | MessagesEffect::AddGroupMember { .. }
            | MessagesEffect::RemoveGroupMember { .. }
            | MessagesEffect::LeaveGroup { .. }
    )
}
enum Operation {
    History,
    Send(String, String),
    Change(String, GroupChange),
}
struct Request {
    account: ServiceAccount,
    group: String,
    review: Option<String>,
    operation: Operation,
}
fn request(sources: &Sources, effect: MessagesEffect) -> Result<Request, String> {
    let (service, account, group, review, op) = match effect {
        MessagesEffect::ReadGroupHistory {
            service,
            account,
            group,
        } => (service, account, group, None, Operation::History),
        MessagesEffect::SendGroupText {
            service,
            account,
            group,
            review,
            operation,
            text,
        } => (
            service,
            account,
            group,
            Some(review),
            Operation::Send(operation, text),
        ),
        MessagesEffect::AddGroupMember {
            service,
            account,
            group,
            review,
            operation,
            person,
            choice,
        } => {
            let choice_value: Choice =
                serde_json::from_str(&choice).map_err(|_| CATALOG.t("messages-contact-missing"))?;
            let route = crate::history::current_route(
                sources,
                &Selection { person, choice },
                &choice_value,
            )?;
            (
                service,
                account,
                group,
                Some(review),
                Operation::Change(operation, GroupChange::Add(route)),
            )
        }
        MessagesEffect::RemoveGroupMember {
            service,
            account,
            group,
            review,
            operation,
            member,
        } => (
            service,
            account,
            group,
            Some(review),
            Operation::Change(operation, GroupChange::Remove(member)),
        ),
        MessagesEffect::LeaveGroup {
            service,
            account,
            group,
            review,
            operation,
        } => (
            service,
            account,
            group,
            Some(review),
            Operation::Change(operation, GroupChange::Leave),
        ),
        _ => return Err(CATALOG.t("messages-groups-unavailable")),
    };
    Ok(Request {
        account: ServiceAccount::new(service, account),
        group,
        review,
        operation: op,
    })
}
pub(super) fn effect(
    view: &GroupsView,
    sources: &Sources,
    effect: MessagesEffect,
    cx: &mut EffectCx<Messages>,
) -> EffectAnswer {
    let Request {
        account,
        group,
        review,
        operation: op,
    } = request(sources, effect)?;
    let selected = view.page.get();
    if !view.admitted(sources, &account) {
        view.close();
        return Err(CATALOG.t("messages-groups-unavailable"));
    }
    if !matches!(selected.as_deref(),Some(Page::Members(p)) if p.account==account && p.group.id==group && review.as_ref().is_none_or(|r|r==&p.review))
    {
        return Err(CATALOG.t("messages-groups-unavailable"));
    }
    let client = view
        .client
        .as_ref()
        .and_then(|c| c.groups(&account))
        .ok_or_else(|| CATALOG.t("messages-groups-unavailable"))?;
    let generation = view.key();
    let view = view.clone();
    let sources = sources.clone();
    // Operation IDs survive uncertain failures. Only a definite receipt clears
    // the draft; another click cannot silently replay a timed-out service write.
    let sent_text = match &op {
        Operation::Send(_, text) => Some(text.clone()),
        _ => None,
    };
    let sent_id = match &op {
        Operation::Send(id, _) | Operation::Change(id, _) => Some(id.clone()),
        Operation::History => None,
    };
    Ok(cx.spawn(move |_|async move {
        match op {
            Operation::History=>client.history(group).await.map(|p|(Some(p),None)),
            Operation::Send(operation,text)=>{client.send(GroupSendRequest{group:group.clone(),review:review.unwrap_or_default(),operation,text:text.into()}).await?; Ok((client.history(group.clone()).await.ok(),client.read(group).await.ok()))},
            Operation::Change(operation,change)=>{let leave=matches!(change,GroupChange::Leave);client.change(GroupChangeRequest{group:group.clone(),review:review.unwrap_or_default(),operation,change}).await?;Ok((None,if leave {None} else {client.read(group).await.ok()}))},
        }
    },move |(),result,_|{
        if view.key()!=generation {return Err(CATALOG.t("messages-groups-unavailable"));}
        if !view.admitted(&sources,&account) {view.close();return Err(CATALOG.t("messages-groups-unavailable"));}
        let (history,membership)=result.map_err(|e|CATALOG.t(match e {tilo_messaging::SendError::Uncertain=>"messages-send-uncertain",tilo_messaging::SendError::AlreadyReserved=>"messages-send-reserved",_=>"messages-groups-unavailable"}))?;
        if sent_text.is_none() && sent_id.is_some() && membership.is_none() {view.close();return Ok(Effected::done());}
        if let Some(text)=sent_text && view.draft.get()==text && sent_id.as_ref()==Some(&view.operation.get()) {view.draft.set(String::new());view.operation.set(uuid::Uuid::new_v4().to_string());}
        let value=history.as_ref().map(|p|serde_json::json!({"service":p.account.service,"account":p.account.account,"group":p.group,"messages":p.messages.iter().map(|r|serde_json::json!({"author":r.author,"id":r.message.id,"text":r.message.text.as_str(),"timestamp_ms":r.message.timestamp_ms,"outgoing":r.message.direction==tilo_messaging::Direction::Outgoing,"delivery":r.message.delivery.map(|d|match d {tilo_messaging::Delivery::Uncertain=>"uncertain",_=>"submitted"})})).collect::<Vec<_>>()}));
        if let Some(membership)=membership {view.page.set(Some(Rc::new(Page::Members(membership))));}
        view.history.set(history.map(Rc::new));view.generation.set(generation.saturating_add(1));
        Ok(value.map_or_else(Effected::done,Effected::value))
    }))
}
pub(super) fn panel(
    view: &GroupsView,
    sources: &Sources,
    actions: &Actions<Messages>,
    page: &GroupMembership,
    mut body: Column,
) -> Column {
    body = body.push(bind(
        actions,
        Button::new("messages.group.history", "messages-group-history"),
        &MessagesEffect::ReadGroupHistory {
            service: page.account.service.clone(),
            account: page.account.account.clone(),
            group: page.group.id.clone(),
        },
    ));
    if let Some(history) = view.history.get() {
        for row in &history.messages {
            body = body
                .push(Box::new(Text::new(&row.author)))
                .push(Box::new(Text::new(row.message.text.as_str())));
            if let Some(delivery) = row.message.delivery {
                body = body.push(Box::new(Text::new(&CATALOG.t(match delivery {
                    tilo_messaging::Delivery::Uncertain => "messages-uncertain",
                    _ => "messages-submitted",
                }))));
            }
        }
    }
    let draft = view.draft.clone();
    let operation = view.operation.clone();
    let p = page;
    let service = p.account.service.clone();
    let account = p.account.account.clone();
    let group = p.group.id.clone();
    let review = p.review.clone();
    let text = draft.clone();
    let can_send = !p.announcement || admin(p);
    body = body.push(Box::new(
        TextInput::from_state(&draft)
            .with_id("messages.group.composer")
            .with_accessible_name(CATALOG.t("messages-compose"))
            .disabled(!can_send),
    ));
    body = body.push(Box::new(actions.bind_effect(
        Button::new("messages.group.send", "messages-group-send").with_enabled(can_send),
        MessagesEffect::SEND_GROUP_TEXT.map(move |()| {
            (
                service.clone(),
                account.clone(),
                group.clone(),
                review.clone(),
                operation.get(),
                text.get(),
            )
        }),
    )));
    members(sources, actions, page, body)
}
fn members(
    sources: &Sources,
    actions: &Actions<Messages>,
    p: &GroupMembership,
    mut body: Column,
) -> Column {
    let admin = admin(p);
    for (person, contact) in sources.contacts().people.iter() {
        if contact.removed {
            continue;
        }
        for choice in crate::view::choices(sources, person.as_str())
            .into_iter()
            .filter(|c| {
                c.service == p.account.service
                    && c.account == p.account.account
                    && !c.recipient.is_empty()
                    && !p.members.iter().any(|m| m.address == c.recipient)
            })
        {
            let label = CATALOG.t_args(
                "messages-group-add-choice",
                &[
                    ("person", ui::i18n::ArgValue::Text(contact.label.clone())),
                    ("route", ui::i18n::ArgValue::Text(choice.label(sources))),
                ],
            );
            body = body.push(bind(
                actions,
                Button::new(
                    &format!("messages.group.add:{person}:{}", choice.association),
                    &label,
                )
                .with_enabled(admin),
                &MessagesEffect::AddGroupMember {
                    service: p.account.service.clone(),
                    account: p.account.account.clone(),
                    group: p.group.id.clone(),
                    review: p.review.clone(),
                    operation: uuid::Uuid::new_v4().to_string(),
                    person: person.as_str().into(),
                    choice: choice.value(),
                },
            ));
        }
    }
    for member in &p.members {
        let label = CATALOG.t_args(
            "messages-group-remove-choice",
            &[("member", ui::i18n::ArgValue::Text(member.address.clone()))],
        );
        body = body.push(bind(
            actions,
            Button::new(&format!("messages.group.remove:{}", member.address), &label)
                .with_enabled(admin && member.address != p.self_member),
            &MessagesEffect::RemoveGroupMember {
                service: p.account.service.clone(),
                account: p.account.account.clone(),
                group: p.group.id.clone(),
                review: p.review.clone(),
                operation: uuid::Uuid::new_v4().to_string(),
                member: member.address.clone(),
            },
        ));
    }
    body.push(bind(
        actions,
        Button::new("messages.group.leave", "messages-group-leave"),
        &MessagesEffect::LeaveGroup {
            service: p.account.service.clone(),
            account: p.account.account.clone(),
            group: p.group.id.clone(),
            review: p.review.clone(),
            operation: uuid::Uuid::new_v4().to_string(),
        },
    ))
}

fn admin(page: &GroupMembership) -> bool {
    page.members
        .iter()
        .find(|m| m.address == page.self_member)
        .is_some_and(|m| {
            matches!(
                m.role,
                tilo_messaging::GroupRole::Admin | tilo_messaging::GroupRole::SuperAdmin
            )
        })
}
