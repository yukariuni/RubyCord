// @description Shows user badges from other Discord client mods on profiles
// @version 0.5.7
// @author iFlex0x, wolfieeeeeeee
// @warning Fetches badges from Equibadges (badges.equicord.org)

const API = "https://badges.equicord.org/users";
const TTL = 6 * 60 * 60 * 1000;
const RETRY_DELAY = 30 * 1000;
const BADGE_ID_BASE = 900_000;
const CACHE_KEY = "equibadges";

// Badges whose art Equibadges still serves in its old form, keyed mod:name.
const ART_OVERRIDES: Record<string, string> = {
  "vencord:vencord contributor":
    "https://cdn.discordapp.com/emojis/1562267648857997356.webp?size=128",
};

type Badge = { uri: string; name: string; source: string };

const settings = phoenix.settings.define({
  showCustom: {
    type: "switch",
    title: "Badge source",
    description: "Show where community badges came from in Badge Directory",
    default: true,
  },
  badgeVault: {
    type: "switch",
    title: "BadgeVault badges",
    description: "Show BadgeVault badges, which any user can make",
    default: false,
  },
});

let users: Record<string, unknown> = {};
let refreshAt = 0;
let refreshing: Promise<void> | undefined;

const providerNames = new Map(Object.entries({
  aliucord: "Aliucord", equicord: "Equicord",
  nekocord: "Nekocord", raincord: "Raincord", replugged: "Replugged",
  reviewdb: "ReviewDB", vencord: "Vencord", enmity: "Enmity",
  velocity: "Velocity", aero: "Aero",
  goosemod: "GooseMod", vendroidenhanced: "Vendroid Enhanced", paicord: "Paicord",
  revenge: "Revenge", betterdiscord: "BetterDiscord", record: "ReCord",
  bunny: "Bunny",
}));

function toHex(text: string): string {
  let out = "";
  for (let i = 0; i < text.length; i++) {
    out += text.charCodeAt(i).toString(16).padStart(2, "0");
  }
  return out;
}

// Phoenix caches badge details by id, so each image needs its own.
function badgeId(uri: string): number {
  let hash = 2166136261;
  for (let i = 0; i < uri.length; i++) {
    hash ^= uri.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  return BADGE_ID_BASE + ((hash >>> 0) % 1_000_000_000);
}

// The runtime unwraps this URL and clamps the art to the static badge size.
function badgeUrl(uri: string, animated = false): string {
  return `https://cdn.discordapp.com/badge-icons/phx_u_${toHex(uri)}${animated ? ".gif" : ".png"}`;
}

async function userIdFromPath(path: string): Promise<string | null> {
  const m = path.match(/^\/users\/(@me|\d+)\/(?:profile|badges)(?:[?#]|$)/);
  if (!m) return null;
  return m[1] === "@me" ? ((await phoenix.discord.me())?.id ?? null) : m[1];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function parseBadges(entries: unknown): Badge[] {
  if (!Array.isArray(entries)) return [];

  const badges: Badge[] = [];
  const seen = new Set<string>();
  for (const entry of entries) {
    if (!isRecord(entry) || typeof entry.mod !== "string" ||
      typeof entry.badge !== "string" || typeof entry.tooltip !== "string") continue;
    const mod = entry.mod.trim().toLowerCase();
    if (!mod || mod === "discord" || (mod === "badgevault" && !settings.badgeVault)) continue;
    const source = providerNames.get(mod) ?? entry.mod.trim();
    const name = entry.tooltip.trim();
    const uri = ART_OVERRIDES[`${mod}:${name.toLowerCase()}`] ?? entry.badge.trim();
    if (!name || !/^https?:\/\/[^/\s]+/i.test(uri) || seen.has(uri)) continue;
    seen.add(uri);
    badges.push({ uri, name, source });
  }
  return badges;
}

async function refreshUsers(): Promise<void> {
  if (!refreshing) {
    refreshing = (async () => {
      const res = await phoenix.fetch(API);
      if (!res.ok) throw new Error(`Equibadges HTTP ${res.status}`);
      const body = res.json<unknown>();
      if (!isRecord(body) || !isRecord(body.users)) throw new Error("Invalid Equibadges response");
      if (!Object.keys(body.users).length) throw new Error("Empty Equibadges response");
      users = body.users;
      refreshAt = Date.now() + TTL;
      // Survive restarts: the bulk map takes ~7 seconds, longer than the
      // loader's 4 second onResponse cutoff, so a warm start must not fetch.
      try {
        phoenix.storage.set(CACHE_KEY, { at: Date.now(), users });
      } catch {}
    })()
      .catch(() => { refreshAt = Date.now() + RETRY_DELAY; })
      .finally(() => { refreshing = undefined; });
  }
  return refreshing;
}

function readCache(): void {
  try {
    const cached = phoenix.storage.get<{ at?: unknown; users?: unknown }>(CACHE_KEY);
    if (!cached || typeof cached.at !== "number" || !isRecord(cached.users)) return;
    if (!Object.keys(cached.users).length) return;
    users = cached.users;
    refreshAt = cached.at + TTL;
  } catch {}
}

async function badgesFor(userId: string): Promise<Badge[]> {
  // Never hold a profile hook for the bulk download: the cached map covers
  // every restart after the first, and a cold start catches up next open.
  if (Date.now() >= refreshAt) void refreshUsers();
  return parseBadges(users[userId]);
}

// Team and role badges read as generic on their own ("Contributor", "Donor"),
// so they keep the mod name. Personal badges like BadgeVault ones do not.
const ROLE_BADGE =
  /\b(contributor|early user|developer|dev|donor|donator|staff|team)\b/i;

function displayName({ name, source }: Badge): string {
  return ROLE_BADGE.test(name) && !name.toLowerCase().startsWith(source.toLowerCase())
    ? `${source} ${name}`
    : name;
}

// Serve the stored map first so the first profile after a restart is instant,
// then refresh only when it is missing or past its TTL.
readCache();
if (Date.now() >= refreshAt) void refreshUsers();

phoenix.discord.onResponse(
  { method: "GET", pathPrefix: "/users/", pathSuffix: "/profile" },
  async (res) => {
    if (res.status < 200 || res.status >= 300) return;
    const userId = await userIdFromPath(res.path);
    if (!userId) return;
    const data = res.json<Record<string, unknown>>();
    if (!isRecord(data)) return;
    const extra = await badgesFor(userId);
    if (!extra.length) return;
    const existing = Array.isArray(data.badges) ? data.badges : [];
    const injected = extra.map((b) => ({
      id: String(badgeId(b.uri)),
      description: displayName(b),
      icon: `phx_u_${toHex(b.uri)}`,
      simple_icon_url: b.uri,
    }));
    data.badges = [...injected, ...existing];
    return { body: data };
  }
);

phoenix.discord.onResponse(
  { method: "GET", pathPrefix: "/users/", pathSuffix: "/badges" },
  async (res) => {
    // `/users/@me/badges` is the directory where you order and hide your own
    // badges, and saves back to Discord, which rejects ids it never issued.
    if (res.path.startsWith("/users/@me/")) return;
    // Discord 404s this endpoint when the user has no badges of its own, which
    // leaves the badge window erroring. Give it a 200 with ours instead.
    const noBadges = res.status === 404;
    if (!noBadges && (res.status < 200 || res.status >= 300)) return;
    const userId = await userIdFromPath(res.path);
    if (!userId) return;
    const extra = await badgesFor(userId);
    if (!extra.length) return;
    const data = noBadges ? {} : res.json<Record<string, unknown>>();
    if (!isRecord(data)) return;
    const existing = Array.isArray(data.badges) ? data.badges : [];
    const injected = extra.map((b) => ({
      badge_id: badgeId(b.uri),
      name: displayName(b),
      description: settings.showCustom ? `Badge from ${b.source}.` : "",
      ...(settings.showCustom ? { info_label: b.source } : {}),
      rarity: 5,
      simple_icon_url: badgeUrl(b.uri),
      simple_icon_raster_url: badgeUrl(b.uri),
      complex_icon_static_url: badgeUrl(b.uri),
      ...(/(\.gif|\.webp)(?:[?#]|$)/i.test(b.uri) ? { complex_icon_animated_url: badgeUrl(b.uri, true) } : {}),
      is_earnable: true,
      owned: true,
      hidden: false,
    }));
    data.badges = [...injected, ...existing];
    return noBadges ? { status: 200, body: data } : { body: data };
  }
);
