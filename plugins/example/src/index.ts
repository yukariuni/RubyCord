// @description Demo plugin covering settings, UI patches, and slash commands
// @version 0.1.1
// @author amaanq
// @enabled false
// @unlisted

const { gateway } = phoenix;

const settings = phoenix.settings.define({
  shout: { type: "switch", title: "Shout", description: "Upper-case incoming messages", default: true },
  suffix: { type: "text", title: "Suffix", description: "Appended to each message", default: "[phoenix]" },
  every: { type: "number", title: "Every nth message", description: "Change 1 in this many", default: 1 },
  reset: {
    type: "button",
    title: "Reset counter",
    onPress() {
      phoenix.storage.set("seen", 0);
      phoenix.log.info("counter reset");
    },
  },
});

phoenix.hook(gateway.messageCreate, {
  before(message) {
    const seen = (phoenix.storage.get<number>("seen") ?? 0) + 1;
    phoenix.storage.set("seen", seen);
    if (seen % Math.max(1, Math.round(settings.every)) !== 0) return;
    const content = settings.shout ? message.content.toUpperCase() : message.content;
    message.content = `${content} ${settings.suffix} #${seen}`;
  },
});

phoenix.ui.addHeaderButton({
  icon: "res://ic_mana_puzzle_piece_plus",
  label: "Example",
  onPress: (ctx) => ctx.reply(`header button pressed in ${ctx.channelId}`),
});

phoenix.ui.addComposerButton({
  icon: "res://ic_mana_angle_brackets",
  label: "Shrug",
  onPress: (ctx) => ctx.reply("¯\\_(ツ)_/¯"),
});

phoenix.ui.addMessageAccessory({
  placement: "header",
  render: ({ messageId }) => `#${messageId.slice(-4)}`,
});

phoenix.ui.addMessageAction({
  label: "Copy id to chat",
  icon: "res://ic_mana_id",
  onPress: (ctx) => ctx.reply(`message ${ctx.messageId} by ${ctx.message?.author.username ?? "someone"}`),
});

phoenix.ui.addProfileSection({
  title: "Phoenix",
  render: ({ userId }) => (userId ? `user ${userId}` : null),
});

phoenix.commands.register({
  name: "echo",
  description: "Sends its arguments back to you only",
  execute: (args, ctx) => {
    ctx.reply(args || "(nothing)");
  },
});

phoenix.messages.onSend((text) => text.replace(/\bteh\b/g, "the"));
