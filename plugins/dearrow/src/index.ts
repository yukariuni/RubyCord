// @description Replaces YouTube titles and thumbnails in embeds using DeArrow
// @version 0.2.0
// @author amaanq

const { gateway } = phoenix;

const settings = phoenix.settings.define({
  titles: {
    type: "switch",
    title: "Replace titles",
    description: "Use crowdsourced titles from DeArrow",
    default: true,
  },
  thumbnails: {
    type: "switch",
    title: "Replace thumbnails",
    description: "Use crowdsourced thumbnails from DeArrow",
    default: true,
  },
});

interface Branding {
  titles: { title: string; original: boolean; votes: number; locked: boolean }[];
  thumbnails: { timestamp: number | null; original: boolean; votes: number; locked: boolean }[];
}

interface Replacement {
  title?: string;
  thumbnail?: string;
}

const cache = new Map<string, Promise<Replacement>>();

const videoId = (url: string | undefined) =>
  url?.match(/(?:youtube\.com\/(?:watch\?(?:.*&)?v=|shorts\/)|youtu\.be\/)([\w-]{11})/)?.[1];

const trusted = (entry: { original: boolean; votes: number; locked: boolean }) =>
  !entry.original && (entry.votes >= 0 || entry.locked);

const lookup = (id: string) => {
  let pending = cache.get(id);
  if (!pending) {
    pending = phoenix
      .fetch(`https://sponsor.ajay.app/api/branding?videoID=${id}`)
      .then((response) => {
        if (!response.ok) return {};
        const branding = response.json<Branding>();
        const title = branding.titles.find(trusted)?.title.replace(/(^|\s)>(\S)/g, "$1$2");
        const thumbnail = branding.thumbnails.find(trusted);
        return {
          title,
          thumbnail:
            thumbnail?.timestamp == null
              ? undefined
              : `https://dearrow-thumb.ajay.app/api/v1/getThumbnail?videoID=${id}&time=${thumbnail.timestamp}`,
        };
      })
      .catch((err) => {
        cache.delete(id);
        phoenix.log.warn(`dearrow ${id}: ${err}`);
        return {};
      });
    // One entry per video seen, so drop the oldest past a few hundred.
    if (cache.size >= 512) cache.delete(cache.keys().next().value!);
    cache.set(id, pending);
  }
  return pending;
};

// Rewrites the YouTube embeds in place, and says whether any changed.
const apply = async (embeds: Phoenix.Embed[] | undefined) => {
  let changed = false;
  for (const embed of embeds ?? []) {
    const id = videoId(embed.url);
    if (!id) continue;
    const { title, thumbnail } = await lookup(id);
    if (title && settings.titles && embed.title !== title) {
      embed.title = title;
      changed = true;
    }
    if (thumbnail && settings.thumbnails && embed.thumbnail && embed.thumbnail.url !== thumbnail) {
      embed.thumbnail = { ...embed.thumbnail, url: thumbnail, proxy_url: thumbnail };
      changed = true;
    }
  }
  return changed;
};

const rebrand = async (message: Phoenix.GatewayMessage) => {
  const embeds = message.embeds;
  if (!(await apply(embeds))) return;
  phoenix.enqueue(gateway.messageUpdate, {
    id: message.id,
    channel_id: message.channel_id,
    ...(message.guild_id ? { guild_id: message.guild_id } : {}),
    embeds,
  });
};

phoenix.on(gateway.messageCreate, rebrand);
phoenix.on(gateway.messageUpdate, rebrand);

// Desktop loads message history over HTTP, past the gateway.
phoenix.discord.onResponse({ method: "GET", pathPrefix: "/channels/", pathSuffix: "/messages" }, async (res) => {
  if (res.status !== 200 || !/youtu\.?be/.test(res.body)) return;
  const messages = res.json<Phoenix.GatewayMessage[]>();
  if (!Array.isArray(messages)) return;
  const changed = await Promise.all(messages.map((message) => apply(message.embeds)));
  if (changed.some(Boolean)) return { body: JSON.stringify(messages) };
});
