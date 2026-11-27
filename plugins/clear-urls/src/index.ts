// @description Removes tracking parameters from links you send and receive
// @version 0.1.2
// @author amaanq

const { gateway } = phoenix;

const TRACKERS = [
  /^utm_/,
  /^(ref_src|ref_url|fbclid|gclid|dclid|gbraid|wbraid|msclkid|yclid|twclid|ttclid|igshid|igsh|mc_eid|mc_cid)$/,
  /^(_hsenc|_hsmi|mkt_tok|oly_anon_id|oly_enc_id|vero_id|wickedid|__s)$/,
];

// Short parameter names are tracking only on these hosts. Elsewhere they are
// often real parameters, such as a YouTube start time.
const SOCIAL = /(^|\.)(twitter\.com|x\.com|tiktok\.com)$/;
const YOUTUBE = /(^|\.)(youtube\.com|youtu\.be)$/;
const SCOPED = new Map([
  ["s", SOCIAL],
  ["t", SOCIAL],
  ["feature", YOUTUBE],
  ["pp", YOUTUBE],
  ["si", /(^|\.)(youtube\.com|youtu\.be|spotify\.com|soundcloud\.com)$/],
]);

const isTracker = (key: string, host: string) =>
  SCOPED.get(key)?.test(host) ?? TRACKERS.some((tracker) => tracker.test(key));

const clean = (url: string) => {
  const query = url.indexOf("?");
  const hash = url.indexOf("#");
  if (query === -1 || (hash !== -1 && hash < query)) return url;
  const end = hash === -1 ? url.length : hash;
  const authority = url.slice(url.indexOf("//") + 2).split(/[/?#]/)[0];
  const host = authority.slice(authority.lastIndexOf("@") + 1).split(":")[0].toLowerCase().replace(/\.$/, "");
  const pairs = url.slice(query + 1, end).split("&");
  const kept = pairs.filter((pair) => {
    if (!pair) return false;
    let key: string;
    try {
      key = decodeURIComponent(pair.split("=")[0]);
    } catch {
      return true;
    }
    return !isTracker(key, host);
  });
  if (kept.length === pairs.length) return url;
  return url.slice(0, query) + (kept.length ? `?${kept.join("&")}` : "") + url.slice(end);
};

const cleanLinks = (text: string) => text.replace(/https?:\/\/(?:[^\s<>()`|]|\([^\s<>()`|]*\))+/gi, (url) => {
  const suffix = url.match(/(?:[.,!?;:\]]|\*\*|~~)+$/)?.[0] ?? "";
  const link = suffix ? url.slice(0, -suffix.length) : url;
  return clean(link) + suffix;
});

const rewrite = (message: { content?: string }) => {
  if (typeof message.content !== "string" || !message.content.includes("?")) return;
  message.content = cleanLinks(message.content);
};

phoenix.hook(gateway.messageCreate, { before: rewrite });
phoenix.hook(gateway.messageUpdate, { before: rewrite });
phoenix.messages.onSend((text) => (text.includes("?") ? cleanLinks(text) : undefined));
