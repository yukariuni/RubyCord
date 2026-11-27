// @description Opens a message's raw text and JSON in a viewer
// @version 1.2.1
// @author cmdr-chara

const show = (msg: Phoenix.Message) => {
  const json = JSON.stringify(msg, null, 2) ?? "null";
  const raw = msg.content ?? "";
  const author = msg.author?.global_name || msg.author?.username;
  const title = author ? `View Raw · ${author}` : "View Raw";

  const buttons: Phoenix.PageButton[] = [];
  if (raw.length > 0) {
    buttons.push({
      label: "Copy raw message",
      icon: "message.actions.copy_text",
      onPress() {
        phoenix.ui.copy(raw);
        phoenix.ui.toast("Copied raw message");
      },
    });
  }
  buttons.push({
    label: "Copy raw JSON",
    icon: "res://ic_mana_angle_brackets",
    onPress() {
      phoenix.ui.copy(json);
      phoenix.ui.toast("Copied raw JSON");
    },
  });
  buttons.push({
    label: "Copy message ID",
    icon: "message.actions.copy_id",
    onPress() {
      phoenix.ui.copy(msg.id);
      phoenix.ui.toast("Copied message ID");
    },
  });

  const sections: Phoenix.PageSection[] = [{ type: "buttons", title: "Actions", buttons }];
  if (raw.length > 0) {
    sections.push({
      type: "code",
      title: "Raw message",
      text: raw,
      monospace: true,
      highlight: false,
    });
  }
  sections.push({
    type: "code",
    title: "Raw JSON",
    text: json,
    monospace: true,
    highlight: true,
  });

  phoenix.ui.openPage({ title, sections });
};

phoenix.ui.addMessageAction({
  label: "View raw",
  icon: "res://ic_mana_angle_brackets",
  onPress(ctx) {
    if (ctx.message) {
      show(ctx.message);
      return;
    }
    if (!ctx.channelId || !ctx.messageId) {
      phoenix.ui.toast("Message is not cached, nothing to show");
      return;
    }
    phoenix.discord
      .get(`/channels/${ctx.channelId}/messages?around=${ctx.messageId}&limit=1`)
      .then((res) => {
        if (!res.ok) {
          phoenix.ui.toast(`Failed to fetch message (${res.status})`);
          return;
        }
        const list = res.json<Phoenix.Message[]>();
        const found = list.find((m) => m.id === ctx.messageId);
        if (!found) {
          phoenix.ui.toast("Message not found");
          return;
        }
        show(found);
      })
      .catch((err) => {
        phoenix.log.error("fetch message failed:", err);
        phoenix.ui.toast("Could not load raw message");
      });
  },
});
