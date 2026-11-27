// @description Translates messages from the message menu, or every message in selected channels, using Kagi or Google
// @version 0.3.0
// @author amaanq

const { gateway } = phoenix;

const settings = phoenix.settings.define({
  channels: {
    type: "text",
    title: "Channels",
    description: "Channel IDs to translate, separated by commas",
    default: "",
  },
  target: {
    type: "text",
    title: "Target language",
    description: "Language code to translate into, like en or de",
    default: "en",
  },
  service: {
    type: "select",
    title: "Service",
    options: [
      { value: "google", label: "Google Translate" },
      { value: "kagi", label: "Kagi, standard quality" },
      { value: "kagi-best", label: "Kagi, best quality (about 10x the usage)" },
    ],
  },
  kagiToken: {
    type: "secret",
    title: "Kagi session token",
    description: "The token from the Session Link in kagi.com/settings/user_details",
    default: "",
  },
});

interface Translation {
  text: string;
  from: string;
  label: string;
}

type Translator = (text: string, target: string) => Promise<Translation>;

const GOOGLE_KEY = "AIzaSyDLEeFI5OtFBwYBIoK_jj5m32rZK5CkCXA";

// Google answers with a code like "id"; this turns it into "Indonesian".
let languageNames: Promise<Map<string, string>> | undefined;
const languageName = async (code: string) => {
  languageNames ??= phoenix
    .fetch(`https://translate-pa.googleapis.com/v1/supportedLanguages?client=gtx&display_language=en&key=${GOOGLE_KEY}`)
    .then((response) => {
      const { sourceLanguages = [] } = response.json<{ sourceLanguages?: { language: string; name: string }[] }>() ?? {};
      return new Map(sourceLanguages.map(({ language, name }) => [language, name]));
    })
    .catch(() => {
      languageNames = undefined;
      return new Map<string, string>();
    });
  return (await languageNames).get(code) ?? code;
};

const google: Translator = async (text, target) => {
  const query = Object.entries({
    "params.client": "gtx",
    dataTypes: "TRANSLATION",
    key: GOOGLE_KEY,
    "query.sourceLanguage": "auto",
    "query.targetLanguage": target,
    "query.text": text,
  })
    .map(([name, value]) => `${name}=${encodeURIComponent(value)}`)
    .join("&");
  const response = await phoenix.fetch(`https://translate-pa.googleapis.com/v1/translate?${query}`);
  if (!response.ok) throw new Error(`Google answered ${response.status}`);

  const { translation, sourceLanguage } = response.json<{ translation: string; sourceLanguage: string }>();
  return { text: translation, from: sourceLanguage, label: await languageName(sourceLanguage) };
};

const kagi =
  (token: string, best: boolean): Translator =>
  async (text, target) => {
    const response = await phoenix.fetch(
      `https://translate.kagi.com/api/translate?token=${encodeURIComponent(token)}`,
      {
        method: "POST",
        body: {
          text,
          source_lang: "auto",
          target_lang: target,
          skip_definition: true,
          model: best ? "best" : "standard",
        },
      },
    );
    switch (response.status) {
      case 200:
        break;
      case 429:
        phoenix.log.warn("Kagi quota exceeded, using Google");
        return google(text, target);
      case 401:
      case 403:
        throw new Error("Kagi rejected the session token");
      default:
        throw new Error(`Kagi answered ${response.status}: ${response.text()}`);
    }

    const { translation, detected_language: detected } = response.json<{
      translation: string;
      detected_language?: { iso: string; label: string };
    }>();
    return { text: translation, from: detected?.iso ?? "", label: detected?.label ?? "" };
  };

const withToken =
  (best: boolean): Translator =>
  (text, target) => {
    const token = settings.kagiToken.trim();
    if (token) return kagi(token, best)(text, target);
    phoenix.log.warn("Kagi is selected without a session token, using Google");
    return google(text, target);
  };

const translators: Record<typeof settings.service, Translator> = {
  google,
  kagi: withToken(false),
  "kagi-best": withToken(true),
};

const baseLanguage = (code: string) => code.split("-")[0].toLowerCase();

// Notes under messages by id, kept to the most recent 512.
const notes = new Map<string, string>();

const note = phoenix.ui.addMessageAccessory({
  render: ({ messageId }) => notes.get(messageId) ?? null,
});

const showUnder = (id: string, text: string) => {
  notes.delete(id);
  notes.set(id, text);
  if (notes.size > 512) notes.delete(notes.keys().next().value!);
  note.update?.(id);
};

/// Translates `content` and shows the result under message `id`. With
/// `quiet`, a message already in the target language is left as it is.
const translateMessage = async (id: string, content: string, quiet: boolean) => {
  if (!content.trim()) return;
  const target = settings.target.trim() || "en";
  const translation = await translators[settings.service](content.trim(), target);
  if (baseLanguage(translation.from) === baseLanguage(target) || translation.text.trim() === content.trim()) {
    if (!quiet) showUnder(id, `Already in ${await languageName(baseLanguage(target))}`);
    return;
  }
  showUnder(id, `${translation.text} (${translation.label})`);
};

phoenix.on(gateway.messageCreate, async (message) => {
  const channels = settings.channels.split(",").map((id) => id.trim());
  if (!channels.includes(message.channel_id)) return;
  try {
    await translateMessage(message.id, message.content ?? "", true);
  } catch (err) {
    phoenix.log.warn(`translating ${message.id}: ${err}`);
  }
});

phoenix.ui.addMessageAction({
  label: "Translate",
  visible: (ctx) => !!ctx.message?.content?.trim(),
  onPress(ctx) {
    const message = ctx.message;
    if (!message) return;
    translateMessage(message.id, message.content ?? "", false).catch((err) => {
      showUnder(message.id, `Translation failed: ${err instanceof Error ? err.message : err}`);
    });
  },
});
