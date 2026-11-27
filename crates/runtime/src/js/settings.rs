//! `phoenix.settings`, the settings a plugin defines and what the user chose.

use rquickjs::{
   CatchResultExt,
   Ctx,
   Exception,
   Function,
   IntoJs,
   Object,
   Value,
   object::Accessor,
};

use crate::{
   Choice,
   Chosen,
   Control,
   js::{
      Engine,
      Registry,
      drain,
   },
};

impl Engine {
   pub fn set_setting(&self, plugin: &str, key: &str, value: Chosen) {
      self.context.with(|ctx| {
         let js_value = match &value {
            Chosen::Switch(on) => Ok(Value::new_bool(ctx.clone(), *on)),
            Chosen::Number(number) => Ok(Value::new_float(ctx.clone(), *number)),
            Chosen::Text(text) => {
               rquickjs::String::from_str(ctx.clone(), text).map(rquickjs::String::into_value)
            },
         };
         let callback = ctx.userdata::<Registry<'_>>().and_then(|registry| {
            registry
               .chosen
               .borrow_mut()
               .entry(plugin.to_owned())
               .or_default()
               .insert(key.to_owned(), value);
            redact_secrets(&registry);
            registry
               .defined
               .borrow()
               .get(plugin)
               .and_then(|list| list.iter().find(|definition| definition.key == key))
               .and_then(|definition| definition.on_change.clone())
         });
         if let Some(callback) = callback
            && let Ok(js_value) = js_value
            && let Err(err) = callback.call::<_, Value<'_>>((js_value,)).catch(&ctx)
         {
            log::error!("[{plugin}] {key} onChange: {err}");
         }
      });
      drain(&self.runtime);
      self.save_choices();
   }

   pub fn press(&self, plugin: &str, key: &str) {
      self.context.with(|ctx| {
         let handler = ctx.userdata::<Registry<'_>>().and_then(|registry| {
            registry
               .defined
               .borrow()
               .get(plugin)?
               .iter()
               .find(|definition| definition.key == key)
               .and_then(|definition| {
                  match &definition.kind {
                     Kind::Button(handler) => Some(handler.clone()),
                     _ => None,
                  }
               })
         });
         if let Some(handler) = handler
            && let Err(err) = handler.call::<_, ()>(()).catch(&ctx)
         {
            log::error!("[{plugin}] {key}: {err}");
         }
      });
      drain(&self.runtime);
   }
}

/// One setting a plugin defined with `phoenix.settings.define`.
pub struct Definition<'js> {
   pub key:         String,
   pub title:       String,
   pub description: Option<String>,
   pub kind:        Kind<'js>,
   /// A text setting edited over several lines.
   pub multiline:   bool,
   on_change:       Option<Function<'js>>,
}

/// A setting's type with its default, or a button's handler.
pub enum Kind<'js> {
   Switch(bool),
   Text(String),
   Secret(String),
   Select {
      options: Vec<Choice>,
      default: String,
   },
   Number(f64),
   Button(Function<'js>),
}

impl Kind<'_> {
   /// The user's choice when it has this kind's type, else the default.
   pub fn current(&self, chosen: Option<&Chosen>) -> Control {
      match (self, chosen) {
         (Self::Switch(_), Some(Chosen::Switch(on))) => Control::Switch(*on),
         (Self::Switch(default), _) => Control::Switch(*default),
         (Self::Text(_), Some(Chosen::Text(text))) => Control::Text(text.clone()),
         (Self::Text(default), _) => Control::Text(default.clone()),
         (Self::Secret(_), Some(Chosen::Text(text))) => Control::Secret(text.clone()),
         (Self::Secret(default), _) => Control::Secret(default.clone()),
         (Self::Select { options, default }, chosen) => {
            let chosen = match chosen {
               Some(Chosen::Text(value)) if options.iter().any(|choice| choice.value == *value) => {
                  value.clone()
               },
               _ => default.clone(),
            };
            Control::Select {
               options: options.clone(),
               chosen,
            }
         },
         (Self::Number(_), Some(Chosen::Number(number))) => Control::Number(*number),
         (Self::Number(default), _) => Control::Number(*default),
         (Self::Button(_), _) => Control::Button,
      }
   }
}

pub fn settings_object<'js>(ctx: &Ctx<'js>, plugin: &str) -> rquickjs::Result<Object<'js>> {
   let settings = Object::new(ctx.clone())?;
   let name = plugin.to_owned();
   settings.set(
      "define",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, schema: Object<'js>| {
         define(&ctx, &name, &schema)
      })?,
   )?;
   Ok(settings)
}

/// Hands every secret setting's current value to the logger to scrub.
fn redact_secrets(registry: &Registry<'_>) {
   let chosen = registry.chosen.borrow();
   let secrets = registry
      .defined
      .borrow()
      .iter()
      .flat_map(|(plugin, definitions)| {
         let picked = chosen.get(plugin);
         definitions.iter().filter_map(move |definition| {
            match definition
               .kind
               .current(picked.and_then(|values| values.get(&definition.key)))
            {
               Control::Secret(value) => Some(value),
               _ => None,
            }
         })
      })
      .collect::<Vec<String>>();
   crate::console::redact(secrets);
}

/// The kind of setting `key` that `spec` describes.
fn setting_kind<'js>(ctx: &Ctx<'js>, key: &str, spec: &Object<'js>) -> rquickjs::Result<Kind<'js>> {
   Ok(match spec.get::<_, String>("type")?.as_str() {
      "switch" => Kind::Switch(spec.get::<_, Option<bool>>("default")?.unwrap_or_default()),
      "text" => {
         Kind::Text(
            spec
               .get::<_, Option<String>>("default")?
               .unwrap_or_default(),
         )
      },
      "secret" => {
         Kind::Secret(
            spec
               .get::<_, Option<String>>("default")?
               .unwrap_or_default(),
         )
      },
      "select" => {
         let options = spec
            .get::<_, Vec<Object<'js>>>("options")?
            .into_iter()
            .map(|option| {
               let value: String = option.get("value")?;
               let label = option.get::<_, Option<String>>("label")?;
               Ok(Choice {
                  label: label.unwrap_or_else(|| value.clone()),
                  value,
               })
            })
            .collect::<rquickjs::Result<Vec<Choice>>>()?;
         let Some(first) = options.first() else {
            return Err(Exception::throw_type(
               ctx,
               &format!("setting {key}: a select needs at least one option"),
            ));
         };
         let default = spec
            .get::<_, Option<String>>("default")?
            .unwrap_or_else(|| first.value.clone());
         Kind::Select { options, default }
      },
      "number" => Kind::Number(spec.get::<_, Option<f64>>("default")?.unwrap_or_default()),
      "button" => Kind::Button(spec.get("onPress")?),
      other => {
         return Err(Exception::throw_type(
            ctx,
            &format!(
               "setting {key}: unknown type {other:?}, expected switch, text, secret, select, \
                number or button"
            ),
         ));
      },
   })
}

/// Records `schema` as the plugin's settings and returns an object with one
/// getter per non-button key that reads the user's current choice.
fn define<'js>(
   ctx: &Ctx<'js>,
   plugin: &str,
   schema: &Object<'js>,
) -> rquickjs::Result<Object<'js>> {
   let mut definitions = Vec::new();
   for entry in schema.props::<String, Object<'js>>() {
      let (key, spec) = entry?;
      let kind = setting_kind(ctx, &key, &spec)?;
      definitions.push(Definition {
         title: spec.get("title")?,
         description: spec.get("description")?,
         multiline: spec.get::<_, Option<bool>>("multiline")?.unwrap_or_default(),
         on_change: spec.get("onChange")?,
         kind,
         key,
      });
   }

   let values = Object::new(ctx.clone())?;
   for definition in &definitions {
      if matches!(definition.kind, Kind::Button(_)) {
         continue;
      }
      let (plugin, key) = (plugin.to_owned(), definition.key.clone());
      let read = move |ctx: Ctx<'js>| -> rquickjs::Result<Value<'js>> {
         let registry = ctx
            .userdata::<Registry<'js>>()
            .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?;
         let defined = registry.defined.borrow();
         let chosen = registry.chosen.borrow();
         let Some(definition) = defined
            .get(&plugin)
            .and_then(|list| list.iter().find(|definition| definition.key == key))
         else {
            return Ok(Value::new_undefined(ctx.clone()));
         };
         let picked = chosen.get(&plugin).and_then(|values| values.get(&key));
         match definition.kind.current(picked) {
            Control::Switch(on) => on.into_js(&ctx),
            Control::Text(text) | Control::Secret(text) => text.into_js(&ctx),
            Control::Select { chosen, .. } => chosen.into_js(&ctx),
            Control::Number(number) => number.into_js(&ctx),
            Control::Button => Ok(Value::new_undefined(ctx.clone())),
         }
      };
      values.prop(
         definition.key.as_str(),
         Accessor::new_get(read).enumerable(),
      )?;
   }
   ctx.userdata::<Registry<'js>>()
      .ok_or_else(|| Exception::throw_internal(ctx, "no registry"))?
      .defined
      .borrow_mut()
      .insert(plugin.to_owned(), definitions);
   if let Some(registry) = ctx.userdata::<Registry<'js>>() {
      redact_secrets(&registry);
   }
   Ok(values)
}
