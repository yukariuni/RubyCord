//! `phoenix.fetch` and `phoenix.discord`.

use rquickjs::{
   Ctx,
   Exception,
   Function,
   Object,
   Value,
   function::Opt,
};

use crate::{
   discord,
   fetch,
   js::{
      background::{
         Settlement,
         in_background,
      },
      intercept::add_discord_hooks,
      state,
   },
   store,
};

/// `{status, ok, text(), json()}`, a subset of the web's `Response`.
pub fn response_object<'js>(
   ctx: &Ctx<'js>,
   response: &fetch::Response,
) -> rquickjs::Result<Object<'js>> {
   let object = Object::new(ctx.clone())?;
   object.set("status", response.status)?;
   object.set("ok", (200..300).contains(&response.status))?;
   let text = String::from_utf8_lossy(&response.body).into_owned();
   let json_text = text.clone();
   object.set("text", Function::new(ctx.clone(), move || text.clone())?)?;
   object.set(
      "json",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
         ctx.json_parse(json_text.clone())
      })?,
   )?;
   Ok(object)
}

/// A fetch-style `{method, headers, body}`. A body that is not a string is
/// sent as JSON.
struct Init {
   method:  String,
   headers: Vec<(String, String)>,
   body:    Option<Vec<u8>>,
}

impl Init {
   fn read<'js>(ctx: &Ctx<'js>, init: Option<Object<'js>>) -> rquickjs::Result<Self> {
      let mut out = Self {
         method:  "GET".to_owned(),
         headers: Vec::new(),
         body:    None,
      };
      let Some(init) = init else {
         return Ok(out);
      };
      if let Some(method) = init.get::<_, Option<String>>("method")? {
         out.method = method.to_ascii_uppercase();
      }
      if let Some(headers) = init.get::<_, Option<Object<'js>>>("headers")? {
         out.headers = headers
            .props::<String, String>()
            .collect::<rquickjs::Result<_>>()?;
      }
      out.set_body(ctx, init.get("body")?)?;
      Ok(out)
   }

   fn set_body<'js>(&mut self, ctx: &Ctx<'js>, body: Value<'js>) -> rquickjs::Result<()> {
      if body.is_undefined() || body.is_null() {
         return Ok(());
      }
      if let Some(text) = body.as_string() {
         self.body = Some(text.to_string()?.into_bytes());
         return Ok(());
      }
      if !self
         .headers
         .iter()
         .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
      {
         self
            .headers
            .push(("Content-Type".to_owned(), "application/json".to_owned()));
      }
      self.body = ctx
         .json_stringify(body)?
         .map(|json| json.to_string())
         .transpose()?
         .map(String::into_bytes);
      Ok(())
   }
}

/// Sends the request on its own thread, so neither the gateway nor the UI
/// waits on the network, and returns a promise that settles when it answers.
pub fn start_fetch<'js>(
   ctx: &Ctx<'js>,
   url: String,
   init: Option<Object<'js>>,
) -> rquickjs::Result<rquickjs::Promise<'js>> {
   let Init {
      method,
      headers,
      body,
   } = Init::read(ctx, init)?;
   let request = fetch::Request {
      url,
      method,
      headers,
      body,
   };
   in_background(ctx, move || Settlement::Fetch(fetch::send(&request)))
}

/// `phoenix.discord`, Discord's API on the app's own client and session:
/// `request(path, init)`, `get` and `delete(path, init)`, `post`, `put` and
/// `patch(path, body, init)`, and `onRequest(filter, handler)`.
pub fn discord_object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   fn send<'js>(
      ctx: &Ctx<'js>,
      path: String,
      init: Init,
   ) -> rquickjs::Result<rquickjs::Promise<'js>> {
      in_background(ctx, move || {
         Settlement::Discord(discord::send(
            &init.method,
            &path,
            init.headers,
            init.body.unwrap_or_default(),
         ))
      })
   }

   let object = Object::new(ctx.clone())?;
   add_discord_hooks(ctx, plugin, &object)?;
   object.set(
      "authorize",
      Function::new(ctx.clone(), |ctx: Ctx<'js>, options: Object<'js>| {
         let client_id = options.get::<_, Option<String>>("clientId")?;
         let redirect_uri = options.get::<_, Option<String>>("redirectUri")?;
         let (Some(client_id), Some(redirect_uri)) = (client_id, redirect_uri) else {
            return Err(Exception::throw_type(
               &ctx,
               "authorize needs clientId and redirectUri",
            ));
         };
         let request = discord::Authorization {
            client_id,
            redirect_uri,
            scopes: options
               .get::<_, Option<Vec<String>>>("scopes")?
               .unwrap_or_else(|| vec!["identify".to_owned()]),
            response_type: options
               .get::<_, Option<String>>("responseType")?
               .unwrap_or_else(|| "code".to_owned()),
         };
         in_background(&ctx, move || Settlement::Text(discord::authorize(&request)))
      })?,
   )?;
   object.set(
      "me",
      Function::new(ctx.clone(), |ctx: Ctx<'js>| {
         let files = state().files_dir();
         in_background(&ctx, move || {
            Settlement::Json(store::database(&files).and_then(|db| store::meta(&db, "me")))
         })
      })?,
   )?;
   object.set(
      "request",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, path: String, init: Opt<Object<'js>>| {
            send(&ctx, path, Init::read(&ctx, init.0)?)
         },
      )?,
   )?;
   for verb in ["GET", "DELETE"] {
      object.set(
         verb.to_ascii_lowercase(),
         Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, path: String, init: Opt<Object<'js>>| {
               let mut init = Init::read(&ctx, init.0)?;
               verb.clone_into(&mut init.method);
               send(&ctx, path, init)
            },
         )?,
      )?;
   }
   for verb in ["POST", "PUT", "PATCH"] {
      object.set(
         verb.to_ascii_lowercase(),
         Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, path: String, body: Opt<Value<'js>>, init: Opt<Object<'js>>| {
               let mut init = Init::read(&ctx, init.0)?;
               verb.clone_into(&mut init.method);
               if let Some(body) = body.0 {
                  init.set_body(&ctx, body)?;
               }
               send(&ctx, path, init)
            },
         )?,
      )?;
   }
   Ok(object)
}
