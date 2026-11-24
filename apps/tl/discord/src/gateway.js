// The Discord gateway: the bridge's held connection, in the connection process alone. A process
// started for a call never opens it; such a process writes records the holder watches.
//
// The library is loaded only here, so the planners and their tests never need it.

/// Connect `client` to Discord as `token`, handing every gateway event to `onEvent`.
///
/// `make`: the library's client factory (`discord.js`), given so a test can replace it.
export async function connect({ make, token, onEvent, watch }) {
  const client = make();
  client.on("messageCreate", (message) => onEvent(discordMessage("MESSAGE_CREATED", message)));
  client.on("messageUpdate", (_, message) => onEvent(discordMessage("MESSAGE_UPDATED", message)));
  client.on("messageDelete", (message) => onEvent(discordMessage("MESSAGE_DELETED", message)));
  client.on("messageReactionAdd", (reaction, user) =>
    onEvent(discordReaction("REACTION_ADDED", reaction, user)),
  );
  client.on("messageReactionRemove", (reaction, user) =>
    onEvent(discordReaction("REACTION_REMOVED", reaction, user)),
  );
  client.on("guildMemberUpdate", (_, member) => onEvent(discordMember(member)));
  await client.login(token);
  if (watch) await watch();
  return client;
}

/// A message of the gateway as the planners read it.
export function discordMessage(source, message) {
  return {
    source,
    payload: message
      ? {
          id: message.id,
          channel_id: message.channelId,
          guild_id: message.guildId,
          content: message.content ?? "",
          timestamp_ms: message.createdTimestamp ?? 0,
          webhook_id: message.webhookId ?? null,
          author: {
            id: message.author?.id ?? "",
            name: message.author?.username ?? "Someone",
            avatar: message.author?.avatar ?? null,
            bot: Boolean(message.author?.bot),
          },
          reference: message.reference ?? null,
        }
      : null,
  };
}

/// A reaction of the gateway as the planners read it.
export function discordReaction(source, reaction, user) {
  return {
    source,
    payload: {
      message_id: reaction?.message?.id ?? "",
      channel_id: reaction?.message?.channelId ?? "",
      emoji: { name: reaction?.emoji?.name ?? "", id: reaction?.emoji?.id ?? "" },
      user_id: user?.id ?? "",
      user_name: user?.username ?? "Someone",
    },
  };
}

/// A member change as the planners read it.
export function discordMember(member) {
  return {
    source: "MEMBER_CHANGED",
    payload: {
      guild_id: member?.guild?.id ?? "",
      user_id: member?.id ?? "",
      roles: [...(member?.roles?.cache?.keys?.() ?? [])],
      banned: false,
      left: false,
      user: { name: member?.user?.username ?? "Someone" },
      avatar: member?.user?.avatar ?? null,
    },
  };
}
