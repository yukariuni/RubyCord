//! The code editor page and its syntax colors.

use phoenix_ui_mobile::{
   Prop,
   prop,
};
use phoenix_wire::Value;

use crate::{
   highlight::{
      self,
      Kind,
   },
   settings::{
      COLOR_COMMENT,
      COLOR_KEYWORD,
      COLOR_NUMBER,
      COLOR_STRING,
      COLOR_TEXT,
      Field,
      ICON_CONSOLE,
      ICON_RELOAD,
      Press,
      rows::{
         InputKind,
         button_row,
         section,
         text_field,
      },
      tree::{
         Tree,
         span,
         text,
      },
   },
};

/// Plugins larger than this are not shown in the code editor.
const MAX_EDIT: usize = 100 * 1024;

pub const fn code_color(kind: Kind) -> i64 {
   match kind {
      Kind::Plain => COLOR_TEXT,
      Kind::Comment => COLOR_COMMENT,
      Kind::String => COLOR_STRING,
      Kind::Keyword => COLOR_KEYWORD,
      Kind::Number => COLOR_NUMBER,
   }
}

/// `source` as highlighted spans.
pub fn code_spans(source: &str) -> Value {
   Value::Arr(
      highlight::js(source)
         .into_iter()
         .map(|(kind, run)| span(run, code_color(kind)))
         .collect(),
   )
}

/// The plugin's code in a multiline monospace field, and Save.
pub fn code_sections(index: usize, name: &str, source: Option<&str>) -> Vec<Tree> {
   let Some(source) = source.filter(|source| source.len() <= MAX_EDIT) else {
      let why = source.map_or_else(
         || "This plugin's code is not available".to_owned(),
         |source| {
            format!(
               "This plugin is {} KB, too large to edit here",
               source.len() / 1024
            )
         },
      );
      return vec![section("", vec![button_row(
         &why,
         ICON_CONSOLE,
         None,
         false,
      )])];
   };
   let mut editor = text_field(Field::Code(index), None, "", source, InputKind::Text);
   if let Some(input) = editor
      .kids
      .first_mut()
      .and_then(|frame| frame.kids.first_mut())
      .and_then(|row| row.kids.first_mut())
   {
      input.set(prop!("multiline"), Some(Value::Bool(true)));
      input.set(Prop::FONT_FAMILY, Some(text("monospace")));
      input.set(Prop::FONT_SIZE, Some(Value::F64(12.0)));
      input.set(prop!("minHeight"), Some(Value::F64(320.0)));
      input.set(Prop::RETURN_KEY, None);
      input.set(Prop::PLACEHOLDER, None);
      input.set(prop!("valueSpans"), Some(code_spans(source)));
   }
   vec![
      section(&format!("{name}.js"), vec![editor]),
      section("", vec![button_row(
         "Save and restart",
         ICON_RELOAD,
         Some(Press::SaveCode(index)),
         false,
      )]),
   ]
}
