//! Opt-in protocol fixture: genuine managed startup and production Capnp client.
//! Never an installed application, model provider or substitute process witness.
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, IntoRawFd};
use std::os::unix::net::UnixStream;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn service(name: &str) -> Result<UnixStream> {
    let fd: i32 = std::env::var(name)?.parse()?;
    if fd < 3 {
        return Err("required private descriptor absent".into());
    }
    // SAFETY: the genuine managed launcher transfers exclusive descriptor ownership.
    Ok(unsafe { UnixStream::from_raw_fd(fd) })
}
fn frame(stream: &mut UnixStream, value: &impl serde::Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    let len = u32::try_from(bytes.len())?;
    stream.write_all(&len.to_le_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}
fn gate(stream: &mut UnixStream, expected: u8) -> Result<()> {
    let mut byte = [0];
    stream.read_exact(&mut byte)?;
    if byte != [expected] {
        return Err("private fixture gate changed".into());
    }
    Ok(())
}

fn exercise(
    identity: serde_json::Value,
    context: serde_json::Value,
    mut model: UnixStream,
) -> capnp::capability::Promise<(), Box<dyn std::error::Error>> {
    capnp::capability::Promise::from_future(async move {
        let agent = context
            .get("agent")
            .and_then(serde_json::Value::as_str)
            .ok_or("Agent absent")?;
        let channel_key = identity
            .get("channel")
            .and_then(serde_json::Value::as_str)
            .ok_or("channel absent")?;
        let presentation: Vec<u8> = serde_json::from_value(
            identity
                .get("presentation")
                .ok_or("presentation absent")?
                .clone(),
        )?;
        let channel = runtime_collab::try_connect()?
            .join_channel_as_kind(channel_key, &presentation, agent, "agent", "")
            .await?;
        let stream = channel.subscribe_enrolled_tasks().await?;
        let envelope = stream.read().await?;
        // A one-task stream must remain open during work, not immediately report retirement.
        if tokio::time::timeout(std::time::Duration::from_millis(20), stream.read())
            .await
            .is_ok()
        {
            return Err("live enrolled stream closed prematurely".into());
        }
        frame(
            &mut model,
            &serde_json::json!({"stage":"offered", "envelope":envelope}),
        )?;
        gate(&mut model, b'A')?;
        let mut custody = runtime_collab::EnrolledRecipientCustody::new(
            agent,
            context
                .get("launch")
                .and_then(serde_json::Value::as_str)
                .ok_or("launch absent")?,
            context
                .get("runtime_epoch")
                .and_then(serde_json::Value::as_str)
                .ok_or("runtime absent")?,
            context
                .get("channel_id")
                .and_then(serde_json::Value::as_str)
                .ok_or("channel absent")?,
        )?;
        let decision = custody.accept(&envelope)?;
        if !decision.execute {
            return Err("fresh authentic offer did not grant recipient acceptance".into());
        }
        let accepted = decision.acceptance;
        let mut forged = accepted.clone();
        forged.recipient_agent = "wrong-agent".into();
        if channel.accept_enrolled_task(&forged).await.is_ok() {
            return Err("forged recipient accepted".into());
        }
        if let Err(error) = channel.accept_enrolled_task(&accepted).await {
            frame(
                &mut model,
                &serde_json::json!({"stage":"refused", "error":error.to_string()}),
            )?;
            gate(&mut model, b'E')?;
            return Err(error.into());
        }
        channel.accept_enrolled_task(&accepted).await?;
        frame(
            &mut model,
            &serde_json::json!({"stage":"accepted", "acceptance":accepted}),
        )?;
        reports(channel, accepted, model).await
    })
}

fn reports(
    channel: runtime_collab::ChannelHandle,
    accepted: runtime_collab::EnrolledTaskAcceptance,
    mut model: UnixStream,
) -> capnp::capability::Promise<(), Box<dyn std::error::Error>> {
    capnp::capability::Promise::from_future(async move {
        gate(&mut model, b'P')?;
        let update = runtime_collab::TaskRunUpdate::Progress {
            message: "Private fixture progress; never a public phase inference".into(),
        };
        channel.report_enrolled_task(&accepted, 1, &update).await?;
        channel.report_enrolled_task(&accepted, 1, &update).await?;
        if channel
            .report_enrolled_task(&accepted, 1, &runtime_collab::TaskRunUpdate::Accepted)
            .await
            .is_ok()
        {
            return Err("changed report retry accepted".into());
        }
        if channel
            .report_enrolled_task(&accepted, 0, &update)
            .await
            .is_ok()
        {
            return Err("stale report accepted".into());
        }
        frame(&mut model, &serde_json::json!({"stage":"progress"}))?;
        let mut next = [0];
        model.read_exact(&mut next)?;
        let finish_sequence = match next[0] {
            b'D' => {
                let applied = runtime_collab::TaskRunUpdate::DirectionsApplied { revision: 1 };
                channel.report_enrolled_task(&accepted, 2, &applied).await?;
                channel.report_enrolled_task(&accepted, 2, &applied).await?;
                frame(
                    &mut model,
                    &serde_json::json!({"stage":"direction-applied"}),
                )?;
                gate(&mut model, b'F')?;
                3
            }
            b'F' => 2,
            _ => return Err("private fixture gate changed".into()),
        };
        let finished = runtime_collab::TaskRunUpdate::Succeeded {
            message: "Fixture complete".into(),
        };
        channel
            .report_enrolled_task(&accepted, finish_sequence, &finished)
            .await?;
        channel
            .report_enrolled_task(&accepted, finish_sequence, &finished)
            .await?;
        if channel
            .report_enrolled_task(&accepted, finish_sequence + 1, &update)
            .await
            .is_ok()
        {
            return Err("terminal run accepted progress".into());
        }
        frame(&mut model, &serde_json::json!({"stage":"finished"}))?;
        gate(&mut model, b'E')?;
        Ok(())
    })
}

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args
        .get(1)
        .is_some_and(|arg| arg == launcher::landlock_shim::SANDBOX_EXEC_SUBCOMMAND)
    {
        launcher::landlock_shim::run_sandbox_exec(args.get(2..).ok_or("shim arguments absent")?);
    }
    let mut collab = service("TILO_SVC_COLLAB_FD")?;
    collab.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    let mut challenge = [0; 32];
    collab.read_exact(&mut challenge)?;
    collab.write_all(&challenge)?;
    gate(&mut collab, 1)?;
    collab.set_read_timeout(None)?;
    let _transferred = collab.into_raw_fd(); // The production portal now owns this same descriptor.
    let mut identity = service("TILO_SVC_IDENTITY_FD")?;
    let mut length = [0; 4];
    identity.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_le_bytes(length))?;
    if length > 16 * 1024 {
        return Err("fixture certified presentation exceeds bound".into());
    }
    let mut presentation = vec![0; length];
    identity.read_exact(&mut presentation)?;
    let context: serde_json::Value =
        serde_json::from_str(&std::env::var("TILO_MANAGED_AGENT_CONTEXT")?)?;
    let model = service("TILO_SVC_MODEL-GATEWAY_FD")?;
    model.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(tokio::task::LocalSet::new().run_until(exercise(
        serde_json::from_slice(&presentation)?,
        context,
        model,
    )))
}
