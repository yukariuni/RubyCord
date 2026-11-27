//! Conversions between JS values and Phoenix's tagged wire values.

use std::str::FromStr;

use rquickjs::{
   Ctx,
   Exception,
   IntoJs,
   Object,
   Value,
};

fn is_float_prop(name: &str) -> bool {
   matches!(
      name,
      "opacity"
         | "scrimOpacity"
         | "width"
         | "height"
         | "minWidth"
         | "maxWidth"
         | "minHeight"
         | "maxHeight"
         | "radius"
         | "radiusTopLeft"
         | "radiusTopRight"
         | "radiusBottomLeft"
         | "radiusBottomRight"
         | "fontSize"
         | "lineHeight"
         | "gap"
         | "gapX"
         | "gapY"
         | "flex"
         | "scale"
         | "scaleX"
         | "scaleY"
         | "translateX"
         | "translateY"
         | "rotation"
         | "elevation"
         | "borderWidth"
         | "hitSlop"
   )
}

fn is_rect_prop(name: &str) -> bool {
   matches!(
      name,
      "padding"
         | "margin"
         | "activePadding"
         | "activeMargin"
         | "rangeInsets"
         | "scrollIndicatorInsets"
   )
}

#[expect(clippy::cast_possible_truncation, reason = "Phoenix rects hold f32")]
fn parse_rect<'js>(value: &Value<'js>) -> Option<phoenix_wire::Value> {
   if let Some(num) = value.as_number() {
      let v = num as f32;
      return Some(phoenix_wire::Value::Rect([v, v, v, v]));
   }
   let arr = value.as_array()?;
   if arr.len() != 4 {
      return None;
   }
   let mut out = [0.0f32; 4];
   for (i, slot) in out.iter_mut().enumerate() {
      *slot = arr.get::<Value<'js>>(i).ok()?.as_number()? as f32;
   }
   Some(phoenix_wire::Value::Rect(out))
}

pub fn parse_props<'js>(
   ctx: &Ctx<'js>,
   object: &Object<'js>,
   skip: Option<&str>,
) -> rquickjs::Result<phoenix_ui_mobile::Props> {
   let mut out = Vec::new();
   for entry in object.props::<String, Value<'js>>() {
      let (name, value) = entry?;
      if skip == Some(name.as_str()) || value.is_undefined() {
         continue;
      }
      let prop = phoenix_ui_mobile::Prop::from_str(&name)
         .map_err(|_| Exception::throw_type(ctx, &format!("unknown Phoenix prop {name:?}")))?;
      let wire = if is_rect_prop(&name)
         && let Some(rect) = parse_rect(&value)
      {
         rect
      } else if is_float_prop(&name)
         && let Some(num) = value.as_number()
      {
         phoenix_wire::Value::F64(num)
      } else {
         js_to_wire(ctx, &value)?
      };
      out.push((prop, wire));
   }
   Ok(out)
}

pub fn js_to_wire<'js>(
   ctx: &Ctx<'js>,
   value: &Value<'js>,
) -> rquickjs::Result<phoenix_wire::Value> {
   if value.is_null() || value.is_undefined() {
      return Ok(phoenix_wire::Value::Null);
   }
   if let Some(flag) = value.as_bool() {
      return Ok(phoenix_wire::Value::Bool(flag));
   }
   if let Some(int) = value.as_int() {
      return Ok(phoenix_wire::Value::I32(int));
   }
   if let Some(num) = value.as_float() {
      if num.fract() == 0.0 && num > f64::from(i32::MAX) && num <= 2.0_f64.powi(53) {
         #[expect(
            clippy::cast_possible_truncation,
            reason = "checked finite integer range"
         )]
         return Ok(phoenix_wire::Value::I64(num as i64));
      }
      return Ok(phoenix_wire::Value::F64(num));
   }
   if let Some(text) = value.as_string() {
      return Ok(phoenix_wire::Value::Str(text.to_string()?));
   }
   if let Some(array) = value.as_array() {
      let mut items = Vec::with_capacity(array.len());
      for item in array.iter::<Value<'js>>() {
         items.push(js_to_wire(ctx, &item?)?);
      }
      return Ok(phoenix_wire::Value::Arr(items));
   }
   if let Some(object) = value.as_object() {
      let mut fields = Vec::new();
      for entry in object.props::<String, Value<'js>>() {
         let (name, field_value) = entry?;
         if field_value.is_undefined() {
            continue;
         }
         let prop = phoenix_ui_mobile::Prop::from_str(&name).map_err(|_| {
            Exception::throw_type(ctx, &format!("unknown Phoenix prop/field {name:?}"))
         })?;
         fields.push((prop.key(), js_to_wire(ctx, &field_value)?));
      }
      return Ok(phoenix_wire::Value::Obj(fields));
   }
   Ok(phoenix_wire::Value::Null)
}

#[expect(clippy::cast_precision_loss, reason = "JS numbers are f64")]
pub fn wire_to_js<'js>(
   ctx: &Ctx<'js>,
   value: &phoenix_wire::Value,
) -> rquickjs::Result<Value<'js>> {
   match value {
      phoenix_wire::Value::Null | phoenix_wire::Value::WPatch { .. } => {
         Ok(Value::new_null(ctx.clone()))
      },
      phoenix_wire::Value::Bool(flag) => Ok(Value::new_bool(ctx.clone(), *flag)),
      phoenix_wire::Value::I32(int) => Ok(Value::new_int(ctx.clone(), *int)),
      phoenix_wire::Value::I64(int) => Ok(Value::new_float(ctx.clone(), *int as f64)),
      phoenix_wire::Value::F64(num) => Ok(Value::new_float(ctx.clone(), *num)),
      phoenix_wire::Value::Str(text) => text.as_str().into_js(ctx),
      phoenix_wire::Value::Arr(items) => {
         let arr = rquickjs::Array::new(ctx.clone())?;
         for (idx, item) in items.iter().enumerate() {
            arr.set(idx, wire_to_js(ctx, item)?)?;
         }
         Ok(arr.into_value())
      },
      phoenix_wire::Value::Obj(fields) => {
         let obj = Object::new(ctx.clone())?;
         for (key, val) in fields {
            let name = phoenix_ui_mobile::PROPS
               .get(usize::from(key.get()))
               .map_or_else(|| key.get().to_string(), |name| (*name).to_owned());
            obj.set(name, wire_to_js(ctx, val)?)?;
         }
         Ok(obj.into_value())
      },
      phoenix_wire::Value::Map(entries) => {
         let obj = Object::new(ctx.clone())?;
         for (key, val) in entries {
            obj.set(key.as_str(), wire_to_js(ctx, val)?)?;
         }
         Ok(obj.into_value())
      },
      phoenix_wire::Value::Rect(rect) => {
         let arr = rquickjs::Array::new(ctx.clone())?;
         for (idx, val) in rect.iter().enumerate() {
            arr.set(idx, f64::from(*val))?;
         }
         Ok(arr.into_value())
      },
      phoenix_wire::Value::Words(words) => words.clone().into_js(ctx),
   }
}
