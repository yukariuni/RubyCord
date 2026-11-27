// @description Shows remaining timeout duration beside usernames
// @version 0.1.0
// @author hakuxyz, port of Valenced "Show Timeout Duration" by Palm

const { gateway, discord, ui } = phoenix;
const RED = 0xffed4245;
const ICON = "res://design_components_icon_native_redesign_generated_images_clockwarningicon";

type Member = {
  user?: { id?: string };
  user_id?: string;
  communication_disabled_until?: string | null;
};

type Message = {
  id?: string;
  author?: { id?: string };
  guild_id?: string;
  member?: Member;
};

type Author = { user: string; guild?: string };

const pad = (id: string) => id.padStart(20, "0");
const key = (guild: string | undefined, user: string) => `${guild ?? ""}:${user}`;
const endOf = (member: Member) => {
  const end = Date.parse(member.communication_disabled_until ?? "");
  return end > Date.now() ? end : 0;
};

const authors = new Map<string, Author>();
const ends = new Map<string, number>();
const marks = new Map<string, (() => void)[]>();

const rememberMember = (member: Member, guild?: string, user = member.user?.id ?? member.user_id) => {
  if (!user) return;
  const end = endOf(member);
  if (end) ends.set(key(guild, user), end);
  else ends.delete(key(guild, user));
};

const rememberMessage = (message: Message) => {
  if (!message.id || !message.author?.id) return;
  authors.set(message.id, { user: message.author.id, guild: message.guild_id });
  if (message.member) rememberMember(message.member, message.guild_id, message.author.id);
};

phoenix.hook(gateway.messageCreate, { after: rememberMessage });
phoenix.hook(gateway.messageUpdate, { after: rememberMessage });
phoenix.on(gateway.messageDelete, (message) => authors.delete(message.id));
phoenix.on(gateway.messageDeleteBulk, (message) => {
  for (const id of message.ids) authors.delete(id);
});

// Members and messages Discord fetches itself, so this sends no requests.
const list = <T>(res: Phoenix.InterceptedResponse) => {
  if (res.status < 200 || res.status >= 300) return [];
  try {
    const body = res.json<T | T[] | null>();
    return Array.isArray(body) ? body : body ? [body] : [];
  } catch {
    return [];
  }
};
discord.onResponse({ pathIncludes: "/members" }, (res) => {
  const guild = res.path.split("/")[2];
  for (const member of list<Member>(res)) rememberMember(member, guild);
});
discord.onResponse({ pathIncludes: "/messages" }, (res) => {
  for (const message of list<Message>(res)) rememberMessage(message);
});

const cachedAuthor = async (id: string) => {
  const rows = await phoenix.stores.sql<{ v: string }>(
    "SELECT CAST(v AS TEXT) AS v FROM messages WHERE k LIKE ? LIMIT 1",
    `%:${pad(id)}`,
  );
  try {
    if (rows[0]) rememberMessage({ ...(JSON.parse(rows[0].v) as Message), id });
  } catch {}
  return authors.get(id);
};

// Member keys are `guild:user`, both zero-padded.
const cachedEnd = async ({ guild, user }: Author) => {
  if (!guild) return 0;
  const rows = await phoenix.stores.sql<{ v: string }>(
    "SELECT CAST(v AS TEXT) AS v FROM members WHERE k = ? LIMIT 1",
    `${pad(guild)}:${pad(user)}`,
  );
  try {
    return rows[0] ? endOf(JSON.parse(rows[0].v) as Member) : 0;
  } catch {
    return 0;
  }
};

const format = (ms: number) => {
  const total = Math.ceil(ms / 1000);
  const parts = [
    [Math.floor(total / 86400), "d"],
    [Math.floor((total % 86400) / 3600), "h"],
    [Math.floor((total % 3600) / 60), "m"],
    [total % 60, "s"],
  ] as const;
  const first = parts.findIndex(([value]) => value > 0);
  if (first < 0) return "0s";
  return parts
    .slice(first, first + 2)
    .filter(([value]) => value > 0)
    .map(([value, unit]) => `${value}${unit}`)
    .join(" ");
};

const clear = (id: string) => {
  for (const remove of marks.get(id) ?? []) remove();
  marks.delete(id);
};

const place = (id: string, node: Phoenix.NodeInfo, remaining: number) => {
  if (node.parent == null) return;
  clear(id);
  const besideTimestamp = (ui.node(node.parent)?.children ?? []).some((child) =>
    ui.node(child)?.accessibilityId?.startsWith("message.timestamp:"),
  );
  marks.set(id, [
    ui.patchProps({ exact: `message.author:${id}` }, { color: RED, margin: [0, 0, 0, 0] }),
    ui.mount(
      node.parent,
      {
        kind: "view",
        props: { direction: "row", align: "center", margin: [0, 0, besideTimestamp ? -6 : -2, 0] },
        children: [
          { kind: "image", props: { uri: ICON, width: 14, height: 14, tint: RED } },
          {
            kind: "text",
            props: { text: format(remaining), color: RED, fontSize: 12, margin: [1.5, 0, 0, 0] },
          },
        ],
      },
      { before: node.id },
    ),
  ]);
};

ui.onNode({ prefix: "message.author:" }, async (node) => {
  const aid = `${node.accessibilityId}`;
  const id = aid.slice("message.author:".length);
  const author = authors.get(id) ?? (await cachedAuthor(id));
  if (!author) return;
  const end = ends.get(key(author.guild, author.user)) || (await cachedEnd(author));
  // The row may have been recycled for another message while the cache was read.
  const live = ui.node(node.id);
  if (live?.accessibilityId !== aid) return;
  if (end > Date.now()) place(id, live, end - Date.now());
  else clear(id);
});

phoenix.onUnload(() => {
  for (const id of [...marks.keys()]) clear(id);
});
