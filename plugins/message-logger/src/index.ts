// @description Keeps deleted messages and edit history
// @version 0.3.0
// @author amaanq

const { gateway } = phoenix;

const settings = phoenix.settings.define({
  keepDeleted: {
    type: "switch",
    title: "Keep deleted messages",
    description: "Keep deleted messages, in red",
    default: true,
  },
  keepEdits: {
    type: "switch",
    title: "Keep edit history",
    description: "Show earlier versions under edited messages",
    default: true,
  },
  keepAttachments: {
    type: "switch",
    title: "Keep removed attachments",
    description: "Leave attachments an edit took off the message in place",
    default: true,
  },
  ignoreBots: {
    type: "switch",
    title: "Ignore bots",
    description: "Skip messages sent by bots",
    default: true,
  },
  ignoreUsers: {
    type: "text",
    title: "Ignored users",
    description: "User IDs separated by commas or spaces",
    default: "",
  },
  ignoreChannels: {
    type: "text",
    title: "Ignored channels",
    description: "Channel IDs separated by commas or spaces",
    default: "",
  },
  ignoreGuilds: {
    type: "text",
    title: "Ignored servers",
    description: "Server IDs separated by commas or spaces",
    default: "",
  },
});

interface Change {
  /// The text before an edit, or the attachments an edit removed.
  text: string;
  removed: boolean;
  at: number;
}

interface Seen {
  content: string;
  attachments: Phoenix.Attachment[];
  changes: Change[];
  deletedAt?: number;
  repainted?: boolean;
  /// The whole message on desktop, to put back one this client deleted.
  message?: Phoenix.GatewayMessage;
}

const isDesktop = typeof (phoenix as { bridge?: unknown }).bridge === "undefined";

const EPHEMERAL = 64;
const RED = 0xfff23f43;
const limit = 8192;
const seen = new Map<string, Seen>();

const ids = (list: string) => list.split(/[\s,]+/).filter(Boolean);

const ignored = (message: Phoenix.GatewayMessage) =>
  (message.flags & EPHEMERAL) !== 0 ||
  (settings.ignoreBots && message.author?.bot === true) ||
  ids(settings.ignoreUsers).includes(message.author?.id) ||
  ids(settings.ignoreChannels).includes(message.channel_id) ||
  (message.guild_id !== undefined && ids(settings.ignoreGuilds).includes(message.guild_id));

const remember = (message: Phoenix.GatewayMessage) => {
  let entry = seen.get(message.id);
  if (!entry) {
    entry = { content: message.content, attachments: [], changes: [] };
    seen.set(message.id, entry);
    if (seen.size > limit) seen.delete(seen.keys().next().value!);
  }
  entry.content = message.content;
  entry.attachments = message.attachments ?? entry.attachments;
  if (isDesktop) entry.message = message;
  return entry;
};

const pad = (n: number) => String(n).padStart(2, "0");
const clock = (at: number) => {
  const date = new Date(at);
  return `${pad(date.getHours())}:${pad(date.getMinutes())}`;
};

// Earlier versions under the message, oldest first, in the accessory's muted style.
const history = phoenix.ui.addMessageAccessory({
  priority: 1,
  render({ messageId }) {
    const entry = seen.get(messageId);
    if (!settings.keepEdits || !entry?.changes.length) return null;
    const text = entry.changes
      .map((change) =>
        change.removed
          ? `Removed ${change.text} · ${clock(change.at)}`
          : `${change.text || "(empty)"} · edited ${clock(change.at)}`,
      )
      .join("\n");
    // Red with the rest of a deleted message.
    return entry.deletedAt === undefined ? text : { text, color: RED };
  },
});

const deleted = phoenix.ui.addMessageAccessory({
  render({ messageId }) {
    const at = seen.get(messageId)?.deletedAt;
    return at === undefined ? null : { text: `Deleted · ${clock(at)}`, color: RED, tint: RED };
  },
});

// Hydrate seen messages from channel history fetches on desktop so edits and
// deletes of messages sent before startup are also captured cleanly.
if (isDesktop) {
  phoenix.discord.onResponse(
    { method: "GET", pathPrefix: "/channels/", pathSuffix: "/messages" },
    (res) => {
      if (res.status < 200 || res.status >= 300) return;
      const list = res.json<unknown>();
      if (!Array.isArray(list)) return;
      for (const item of list) {
        const msg = item as Phoenix.GatewayMessage;
        if (!msg?.id || typeof msg.content !== "string" || ignored(msg)) continue;
        if (!seen.has(msg.id)) remember(msg);
      }
    },
  );
}

phoenix.hook(gateway.messageCreate, {
  before(message) {
    if (!message.id || typeof message.content !== "string" || ignored(message)) return;
    // A create whose nonce names an existing message replaces that message in
    // place, which would erase it without a delete event.
    const nonce = message.nonce === undefined ? undefined : String(message.nonce);
    if (nonce && nonce !== message.id && seen.has(nonce)) delete message.nonce;
    remember(message);
  },
});

phoenix.hook(gateway.messageUpdate, {
  before(message) {
    // Embed-only updates carry no content.
    if (!message.id || !message.channel_id || typeof message.content !== "string") return;
    if (ignored(message)) return;
    const old = seen.get(message.id);
    const at = message.edited_timestamp ? Date.parse(message.edited_timestamp) : Date.now();
    const removed = old?.attachments.filter(
      (attachment) => !message.attachments?.some((kept) => kept.id === attachment.id),
    ) ?? [];
    const changes = old?.changes ?? [];
    if (old && old.content !== message.content) changes.push({ text: old.content, removed: false, at });
    if (removed.length && settings.keepAttachments) {
      message.attachments = [...(message.attachments ?? []), ...removed];
      changes.push({ text: removed.map((attachment) => attachment.filename).join(", "), removed: true, at });
    }
    remember(message).changes = changes;
    if (old) history.update?.(message.id);
  },
});

// Keep deleted messages marked. On desktop, also handle messages loaded from
// local cache before startup (absent from `seen`) as long as their channel and
// server aren't ignored, and trigger a content-free MESSAGE_UPDATE so GPUI
// repaints the row immediately with its native accessory.
const markDeleted = (id: string, channelId?: string, guildId?: string) => {
  let entry = seen.get(id);
  if (!entry) {
    if (!isDesktop) return false;
    if (
      (channelId && ids(settings.ignoreChannels).includes(channelId)) ||
      (guildId && ids(settings.ignoreGuilds).includes(guildId))
    ) {
      return false;
    }
    entry = { content: "", attachments: [], changes: [] };
    seen.set(id, entry);
    if (seen.size > limit) seen.delete(seen.keys().next().value!);
  }
  if (entry.deletedAt === undefined) {
    entry.deletedAt = Date.now();
    deleted.update?.(id);
    if (entry.changes.length) history.update?.(id);
  }
  if (isDesktop && channelId && !entry.repainted) {
    try {
      phoenix.dispatch(gateway.messageUpdate, {
        id,
        channel_id: channelId,
        ...(guildId ? { guild_id: guildId } : {}),
      });
      entry.repainted = true;
    } catch {
      // Runs outside a gateway hook during same-client optimistic delete;
      // the follow-up MESSAGE_DELETE gateway event will trigger the repaint.
    }
  }
  return true;
};

phoenix.hook(gateway.messageDelete, {
  before(message) {
    if (settings.keepDeleted && markDeleted(message.id, message.channel_id, message.guild_id)) {
      return false;
    }
  },
});

// Desktop drops a message this client deletes before any gateway event, so
// once Discord confirms the delete, the message goes back in and is marked.
if (isDesktop) {
  phoenix.discord.onResponse({ method: "DELETE", pathPrefix: "/channels/", pathIncludes: "/messages/" }, (res) => {
    const [, channelId, , id, rest] = res.path.split("?")[0].split("/").slice(1);
    if (res.status < 200 || res.status >= 300 || rest !== undefined || !settings.keepDeleted) return;
    const message = seen.get(id)?.message;
    if (!message || message.channel_id !== channelId || !markDeleted(id)) return;
    const { nonce: _, ...restored } = message;
    phoenix.enqueue(gateway.messageCreate, restored);
  });
}

phoenix.hook(gateway.messageDeleteBulk, {
  before(bulk) {
    if (!settings.keepDeleted) return;
    bulk.ids = bulk.ids.filter((id) => !markDeleted(id, bulk.channel_id, bulk.guild_id));
    if (!bulk.ids.length) return false;
  },
});
