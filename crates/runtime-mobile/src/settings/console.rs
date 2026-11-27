//! The console page and the colored log lines it shows.

use phoenix_ui_mobile::{
   Prop,
   prop,
   templates,
};
use phoenix_wire::Value;

use crate::{
   highlight,
   settings::{
      COLOR_ACCENT,
      COLOR_ERROR,
      COLOR_MUTED,
      COLOR_TAG,
      COLOR_TEXT,
      COLOR_WARN,
      Field,
      ICON_CONSOLE,
      Slot,
      code::code_color,
      rows::{
         InputKind,
         is_radius,
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

/// Lines the console page shows, newest first.
const LOG_LINES: usize = 150;

/// The newest kept lines, colored by part: time, `[plugin]` tag, level.
pub fn log_lines() -> Vec<Tree> {
   let lines = phoenix_runtime::console::lines();
   if lines.is_empty() {
      return vec![log_line(&[("Nothing logged yet".to_owned(), COLOR_MUTED)])];
   }
   lines
      .iter()
      .take(LOG_LINES)
      .map(|line| log_line(&line_spans(line)))
      .collect()
}

fn line_spans(line: &phoenix_runtime::console::Line) -> Vec<(String, i64)> {
   let mut spans = vec![(format!("{} ", line.time), COLOR_MUTED)];
   let body = match line.level {
      log::Level::Error => COLOR_ERROR,
      log::Level::Warn => COLOR_WARN,
      log::Level::Debug | log::Level::Trace => COLOR_MUTED,
      log::Level::Info => COLOR_TEXT,
   };
   let mut rest = line.text.as_str();
   // What the console ran, echoed as "> code": highlighted like the editor.
   if line.level == log::Level::Info
      && let Some(code) = rest.strip_prefix("> ")
   {
      spans.push(("> ".to_owned(), COLOR_ACCENT));
      spans.extend(
         highlight::js(code)
            .into_iter()
            .map(|(kind, run)| (run.to_owned(), code_color(kind))),
      );
      return spans;
   }
   if rest.starts_with('[')
      && let Some(end) = rest.find("] ")
   {
      spans.push((rest[..=end].to_owned(), COLOR_TAG));
      rest = &rest[end + 1..];
   }
   spans.push((rest.trim_end().to_owned(), body));
   spans
}

/// One small monospace console line, selectable for copying.
pub fn log_line(pieces: &[(String, i64)]) -> Tree {
   let mut node = Tree::from_template(&templates().header);
   node.set(Prop::ROLE, None);
   node.set(Prop::MARGIN, None);
   let whole = pieces
      .iter()
      .map(|(piece, _)| piece.as_str())
      .collect::<String>();
   node.set(Prop::TEXT, Some(text(&whole)));
   node.set(
      prop!("spans"),
      Some(Value::Arr(
         pieces
            .iter()
            .map(|(piece, color)| span(piece, *color))
            .collect(),
      )),
   );
   node.set(Prop::COLOR, Some(Value::I64(COLOR_TEXT)));
   node.set(Prop::FONT_SIZE, Some(Value::F64(12.0)));
   node.set(Prop::LINE_HEIGHT, Some(Value::F64(16.0)));
   node.set(prop!("fontWeight"), Some(Value::I32(400)));
   node.set(Prop::FONT_FAMILY, Some(text("monospace")));
   node.set(Prop::SELECTABLE, Some(Value::Bool(true)));
   node
}

pub fn console_sections() -> Vec<Tree> {
   let input_row = text_field(
      Field::Eval,
      Some(ICON_CONSOLE),
      "Evaluate JavaScript",
      "",
      InputKind::Text,
   );
   // A padded box the lines go into; [`Settings::log_ops`] fills it.
   let mut log_box = Tree::from_template(&templates().button_row);
   log_box.kids.clear();
   log_box.props.retain(|(key, _)| !is_radius(*key));
   for key in [
      Prop::ON,
      Prop::ROLE,
      Prop::ACCESSIBILITY_LABEL,
      Prop::ACCESSIBILITY_ID,
   ] {
      log_box.set(key, None);
   }
   log_box.set(Prop::PADDING, Some(Value::Rect([12.0, 10.0, 12.0, 10.0])));
   log_box.set(Prop::GAP, Some(Value::F64(2.0)));
   log_box.slot = Some(Slot::Log);
   vec![
      section("", vec![input_row]),
      section("Log, newest first", vec![log_box]),
   ]
}
