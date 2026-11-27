//! `phoenix.bridge`, raw calls into Phoenix's host modules.

use std::str::FromStr;

use rquickjs::{
   Ctx,
   Exception,
   Function,
   Object,
   Value,
   function::Opt,
};

use crate::js::wire::js_to_wire;

/// `phoenix.bridge`, raw access to Phoenix's host modules (`GuestHost.invoke`).
pub fn bridge_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
   let obj = Object::new(ctx.clone())?;
   obj.set(
      "invoke",
      Function::new(
         ctx.clone(),
         |ctx: Ctx<'js>, module: u16, method: u16, args: Opt<Value<'js>>, body: Opt<String>| {
            let wire_args = match args.0 {
               Some(val) if !val.is_undefined() && !val.is_null() => js_to_host_wire(&ctx, &val)?,
               _ => phoenix_wire::Value::Obj(Vec::new()),
            };
            let call = phoenix_wire::HostCall {
               module,
               method,
               args: wire_args,
               body: body.0.map_or_else(Vec::new, String::into_bytes),
            };
            Ok::<_, rquickjs::Error>(crate::bridge::invoke(&call.encode()))
         },
      )?,
   )?;
   Ok(obj)
}

fn js_to_host_wire<'js>(
   ctx: &Ctx<'js>,
   value: &Value<'js>,
) -> rquickjs::Result<phoenix_wire::Value> {
   if let Some(object) = value.as_object()
      && !value.is_array()
   {
      let mut fields = Vec::new();
      for entry in object.props::<String, Value<'js>>() {
         let (name, field_value) = entry?;
         if field_value.is_undefined() {
            continue;
         }
         let key = if let Ok(num) = name.parse::<u16>() {
            phoenix_wire::Key::new(num)
         } else {
            phoenix_ui_mobile::Prop::from_str(&name)
               .map(phoenix_ui_mobile::Prop::key)
               .map_err(|_| {
                  Exception::throw_type(
                     ctx,
                     &format!(
                        "unknown host field key {name:?} (pass numeric hh.jb key or known prop \
                         name)"
                     ),
                  )
               })?
         };
         fields.push((key, js_to_host_wire(ctx, &field_value)?));
      }
      return Ok(phoenix_wire::Value::Obj(fields));
   }
   js_to_wire(ctx, value)
}
