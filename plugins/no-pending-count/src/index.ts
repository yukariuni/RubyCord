// @description Hides the badge counts of incoming friend requests and message requests (Desktop)
// @version 0.1.0
// @author 0Allu

type Count = "friendRequests" | "messageRequests";

const shown = new Map<Count, Phoenix.Remover>();

const sync = (count: Count, hide: boolean) => {
  const restore = shown.get(count);
  if (hide && !restore && phoenix.ui.hideCount) {
    shown.set(count, phoenix.ui.hideCount(count));
  } else if (!hide && restore) {
    restore();
    shown.delete(count);
  }
};

const settings = phoenix.settings.define({
  friendRequests: {
    type: "switch",
    title: "Hide friend requests count",
    description: "On the Home button, the Friends row and the inbox's Friends tab",
    default: true,
    onChange: (on) => sync("friendRequests", on),
  },
  messageRequests: {
    type: "switch",
    title: "Hide message requests count",
    description: "On the Message Requests row, which still shows while there are requests",
    default: true,
    onChange: (on) => sync("messageRequests", on),
  },
});

if (!phoenix.ui.hideCount) phoenix.log.warn("hiding counts needs Desktop Phoenix");
sync("friendRequests", settings.friendRequests);
sync("messageRequests", settings.messageRequests);
