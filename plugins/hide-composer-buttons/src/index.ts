// @description Hides extra buttons in the message bar (Gift, Voice Message, etc.)
// @version 1.0.0
// @author mantikafasi

const GIFT_ID = "composer.gift";
const VOICE_ID = "Record Voice Message";

let hideGift: Phoenix.Remover | null = null;
let hideVoice: Phoenix.Remover | null = null;

const syncGift = (on: boolean) => {
  if (on && !hideGift) hideGift = phoenix.ui.hide(GIFT_ID);
  else if (!on && hideGift) {
    hideGift();
    hideGift = null;
  }
};

const syncVoice = (on: boolean) => {
  if (on && !hideVoice) hideVoice = phoenix.ui.hide(VOICE_ID);
  else if (!on && hideVoice) {
    hideVoice();
    hideVoice = null;
  }
};

const settings = phoenix.settings.define({
  gift: {
    type: "switch",
    title: "Hide Gift button",
    description: "Remove the Nitro Gift button from the chat composer",
    default: true,
    onChange: syncGift,
  },
  voice: {
    type: "switch",
    title: "Hide Voice Message button",
    description: "Remove the Record Voice Message button from the chat composer",
    default: false,
    onChange: syncVoice,
  },
});

syncGift(settings.gift);
syncVoice(settings.voice);
