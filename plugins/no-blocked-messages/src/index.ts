// @description Hides messages from blocked (and optionally ignored) users completely
// @version 1.0.1
// @author Nachtasyl

const { gateway } = phoenix;

const settings = phoenix.settings.define({
  applyToIgnored: {
    type: "switch",
    title: "Also hide ignored users",
    description: "Hide messages from users you marked as Ignored (Discord Ignore), not only Blocked",
    default: true,
  },
});

/** Discord relationship type: BLOCKED */
const REL_BLOCKED = 2;

interface Relationship {
  id: string;
  type: number;
  user_ignored?: boolean;
  user?: { id?: string };
}

const blocked = new Set<string>();
const ignored = new Set<string>();
const CACHE_KEY = "relationships";

const authorId = (msg: { author?: { id?: string } } | null | undefined) =>
  msg?.author?.id;

const shouldHide = (userId?: string | null) => {
  if (!userId) return false;
  if (blocked.has(userId)) return true;
  return settings.applyToIgnored && ignored.has(userId);
};

const applyRelationship = (rel: Relationship) => {
  const id = rel.id || rel.user?.id;
  if (!id) return;

  if (rel.type === REL_BLOCKED) blocked.add(id);
  else blocked.delete(id);

  if (rel.user_ignored) ignored.add(id);
  else ignored.delete(id);
};

const removeRelationship = (rel: { id?: string; user?: { id?: string } }) => {
  const id = rel.id || rel.user?.id;
  if (!id) return;
  blocked.delete(id);
  ignored.delete(id);
};

const replaceFromList = (list: Relationship[]) => {
  blocked.clear();
  ignored.clear();
  for (const rel of list) applyRelationship(rel);
  try {
    phoenix.storage.set(CACHE_KEY, {
      blocked: [...blocked],
      ignored: [...ignored],
    });
  } catch {
    // storage can fail on first run; live sets still work
  }
};

const readCache = () => {
  try {
    const cached = phoenix.storage.get<{ blocked?: string[]; ignored?: string[] }>(CACHE_KEY);
    if (!cached) return;
    if (Array.isArray(cached.blocked)) for (const id of cached.blocked) blocked.add(id);
    if (Array.isArray(cached.ignored)) for (const id of cached.ignored) ignored.add(id);
  } catch {
    // ignore bad cache
  }
};

const loadRelationships = async () => {
  const res = await phoenix.discord.get("/users/@me/relationships");
  if (!res.ok) throw new Error(`relationships HTTP ${res.status}`);
  const data = res.json<unknown>();
  if (Array.isArray(data)) replaceFromList(data as Relationship[]);
};

// Warm from disk so the first messages after start are filtered before the GET returns.
readCache();
// Desktop has no session until its own first API request, so a load at
// startup fails there. Try again after the first response.
loadRelationships().catch(() => {
  const stop = phoenix.discord.onResponse({ method: "GET" }, () => {
    stop();
    loadRelationships().catch((e) =>
      phoenix.log.error("no-blocked-messages: failed to load relationships", e),
    );
  });
});

// Keep sets in sync when Discord returns the full list again.
phoenix.discord.onResponse(
  { method: "GET", pathPrefix: "/users/@me/relationships" },
  (res) => {
    if (res.status < 200 || res.status >= 300) return;
    const data = res.json<unknown>();
    if (!Array.isArray(data)) return;
    replaceFromList(data as Relationship[]);
  },
);

// Live relationship events (handles exist only if this Phoenix build registered them).
const onRel = (
  name: "relationshipAdd" | "relationshipUpdate" | "relationshipRemove",
  handler: (rel: Relationship) => void,
) => {
  const sig = gateway[name];
  if (!sig) return;
  phoenix.hook(sig, {
    before(payload) {
      handler(payload as Relationship);
    },
  });
};

onRel("relationshipAdd", applyRelationship);
onRel("relationshipUpdate", applyRelationship);
onRel("relationshipRemove", removeRelationship);

// Real-time: drop before Phoenix stores / renders the message.
phoenix.hook(gateway.messageCreate, {
  before(message) {
    if (shouldHide(authorId(message))) return false;
  },
});

phoenix.hook(gateway.messageUpdate, {
  before(message) {
    if (shouldHide(authorId(message))) return false;
  },
});

// Channel history / jump-to-message loads.
phoenix.discord.onResponse(
  { method: "GET", pathPrefix: "/channels/", pathSuffix: "/messages" },
  (res) => {
    if (res.status < 200 || res.status >= 300) return;
    const list = res.json<unknown>();
    if (!Array.isArray(list)) return;

    const filtered = list.filter((item) => {
      const msg = item as { author?: { id?: string } };
      return !shouldHide(authorId(msg));
    });

    if (filtered.length !== list.length) return { body: filtered };
  },
);
