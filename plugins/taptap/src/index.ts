// @description Double-tap a message to reply, or your own to edit
// @version 1.1.0
// @author mantikafasi

const settings = phoenix.settings.define({
  reply: {
    type: "switch",
    title: "Double-tap to reply",
    description: "Double-tap any message to start replying to it",
    default: true,
  },
  editOwn: {
    type: "switch",
    title: "Edit own messages",
    description: "Double-tap your own messages to edit them instead of replying",
    default: true,
  },
});

// Enable native rowDoubleTap events on chat message lists (applied in Rust, zero JS per frame).
phoenix.ui.patchProps(
  { prefix: "message.content:" },
  { addOn: ["rowDoubleTap"] },
  { target: "list" },
);

let pendingAction: string | null = null;
let pendingUntil = 0;

phoenix.ui.onEvent(
  "rowDoubleTap",
  (ev) => {
    if (!ev.messageId) return false;
    const isOwn =
      ev.currentUserId != null &&
      ev.message?.author?.id === ev.currentUserId;

    const action =
      isOwn && settings.editOwn
        ? "message.actions.edit"
        : settings.reply
          ? "message.actions.reply"
          : null;
    if (!action) return false;
    if (ev.press) {
      ev.press(action);
      return true;
    }

    pendingAction = action;
    pendingUntil = Date.now() + 2000;
    // Hide the transient action sheet synchronously in Rust so there is no visual flash.
    phoenix.ui.patchProps(
      "message.actions.root",
      { hidden: true, opacity: 0, scrimOpacity: 0 },
      { target: "sheet", once: true, timeoutMs: 2000 },
    );
    phoenix.ui.dispatch(
      ev.node,
      "rowLongPress",
      (ev.payload as Record<string, unknown>) ?? {},
    );
    return true;
  },
  { intercept: true },
);

phoenix.ui.onNode("message.actions.root", (node) => {
  if (!pendingAction) return;
  const action = pendingAction;
  pendingAction = null;
  if (Date.now() > pendingUntil) return;

  const btn = phoenix.ui.findDown(node.id, action, 8);
  if (btn != null) {
    phoenix.ui.dispatch(btn, "press");
  } else if (node.sheet != null) {
    phoenix.ui.dispatch(node.sheet, "dismiss");
  }
});
