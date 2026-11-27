// @description Stops your replies from pinging the person you reply to
// @version 0.1.0
// @author amaanq

interface AllowedMentions {
   parse?: string[];
   users?: string[];
   roles?: string[];
   replied_user?: boolean;
   [key: string]: unknown;
}

interface ReplyPayload extends Phoenix.SendPayload {
   message_reference?: Phoenix.MessageReference;
   allowed_mentions?: AllowedMentions;
}

// Discord pings the replied-to author unless allowed_mentions.replied_user is
// false. An absent allowed_mentions defaults to a ping, so a reply always needs
// the field set.
phoenix.discord.onRequest(
   { method: "POST", pathPrefix: "/channels/", pathSuffix: "/messages" },
   (req) => {
      const payload = req.json<ReplyPayload>();
      if (!payload?.message_reference) return;
      const mentions = payload.allowed_mentions ?? {};
      if (mentions.replied_user === false) return;
      payload.allowed_mentions = { ...mentions, replied_user: false };
      return { body: payload };
   },
);
