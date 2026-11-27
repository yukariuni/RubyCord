//! Phoenix's accessibility ids that plugin UI anchors to, as of versionCode
//! 356207.

pub const SEARCH_ENTRY: &str = "search.entry";
pub const COMPOSER_EXPRESSIONS: &str = "composer.expressions";
pub const MESSAGE_ACTIONS: &str = "message.actions.root";
pub const MESSAGE_DELETE: &str = "message.actions.delete";
pub const PROFILE_TAB_PAGES: &str = "profile-tab-pages";
pub const SUGGESTIONS: &str = "suggestions";
pub const AVATAR: &str = "Avatar";
/// The emoji picker under the message box. Reactions open a sheet of their own.
pub const EMOJI_SHEET: &str = "xp-sheet";
pub const MESSAGE_ROW_PREFIX: &str = "message.actions.";

/// The part of a message a `message.<part>:<id>` id names.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MessagePart {
   Content,
   Author,
   Avatar,
   Timestamp,
}

/// Splits a `message.<part>:<id>` id into its part and the message's id.
pub fn message_part(aid: &str) -> Option<(MessagePart, &str)> {
   let (part, id) = aid.strip_prefix("message.")?.split_once(':')?;
   let part = match part {
      "content" => MessagePart::Content,
      "author" => MessagePart::Author,
      "avatar" => MessagePart::Avatar,
      "timestamp" => MessagePart::Timestamp,
      _ => return None,
   };
   (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())).then_some((part, id))
}

/// The message id in any `message.<part>:<id>` id.
pub fn message_id(aid: &str) -> Option<&str> {
   message_part(aid).map(|(_, id)| id)
}

/// A row in a channel's member list, `channel.details.member:<user id>`.
pub const MEMBER_ROW_PREFIX: &str = "channel.details.member:";
/// A row in the DM list, `dm.row:<channel id>`.
pub const DM_ROW_PREFIX: &str = "dm.row:";

/// The snowflake in `aid` after `prefix`.
pub fn snowflake<'a>(aid: &'a str, prefix: &str) -> Option<&'a str> {
   let id = aid.strip_prefix(prefix)?;
   (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())).then_some(id)
}
