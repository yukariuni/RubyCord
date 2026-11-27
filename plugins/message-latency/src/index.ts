// @description Flags messages with high send latency
// @version 0.1.0
// @author amaanq

const { gateway } = phoenix;

const settings = phoenix.settings.define({
  threshold: {
    type: "number",
    title: "Threshold",
    description: "Seconds a message must take to get marked",
    default: 2,
  },
  showBots: {
    type: "switch",
    title: "Mark bots",
    description: "Also mark messages sent by bots",
    default: false,
  },
});

const DISCORD_EPOCH = 1420070400000n;
const DAY = 86_400_000;

const sentAt = (snowflake: string) => Number((BigInt(snowflake) >> 22n) + DISCORD_EPOCH);

phoenix.hook(gateway.messageCreate, {
  before(message) {
    if (!settings.showBots && message.author?.bot) return;
    const nonce = typeof message.nonce === "number" ? String(message.nonce) : message.nonce;
    if (!nonce || !/^\d{17,20}$/.test(nonce) || !/^\d{17,20}$/.test(message.id)) return;
    const delay = sentAt(message.id) - sentAt(nonce);
    if (delay < settings.threshold * 1000 || delay > DAY) return;
    message.content += `\n-# took ${(delay / 1000).toFixed(1)}s to send`;
  },
});
