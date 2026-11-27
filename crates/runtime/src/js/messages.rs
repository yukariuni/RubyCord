//! `phoenix.messages`, local-only messages.

use rquickjs::{
   Ctx,
   Exception,
   Function,
   Object,
   Value,
};
use serde_json::value::RawValue;

use crate::local;

/// `phoenix.messages`, local-only messages.
pub fn messages_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
   let messages = Object::new(ctx.clone())?;
   messages.set(
      "addLocal",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>,
          channel_id: String,
          content: String,
          options: rquickjs::function::Opt<Object<'js>>| {
            let (author, guild_id, embeds) = match options.0 {
               Some(options) => {
                  (
                     options.get::<_, Option<String>>("author")?,
                     options.get::<_, Option<String>>("guildId")?,
                     options.get::<_, Option<Value<'js>>>("embeds")?,
                  )
               },
               None => (None, None, None),
            };
            let embeds = match embeds {
               Some(embeds) => {
                  ctx.json_stringify(embeds)?
                     .map(|json| json.to_string())
                     .transpose()?
               },
               None => None,
            };
            let embeds = RawValue::from_string(embeds.unwrap_or_else(|| "[]".to_owned()))
               .map_err(|err| Exception::throw_type(&ctx, &format!("embeds: {err}")))?;
            let options = local::Options {
               author:   author.as_deref().unwrap_or("Phoenix"),
               guild_id: guild_id.as_deref(),
               embeds:   &embeds,
            };
            local::add(&channel_id, &content, &options)
               .ok_or_else(|| Exception::throw_message(&ctx, "this build has no MESSAGE_CREATE"))
         },
      )?,
   )?;
   messages.set(
      "updateLocal",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, message: Value<'js>| {
         let json = ctx
            .json_stringify(message)?
            .ok_or_else(|| Exception::throw_type(&ctx, "updateLocal needs a message object"))?
            .to_string()?;
         Ok::<_, rquickjs::Error>(local::update(json))
      })?,
   )?;
   messages.set(
      "deleteLocal",
      Function::new(ctx.clone(), |channel_id: String, id: String| {
         local::delete(&channel_id, &id)
      })?,
   )?;
   Ok(messages)
}
