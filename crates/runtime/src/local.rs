//! Messages shown only on this device, fed to Phoenix's own message handlers.

use std::time::{
   SystemTime,
   UNIX_EPOCH,
};

use serde::Serialize;
use serde_json::value::RawValue;

use crate::gateway;

const DISCORD_EPOCH_MS: u64 = 1_420_070_400_000;
const EPHEMERAL: u32 = 1 << 6;

#[derive(Serialize)]
struct Author<'a> {
   id:            &'static str,
   username:      &'a str,
   global_name:   &'a str,
   discriminator: &'static str,
   avatar:        Option<()>,
   bot:           bool,
   public_flags:  u32,
}

#[derive(Serialize)]
struct LocalMessage<'a> {
   id:               &'a str,
   nonce:            &'a str,
   channel_id:       &'a str,
   #[serde(skip_serializing_if = "Option::is_none")]
   guild_id:         Option<&'a str>,
   content:          &'a str,
   #[serde(rename = "type")]
   kind:             u32,
   flags:            u32,
   tts:              bool,
   mention_everyone: bool,
   mentions:         [(); 0],
   mention_roles:    [(); 0],
   attachments:      [(); 0],
   embeds:           &'a RawValue,
   components:       [(); 0],
   pinned:           bool,
   timestamp:        &'a str,
   edited_timestamp: Option<()>,
   author:           Author<'a>,
}

#[derive(Serialize)]
struct Deleted<'a> {
   id:         &'a str,
   channel_id: &'a str,
}

/// Who a local message shows as, and where.
pub struct Options<'a> {
   pub author:   &'a str,
   pub guild_id: Option<&'a str>,
   /// A JSON array of embed objects.
   pub embeds:   &'a RawValue,
}

/// Shows `content` in `channel_id` as an ephemeral message from `author` and
/// returns its id. Nothing is sent to Discord.
#[must_use]
pub fn add(channel_id: &str, content: &str, options: &Options<'_>) -> Option<String> {
   let event = gateway::event("MESSAGE_CREATE")?;
   let now = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default();
   let millis = u64::try_from(now.as_millis()).unwrap_or(u64::MAX);
   let id = (millis.saturating_sub(DISCORD_EPOCH_MS) << 22).to_string();
   let timestamp = iso8601(now.as_secs(), now.subsec_millis());
   let message = LocalMessage {
      id: &id,
      nonce: &id,
      channel_id,
      guild_id: options.guild_id,
      content,
      kind: 0,
      flags: EPHEMERAL,
      tts: false,
      mention_everyone: false,
      mentions: [],
      mention_roles: [],
      attachments: [],
      embeds: options.embeds,
      components: [],
      pinned: false,
      timestamp: &timestamp,
      edited_timestamp: None,
      author: Author {
         id:            "0",
         username:      options.author,
         global_name:   options.author,
         discriminator: "0000",
         avatar:        None,
         bot:           true,
         public_flags:  0,
      },
   };
   let json = serde_json::to_string(&message).expect("a local message serializes");
   gateway::enqueue(event, json);
   Some(id)
}

/// Feeds `message`, a JSON message object, to Phoenix as an edit.
#[must_use]
pub fn update(message: String) -> bool {
   let Some(event) = gateway::event("MESSAGE_UPDATE") else {
      return false;
   };
   gateway::enqueue(event, message);
   true
}

#[must_use]
pub fn delete(channel_id: &str, id: &str) -> bool {
   let Some(event) = gateway::event("MESSAGE_DELETE") else {
      return false;
   };
   let json = serde_json::to_string(&Deleted { id, channel_id }).expect("a delete serializes");
   gateway::enqueue(event, json);
   true
}

/// `2026-11-22T12:30:00.000Z` for a Unix time.
fn iso8601(secs: u64, millis: u32) -> String {
   let days = secs / 86_400;
   let rest = secs % 86_400;
   // Howard Hinnant's civil_from_days, for days since 1970-01-01.
   let shifted = days + 719_468;
   let era = shifted / 146_097;
   let day_of_era = shifted % 146_097;
   let year_of_era =
      (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
   let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
   let month_index = (5 * day_of_year + 2) / 153;
   let day = day_of_year - (153 * month_index + 2) / 5 + 1;
   let month = if month_index < 10 {
      month_index + 3
   } else {
      month_index - 9
   };
   let year = year_of_era + era * 400 + u64::from(month <= 2);
   format!(
      "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
      rest / 3600,
      rest % 3600 / 60,
      rest % 60
   )
}
