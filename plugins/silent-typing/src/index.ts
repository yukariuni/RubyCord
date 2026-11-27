// @description Prevents Discord from sending typing indicators
// @version 1.0.0
// @author mantikafasi

phoenix.discord.onRequest(
  { method: "POST", pathPrefix: "/channels/", pathSuffix: "/typing" },
  () => false,
);
