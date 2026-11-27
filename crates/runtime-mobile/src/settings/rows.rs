//! Sections, rows and text fields built from Phoenix's captured layouts.

use phoenix_ui_mobile::{
   Prop,
   Props,
   motion::Worklet,
   templates,
};
use phoenix_wire::Value;

use crate::settings::{
   Field,
   ICON_CHECK,
   ICON_PLUGIN,
   Press,
   Slot,
   tree::{
      Animate,
      Tree,
      text,
   },
};

const RADII: [Prop; 5] = [
   Prop::RADIUS,
   Prop::RADIUS_TOP_LEFT,
   Prop::RADIUS_TOP_RIGHT,
   Prop::RADIUS_BOTTOM_LEFT,
   Prop::RADIUS_BOTTOM_RIGHT,
];

pub fn is_radius(key: Prop) -> bool {
   RADII.contains(&key)
}

pub fn section(title: &str, rows: Vec<Tree>) -> Tree {
   let layouts = templates();
   let mut section = Tree::from_template(&layouts.section);
   let mut group = Tree::from_template(&layouts.group);
   group.kids = rows
      .into_iter()
      .enumerate()
      .flat_map(|(index, row)| {
         let divider = (index > 0).then(|| Tree::from_template(&layouts.divider));
         divider.into_iter().chain([row])
      })
      .collect();
   if !title.is_empty() {
      let mut header = Tree::from_template(&layouts.header);
      header.set(Prop::TEXT, Some(text(title)));
      section.kids.push(header);
   }
   section.kids.push(group);
   section
}

/// `root > row > [icon, text, chevron]`
pub fn button_row(title: &str, icon: &str, press: Option<Press>, chevron: bool) -> Tree {
   let mut root = Tree::from_template(&templates().button_row);
   root.props.retain(|(key, _)| !is_radius(*key));
   root.set(Prop::ACCESSIBILITY_LABEL, Some(text(title)));
   root.set(
      Prop::ACCESSIBILITY_ID,
      Some(text(&format!("phoenix-{title}"))),
   );
   if let Some(row) = root.kids.first_mut() {
      if let Some(icon_node) = row.kids.get_mut(0) {
         icon_node.set(Prop::URI, Some(text(icon)));
      }
      if let Some(label) = row.kids.get_mut(1) {
         label.set(Prop::TEXT, Some(text(title)));
      }
      if !chevron {
         row.kids.truncate(2);
      }
   }
   if press.is_none() {
      root.set(Prop::ON, None);
      root.set(Prop::ROLE, None);
   }
   root.press = press;
   root
}

/// A button row without an icon, with a checkmark in the chevron's place when
/// `selected`.
pub fn select_row(label: &str, selected: bool, press: Press) -> Tree {
   let mut root = button_row(label, ICON_PLUGIN, Some(press), selected);
   if let Some(row) = root.kids.first_mut() {
      row.kids.remove(0);
      if let Some(check) = row.kids.get_mut(1).and_then(|wrap| wrap.kids.first_mut()) {
         check.set(Prop::URI, Some(text(ICON_CHECK)));
      }
   }
   root.set(Prop::ACCESSIBILITY_CHECKED, Some(Value::Bool(selected)));
   root
}

/// `root > row > [label, value wrap > value, chevron]`
pub fn value_row(label: &str, value: &str) -> Tree {
   let mut root = Tree::from_template(&templates().value_row);
   root.props.retain(|(key, _)| !is_radius(*key));
   root.set(Prop::ON, None);
   root.set(Prop::ROLE, None);
   root.set(
      Prop::ACCESSIBILITY_LABEL,
      Some(text(&format!("{label}, {value}"))),
   );
   if let Some(row) = root.kids.first_mut() {
      if let Some(label_node) = row.kids.get_mut(0) {
         label_node.set(Prop::TEXT, Some(text(label)));
      }
      if let Some(value_node) = row
         .kids
         .get_mut(1)
         .and_then(|wrap| wrap.kids.get_mut(0))
         .and_then(|inner| inner.kids.get_mut(0))
      {
         value_node.set(Prop::TEXT, Some(text(value)));
      }
      row.kids.truncate(2);
   }
   root
}

/// `root > row > [icon, column > [title, subtitle], wrap > track > knob]`.
/// Pressing the row runs `press`, pressing the switch runs `toggle`.
pub fn switch_row(
   icon: Option<&str>,
   title: &str,
   subtitle: Option<&str>,
   on: bool,
   press: Press,
   toggle: Press,
) -> Tree {
   let layouts = templates();
   let mut root = Tree::from_template(&layouts.switch_row);
   root.props.retain(|(key, _)| !is_radius(*key));
   let label = subtitle.map_or_else(
      || title.to_owned(),
      |subtitle| format!("{title}, {subtitle}"),
   );
   root.set(Prop::ACCESSIBILITY_LABEL, Some(text(&label)));
   root.set(Prop::ACCESSIBILITY_CHECKED, Some(Value::Bool(on)));
   root.set(
      Prop::ACCESSIBILITY_ID,
      Some(text(&format!("phoenix-{title}"))),
   );
   root.press = Some(press);
   let Some(row) = root.kids.first_mut() else {
      return root;
   };
   if let Some(icon_node) = row.kids.get_mut(0) {
      icon_node.set(Prop::URI, icon.map(text));
   }
   if let Some(column) = row.kids.get_mut(1) {
      if let Some(title_node) = column.kids.get_mut(0) {
         title_node.set(Prop::TEXT, Some(text(title)));
      }
      match subtitle {
         Some(subtitle) => {
            if let Some(subtitle_node) = column.kids.get_mut(1) {
               subtitle_node.set(Prop::TEXT, Some(text(subtitle)));
            }
         },
         None => column.kids.truncate(1),
      }
   }
   if let Some(track) = row.kids.get_mut(2).and_then(|wrap| wrap.kids.get_mut(0)) {
      track.press = Some(toggle);
      animate(track, &layouts.track_worklet, &layouts.track_off, on);
      if let Some(knob) = track.kids.get_mut(0) {
         animate(knob, &layouts.knob_worklet, &layouts.knob_off, on);
      }
   }
   if icon.is_none() {
      row.kids.remove(0);
   }
   root
}

/// Gives a switch part built from the "on" template the props for `on` and a
/// worklet to spring between both states.
fn animate(node: &mut Tree, worklet: &'static Worklet, off: &Props, on: bool) {
   let other = if on {
      off.clone()
   } else {
      std::mem::replace(&mut node.props, off.clone())
   };
   node.animate = Some(Animate {
      worklet,
      other,
      flipped: false,
   });
}

/// What a text field takes, which picks its keyboard.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
   Text,
   Number,
   Secret,
}

/// `root > frame > row > [icon wrap > icon, input]`, Phoenix's search box.
pub fn text_field(
   field: Field,
   icon: Option<&str>,
   placeholder: &str,
   value: &str,
   entry: InputKind,
) -> Tree {
   let mut root = Tree::from_template(&templates().input);
   let Some(row) = root
      .kids
      .first_mut()
      .and_then(|frame| frame.kids.first_mut())
   else {
      return root;
   };
   if let Some(icon_node) = row.kids.first_mut().and_then(|wrap| wrap.kids.first_mut()) {
      icon_node.set(Prop::URI, icon.map(text));
   }
   if let Some(input) = row.kids.get_mut(1) {
      input.set(Prop::PLACEHOLDER, Some(text(placeholder)));
      input.set(Prop::VALUE, Some(text(value)));
      input.set(
         Prop::KEYBOARD,
         (entry == InputKind::Number).then(|| text("number")),
      );
      input.set(
         Prop::SECURE,
         (entry == InputKind::Secret).then_some(Value::Bool(true)),
      );
      input.set(Prop::RETURN_KEY, Some(text("go")));
      input.set(Prop::ROLE, None);
      input.set(
         Prop::ACCESSIBILITY_ID,
         Some(text(&format!("phoenix-{placeholder}"))),
      );
      input.slot = Some(Slot::Input(field));
   }
   if icon.is_none() {
      row.kids.remove(0);
      if let Some(input) = row.kids.first_mut() {
         input.set(Prop::MARGIN, None);
      }
   }
   root
}
