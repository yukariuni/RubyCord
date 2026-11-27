// @description Adds a toggle to automatically spoiler everything you send
// @version 1.2.1
// @author cmdr-chara

const KEY = "enabled";
const LIMIT = 2000;
const ICON = "res://ic_mana_image_spoiler";
const TINT_ON = 0xff5865f2;

let on = phoenix.storage.get<boolean>(KEY) ?? false;

const syncButton = () => {
  button.update({
    icon: ICON,
    label: on ? "Spoiler mode (on)" : "Spoiler mode (off)",
    tint: on ? TINT_ON : null,
  });
};

const settings = phoenix.settings.define({
  enabled: {
    type: "switch",
    title: "Spoiler mode",
    description: "Wrap outgoing messages and attachments in spoilers automatically",
    default: on,
    onChange(value) {
      on = value;
      phoenix.storage.set(KEY, on);
      syncButton();
    },
  },
});

on = settings.enabled;

const wrapText = (text: string): string | undefined => {
  if (!text.trim() || text.includes("||")) return;
  if (text.length + 4 > LIMIT) {
    phoenix.ui.toast("Message too long to wrap in spoilers");
    return;
  }
  return `||${text}||`;
};

const button = phoenix.ui.addComposerButton({
  icon: ICON,
  label: on ? "Spoiler mode (on)" : "Spoiler mode (off)",
  tint: on ? TINT_ON : undefined,
  onPress() {
    on = !on;
    phoenix.storage.set(KEY, on);
    syncButton();
    phoenix.ui.toast(on ? "Spoiler mode on" : "Spoiler mode off");
  },
});

phoenix.messages.onSend((text) => {
  if (!on) return;
  return wrapText(text);
});

phoenix.discord.onRequest(
  { method: "POST", pathPrefix: "/channels/", pathSuffix: "/messages" },
  (req) => {
    if (!on) return;
    const payload = req.json<Phoenix.SendPayload>();
    if (!payload) return;
    let changed = false;
    if (typeof payload.content === "string") {
      const wrapped = wrapText(payload.content);
      if (wrapped !== undefined) {
        payload.content = wrapped;
        changed = true;
      }
    }
    if (Array.isArray(payload.attachments)) {
      for (const attachment of payload.attachments) {
        if (typeof attachment.filename === "string" && !attachment.filename.startsWith("SPOILER_")) {
          attachment.filename = `SPOILER_${attachment.filename}`;
          changed = true;
        }
      }
    }
    return changed ? { body: payload } : undefined;
  },
);
