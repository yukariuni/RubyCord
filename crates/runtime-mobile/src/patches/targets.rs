use phoenix_ui_mobile::{
   Node,
   Prop,
   Props,
   ViewKind,
   get_prop,
   mirror::{
      Created,
      Mirror,
   },
   prop,
   templates,
};
use phoenix_wire::Value;

use crate::patches::{
   Contribution,
   Draft,
   Handler,
   Outcome,
   Place,
   Rendered,
   aids::{
      self,
      MessagePart,
      message_part,
   },
   menu::SheetBody,
   tabs,
};

const DANGER: i64 = 4_294_475_386;
const MUTED: i64 = 4_288_059_294;
const DEFAULT_ICON: &str = "res://ic_mana_puzzle_piece_plus";

/// Where a contribution goes, next to one of Phoenix's own nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
   /// The chat header, left of search.
   Header,
   /// The message box, left of the emoji button.
   Composer,
   /// Under a message's content.
   Content,
   /// After a message's timestamp.
   Timestamp,
   /// The message long-press menu, above Delete.
   Action,
   /// The end of a profile's main tab.
   Profile,
   /// A tab after a profile's own tabs. Plugins add these; the page that goes
   /// with each is drawn at [`Target::TabPage`].
   Tab,
   /// The pages of profile tabs, after the profile's own pages.
   TabPage,
   /// After a user's name in the member list and the DM list.
   Member,
   /// After the author's name in a message header.
   Author,
}

impl Target {
   pub const ALL: [Self; 10] = [
      Self::Header,
      Self::Composer,
      Self::Content,
      Self::Timestamp,
      Self::Action,
      Self::Profile,
      Self::Tab,
      Self::TabPage,
      Self::Member,
      Self::Author,
   ];

   /// The target whose contributions this one draws.
   pub const fn drawn_by(self) -> Self {
      match self {
         Self::TabPage => Self::Tab,
         other => other,
      }
   }

   pub fn matches(self, aid: &str) -> bool {
      match self {
         Self::Header => aid == aids::SEARCH_ENTRY,
         Self::Composer => aid == aids::COMPOSER_EXPRESSIONS,
         Self::Content => matches!(message_part(aid), Some((MessagePart::Content, _))),
         Self::Timestamp => matches!(message_part(aid), Some((MessagePart::Timestamp, _))),
         Self::Action => aid == aids::MESSAGE_ACTIONS,
         Self::Profile | Self::Tab | Self::TabPage => aid == aids::PROFILE_TAB_PAGES,
         Self::Member => {
            aids::snowflake(aid, aids::MEMBER_ROW_PREFIX).is_some()
               || aids::snowflake(aid, aids::DM_ROW_PREFIX).is_some()
         },
         Self::Author => matches!(message_part(aid), Some((MessagePart::Author, _))),
      }
   }

   /// Where this target's nodes go for the anchor `id`.
   pub fn place(self, mirror: &Mirror, id: i32, aid: &str, seen: &Seen) -> Option<Placement> {
      match self {
         Self::Header | Self::Composer => {
            Some(Placement {
               parent: mirror.parent(id)?,
               before: Some(id),
               place:  Place::Screen,
            })
         },
         Self::Content => {
            Some(Placement {
               parent: id,
               before: None,
               place:  Place::Message(message_part(aid)?.1.to_owned()),
            })
         },
         Self::Timestamp => {
            Some(Placement {
               parent: mirror.parent(id)?,
               before: None,
               place:  Place::Message(message_part(aid)?.1.to_owned()),
            })
         },
         Self::Action => {
            let groups = *mirror.kids(id).last()?;
            let delete = mirror.kids(groups).iter().copied().find(|group| {
               mirror
                  .find_down(*group, &|aid| aid == aids::MESSAGE_DELETE, 2)
                  .is_some()
            });
            Some(Placement {
               parent: groups,
               before: delete,
               place:  seen
                  .long_pressed
                  .clone()
                  .map_or(Place::Screen, Place::Message),
            })
         },
         Self::Profile => {
            // Phoenix nests the sections six levels under the pages row.
            let mut node = id;
            for _ in 0..6 {
               node = *mirror.kids(node).first()?;
            }
            Some(Placement {
               parent: node,
               before: None,
               place:  Place::User(seen.profile_user.clone()),
            })
         },
         Self::Tab => {
            let (row, indicator) = tab_row(mirror, id)?;
            Some(Placement {
               parent: row,
               before: Some(indicator),
               place:  Place::User(seen.profile_user.clone()),
            })
         },
         Self::TabPage => {
            Some(Placement {
               parent: *mirror.kids(id).first()?,
               before: None,
               place:  Place::User(seen.profile_user.clone()),
            })
         },
         Self::Member => {
            let place = match aids::snowflake(aid, aids::MEMBER_ROW_PREFIX) {
               Some(user) => Place::User(Some(user.to_owned())),
               None => Place::Channel(aids::snowflake(aid, aids::DM_ROW_PREFIX)?.to_owned()),
            };
            after(mirror, member_name(mirror, id)?.0, place)
         },
         Self::Author => after(mirror, id, Place::Message(message_part(aid)?.1.to_owned())),
      }
   }

   /// Whether drawing this target needs a plugin's `render` or `visible`.
   pub const fn evaluates(self) -> bool {
      matches!(
         self,
         Self::Content
            | Self::Timestamp
            | Self::Profile
            | Self::TabPage
            | Self::Member
            | Self::Author
      )
   }

   /// The name in DM row `id` when it stretches across its row, as it does
   /// without tags, which would leave decorations at the row's far end.
   pub fn stretched_name(mirror: &Mirror, id: i32, aid: &str) -> Option<i32> {
      aids::snowflake(aid, aids::DM_ROW_PREFIX)?;
      match member_name(mirror, id)? {
         (name, 2) => Some(name),
         _ => None,
      }
   }

   /// Whether this target's places are about a user, which its plugins get
   /// as `userId`.
   pub const fn decorates(self) -> bool {
      matches!(self, Self::Member | Self::Author)
   }
}

/// The name in member or DM row `id`, the first leaf of its name column,
/// and how deep it is there.
///
/// `row > [avatar, column > [names > [name, tags…], …]]`, where a DM with
/// tags has its names one row further down, and one without has the name
/// right in the row with the time.
fn member_name(mirror: &Mirror, id: i32) -> Option<(i32, usize)> {
   let mut name = *mirror.kids(id).get(1)?;
   for depth in 0..4 {
      let Some(first) = mirror.kids(name).first() else {
         return Some((name, depth));
      };
      name = *first;
   }
   None
}

/// Right after `node`, among its siblings.
fn after(mirror: &Mirror, node: i32, place: Place) -> Option<Placement> {
   let parent = mirror.parent(node)?;
   let siblings = mirror.kids(parent);
   let at = siblings.iter().position(|sibling| *sibling == node)?;
   Some(Placement {
      parent,
      before: siblings.get(at + 1).copied(),
      place,
   })
}

/// The row holding the tabs of the profile whose pages are `pager`, and its
/// last child, the sliding indicator.
///
/// `column > [tabs > stack > column > [strip > row > [tab…, indicator],
/// divider], frame > pager]`
pub fn tab_row(mirror: &Mirror, pager: i32) -> Option<(i32, i32)> {
   let frame = mirror.parent(pager)?;
   let siblings = mirror.kids(mirror.parent(frame)?);
   let at = siblings.iter().position(|sibling| *sibling == frame)?;
   let mut node = *siblings.get(at.checked_sub(1)?)?;
   for _ in 0..4 {
      node = *mirror.kids(node).first()?;
   }
   let kids = mirror.kids(node);
   if kids.len() < 2 {
      return None;
   }
   Some((node, *kids.last()?))
}

/// What the dispatches and batches showed about the screen.
#[derive(Default)]
pub struct Seen {
   pub long_pressed: Option<String>,
   pub profile_user: Option<String>,
}

pub struct Placement {
   pub parent: i32,
   /// The sibling the nodes go before, or the end.
   pub before: Option<i32>,
   pub place:  Place,
}

/// Phoenix's own nodes, snapshotted when they appear, so plugin nodes look
/// like their neighbours.
#[derive(Default)]
pub struct Templates {
   header:     Option<Node>,
   composer:   Option<Node>,
   timestamp:  Option<Node>,
   profile:    Option<Node>,
   /// A profile tab that is not selected, and the page width.
   tab:        Option<Node>,
   page_width: Option<Value>,
   /// The first text color on Phoenix's first page.
   page_text:  Option<i64>,
   /// The selected tab's text color.
   tab_color:  Option<i64>,
   /// The icons of the message menu's rows, by their accessibility id.
   icons:      std::collections::HashMap<String, String>,
}

/// A row of a menu sheet.
pub(super) struct MenuRow {
   pub(super) label:  String,
   pub(super) icon:   Option<String>,
   pub(super) danger: bool,
   pub(super) press:  Handler,
   pub(super) id:     Option<String>,
}

impl Templates {
   pub fn learn(&mut self, mirror: &Mirror, id: i32, aid: &str, created: &Created<'_>) {
      match aid {
         aids::SEARCH_ENTRY => self.header = mirror.snapshot(id, created),
         aids::COMPOSER_EXPRESSIONS => self.composer = mirror.snapshot(id, created),
         aids::PROFILE_TAB_PAGES => {
            self.learn_profile(mirror, id, created);
            self.learn_tabs(mirror, id, created);
         },
         _ if matches!(message_part(aid), Some((MessagePart::Timestamp, _)))
            && self.timestamp.is_none() =>
         {
            self.timestamp = mirror.snapshot(id, created);
         },
         _ if aid.starts_with(aids::MESSAGE_ROW_PREFIX) => {
            if let Some(icon) = mirror.snapshot(id, created).as_ref().and_then(first_image) {
               self.icons.insert(aid.to_owned(), icon);
            }
         },
         _ => {},
      }
   }

   /// A menu sheet like the message long-press one: `title` over optional
   /// `body` and `rows`.
   pub(super) fn menu(
      &self,
      title: Option<&str>,
      body: Option<&SheetBody>,
      rows: Vec<MenuRow>,
      dismiss: &Handler,
   ) -> Draft {
      let mut kids = Vec::new();
      if let Some(title) = title
         && let Some(sample) = templates()
            .button_row
            .kids
            .first()
            .and_then(|inner| inner.kids.get(1))
      {
         let mut head = text(&sample.props, &Rendered {
            text:  title.to_owned(),
            color: None,
            tint:  None,
            icons: Vec::new(),
         });
         head.set(Prop::MARGIN, Some(Value::Rect([4.0, 0.0, 4.0, 8.0])));
         head.set(prop!("fontWeight"), Some(Value::I32(700)));
         kids.push(head);
      }
      if let Some(body) = body {
         let mut content = Draft {
            kind:  ViewKind::Text,
            props: vec![
               (Prop::TEXT, Value::Str(body.text.clone())),
               (Prop::COLOR, Value::I64(0xFFF6_F6F7)),
               (Prop::FONT_SIZE, Value::F64(12.0)),
               (Prop::LINE_HEIGHT, Value::F64(16.0)),
               (prop!("fontWeight"), Value::I32(400)),
               (Prop::SELECTABLE, Value::Bool(true)),
            ],
            kids:  Vec::new(),
            press: None,
            src:   None,
         };
         if body.monospace {
            content.set(Prop::FONT_FAMILY, Some(Value::Str("monospace".to_owned())));
         }
         if body.highlight && body.text.len() <= 32 * 1024 {
            let spans = crate::highlight::js(&body.text)
               .into_iter()
               .map(|(kind, run)| {
                  let color = match kind {
                     crate::highlight::Kind::Plain => 0xFFF6_F6F7,
                     crate::highlight::Kind::Comment => 0xFF8B_949E,
                     crate::highlight::Kind::String => 0xFF7E_E787,
                     crate::highlight::Kind::Keyword => 0xFFFF_7B72,
                     crate::highlight::Kind::Number => 0xFF79_C0FF,
                  };
                  Value::Obj(vec![
                     (Prop::TEXT.key(), Value::Str(run.to_owned())),
                     (Prop::COLOR.key(), Value::I64(color)),
                  ])
               })
               .collect();
            content.set(prop!("spans"), Some(Value::Arr(spans)));
         }
         let scroll = Draft {
            kind:  ViewKind::Scroll,
            props: vec![
               (prop!("maxHeight"), Value::F64(420.0)),
               (prop!("panYieldsToScroll"), Value::Bool(true)),
            ],
            kids:  vec![content],
            press: None,
            src:   None,
         };
         let box_view = Draft {
            kind:  ViewKind::View,
            props: vec![
               (prop!("background"), Value::I64(0xFF14_1518)),
               (Prop::RADIUS, Value::F64(12.0)),
               (Prop::PADDING, Value::Rect([12.0, 10.0, 12.0, 10.0])),
               (Prop::MARGIN, Value::Rect([0.0, 0.0, 0.0, 12.0])),
            ],
            kids:  vec![scroll],
            press: None,
            src:   None,
         };
         kids.push(box_view);
      }
      kids.extend(self.group(rows));
      let mut sheet = Draft::from_node(&templates().sheet);
      sheet.press = Some(dismiss.clone());
      let mut groups = &mut sheet;
      while !groups.kids.is_empty() {
         groups = &mut groups.kids[0];
      }
      groups.kids = kids;
      sheet
   }

   fn learn_tabs(&mut self, mirror: &Mirror, pager: i32, created: &Created<'_>) {
      let page = mirror
         .kids(pager)
         .first()
         .and_then(|row| mirror.kids(*row).first());
      if let Some(width) = page
         .and_then(|page| created.get(page))
         .and_then(|(_, props)| get_prop(props, prop!("width")))
      {
         self.page_width = Some(width.clone());
      }
      if let Some(page) = page.and_then(|page| mirror.snapshot(*page, created)) {
         self.page_text = first_text_color(&page).or(self.page_text);
      }
      let Some((row, _)) = tab_row(mirror, pager) else {
         return;
      };
      let tabs = mirror.kids(row);
      let color = |tab: &Node| {
         match tab
            .kids
            .first()?
            .kids
            .first()
            .and_then(|text| get_prop(&text.props, Prop::COLOR))
         {
            Some(Value::I64(color)) => Some(*color),
            Some(Value::I32(color)) => Some(i64::from(*color)),
            _ => None,
         }
      };
      for tab in &tabs[..tabs.len() - 1] {
         let Some(node) = mirror.snapshot(*tab, created) else {
            continue;
         };
         let selected = get_prop(&node.props, prop!("accessibilitySelected"));
         if matches!(selected, Some(Value::Bool(true))) {
            self.tab_color = color(&node).or(self.tab_color);
         } else if node
            .kids
            .first()
            .is_some_and(|inner| !inner.kids.is_empty())
         {
            self.tab = Some(node);
         }
      }
   }

   fn learn_profile(&mut self, mirror: &Mirror, id: i32, created: &Created<'_>) {
      let seen = Seen::default();
      let Some(placement) = Target::Profile.place(mirror, id, "", &seen) else {
         return;
      };
      let first = mirror
         .kids(placement.parent)
         .first()
         .and_then(|section| mirror.kids(*section).first());
      let Some(section) = first.and_then(|first| mirror.snapshot(*first, created)) else {
         return;
      };
      let shaped = section
         .kids
         .first()
         .is_some_and(|head| head.kind == ViewKind::Text)
         && section
            .kids
            .get(1)
            .and_then(|body| body.kids.first())
            .is_some_and(|text| text.kind == ViewKind::Text);
      if shaped {
         self.profile = Some(section);
      }
   }

   /// The nodes `contributions` draw at `place`. `outcome` answers what
   /// plugins already evaluated there, `None` while it is pending.
   pub fn draw(
      &self,
      target: Target,
      contributions: &[&Contribution],
      place: &Place,
      outcome: &impl Fn(&Contribution) -> Option<Outcome>,
   ) -> Vec<Draft> {
      match target {
         Target::Header => Self::icon_buttons(self.header.as_ref(), contributions, place),
         Target::Composer => Self::icon_buttons(self.composer.as_ref(), contributions, place),
         Target::Content | Target::Timestamp => {
            contributions
               .iter()
               .filter_map(|contribution| {
                  let Some(Outcome::Text(Some(rendered))) = outcome(contribution) else {
                     return None;
                  };
                  // A tint alone draws no line.
                  if rendered.text.is_empty() {
                     return None;
                  }
                  let mut node = text(&self.accessory_props(), &rendered);
                  if target == Target::Content {
                     node.set(Prop::MARGIN, Some(Value::Rect([0.0, 2.0, 0.0, 0.0])));
                  }
                  Some(node)
               })
               .collect()
         },
         Target::Action => self.action_group(contributions, place, outcome),
         Target::Member | Target::Author => {
            contributions
               .iter()
               .filter_map(|contribution| {
                  let Some(Outcome::Text(Some(rendered))) = outcome(contribution) else {
                     return None;
                  };
                  decoration(&rendered, target == Target::Member)
               })
               .collect()
         },
         Target::Tab => {
            let Some(tab) = &self.tab else {
               return Vec::new();
            };
            contributions
               .iter()
               .map(|contribution| tabs::tab(tab, contribution))
               .collect()
         },
         Target::TabPage => {
            contributions
               .iter()
               .map(|contribution| {
                  tabs::page(
                     self.page_width.clone(),
                     self.palette(),
                     contribution,
                     outcome(contribution),
                  )
               })
               .collect()
         },
         Target::Profile => {
            let Some(section) = &self.profile else {
               return Vec::new();
            };
            contributions
               .iter()
               .filter_map(|contribution| {
                  let Some(Outcome::Text(Some(rendered))) = outcome(contribution) else {
                     return None;
                  };
                  let mut node = Draft::from_node(section);
                  let mut head = text(&section.kids[0].props, &Rendered {
                     text:  contribution.label.clone(),
                     color: None,
                     tint:  None,
                     icons: Vec::new(),
                  });
                  head.set(
                     Prop::ACCESSIBILITY_LABEL,
                     Some(Value::Str(contribution.label.clone())),
                  );
                  let body = text(&section.kids[1].kids[0].props, &rendered);
                  node.kids.truncate(2);
                  node.kids[0] = head;
                  node.kids[1].kids = vec![body];
                  Some(node)
               })
               .collect()
         },
      }
   }

   fn icon_buttons(
      template: Option<&Node>,
      contributions: &[&Contribution],
      place: &Place,
   ) -> Vec<Draft> {
      let Some(template) = template else {
         return Vec::new();
      };
      contributions
         .iter()
         .map(|contribution| {
            let mut button = Draft::from_node(template);
            button.set(
               Prop::ACCESSIBILITY_ID,
               Some(Value::Str(format!(
                  "phoenix.{}.{}",
                  contribution.plugin, contribution.id.0
               ))),
            );
            button.set(
               Prop::ACCESSIBILITY_LABEL,
               Some(Value::Str(contribution.label.clone())),
            );
            if let Some(image) = button.kids.first_mut() {
               if let Some(icon) = &contribution.icon {
                  image.set(Prop::URI, Some(Value::Str(icon.clone())));
               }
               if let Some(tint) = contribution.tint {
                  image.set(Prop::TINT, Some(Value::I64(tint)));
               }
            }
            button.press = Some(Handler::Press {
               contribution: contribution.id,
               place:        place.clone(),
               dismiss:      false,
            });
            button
         })
         .collect()
   }

   fn action_group(
      &self,
      contributions: &[&Contribution],
      place: &Place,
      outcome: &impl Fn(&Contribution) -> Option<Outcome>,
   ) -> Vec<Draft> {
      let rows = contributions
         .iter()
         .filter(|contribution| {
            !contribution.conditional || matches!(outcome(contribution), Some(Outcome::Shown(true)))
         })
         .map(|contribution| {
            MenuRow {
               label:  contribution.label.clone(),
               icon:   contribution.icon.clone(),
               danger: contribution.danger,
               press:  Handler::Press {
                  contribution: contribution.id,
                  place:        place.clone(),
                  dismiss:      true,
               },
               id:     Some(format!(
                  "phoenix.{}.{}",
                  contribution.plugin, contribution.id.0
               )),
            }
         })
         .collect();
      self.group(rows)
   }

   /// `rows` in one rounded group, styled like the message menu's rows.
   fn group(&self, rows: Vec<MenuRow>) -> Vec<Draft> {
      let layouts = templates();
      let count = rows.len();
      let mut kids = Vec::new();
      for (index, entry) in rows.into_iter().enumerate() {
         let mut draft = Draft::from_node(&layouts.button_row);
         for corner in [
            Prop::RADIUS,
            Prop::RADIUS_TOP_LEFT,
            Prop::RADIUS_TOP_RIGHT,
            Prop::RADIUS_BOTTOM_LEFT,
            Prop::RADIUS_BOTTOM_RIGHT,
         ] {
            draft.set(corner, None);
         }
         let round = Some(Value::F64(12.0));
         if count == 1 {
            draft.set(Prop::RADIUS, round);
         } else if index == 0 {
            draft.set(Prop::RADIUS_TOP_LEFT, round.clone());
            draft.set(Prop::RADIUS_TOP_RIGHT, round);
         } else if index == count - 1 {
            draft.set(Prop::RADIUS_BOTTOM_LEFT, round.clone());
            draft.set(Prop::RADIUS_BOTTOM_RIGHT, round);
         }
         draft.set(Prop::ACCESSIBILITY_ID, entry.id.map(Value::Str));
         draft.set(
            Prop::ACCESSIBILITY_LABEL,
            Some(Value::Str(entry.label.clone())),
         );
         let inner = &mut draft.kids[0];
         // The settings row ends in a chevron that menu rows lack.
         inner.kids.truncate(2);
         // An icon without a scheme names a message menu row to borrow it from.
         let icon = match entry.icon.as_deref() {
            Some(icon) if !icon.contains("://") => {
               self.icons.get(icon).map_or(DEFAULT_ICON, String::as_str)
            },
            Some(icon) => icon,
            None => DEFAULT_ICON,
         };
         inner.kids[0].set(Prop::URI, Some(Value::Str(icon.to_owned())));
         inner.kids[1].set(Prop::TEXT, Some(Value::Str(entry.label.clone())));
         if entry.danger {
            inner.kids[1].set(Prop::COLOR, Some(Value::I64(DANGER)));
            inner.kids[0].set(Prop::TINT, Some(Value::I64(DANGER)));
         }
         draft.press = Some(entry.press);
         if index > 0 {
            kids.push(Draft::from_node(&layouts.divider));
         }
         kids.push(draft);
      }
      if kids.is_empty() {
         return Vec::new();
      }
      let mut group = Draft::from_node(&layouts.group);
      group.kids = kids;
      vec![group]
   }

   /// Resolves a menu icon name: a URI with `://` passes through; an
   /// accessibility id borrows the icon learned from the message action menu.
   pub(super) fn resolve_icon<'a>(&'a self, icon: Option<&'a str>) -> &'a str {
      match icon {
         Some(icon) if !icon.contains("://") => {
            self.icons.get(icon).map_or(DEFAULT_ICON, String::as_str)
         },
         Some(icon) => icon,
         None => DEFAULT_ICON,
      }
   }

   /// The colors of Phoenix's first profile page.
   pub fn palette(&self) -> tabs::Palette {
      tabs::Palette {
         text:  self.page_text.unwrap_or(tabs::TEXT),
         muted: self.tab_colors().1,
      }
   }

   /// A plugin tab's text color when selected, and when not.
   pub fn tab_colors(&self) -> (i64, i64) {
      let unselected = self
         .tab
         .as_ref()
         .and_then(|tab| tab.kids.first()?.kids.first())
         .and_then(|text| {
            match get_prop(&text.props, Prop::COLOR) {
               Some(Value::I64(color)) => Some(*color),
               Some(Value::I32(color)) => Some(i64::from(*color)),
               _ => None,
            }
         })
         .unwrap_or(tabs::MUTED);
      (self.tab_color.unwrap_or(tabs::ACCENT), unselected)
   }

   /// The timestamp's text style without its placement, or a fallback.
   fn accessory_props(&self) -> Props {
      const DROPPED: [Prop; 7] = [
         Prop::ACCESSIBILITY_ID,
         Prop::ACCESSIBILITY_LABEL,
         prop!("accessibilityHidden"),
         prop!("maxLines"),
         Prop::MARGIN,
         prop!("gravity"),
         Prop::ON,
      ];
      self.timestamp.as_ref().map_or_else(
         || {
            vec![
               (Prop::COLOR, Value::I64(MUTED)),
               (Prop::FONT_SIZE, Value::F64(12.0)),
               (prop!("fontWeight"), Value::I32(500)),
            ]
         },
         |timestamp| {
            timestamp
               .props
               .iter()
               .filter(|(key, _)| !DROPPED.contains(key))
               .cloned()
               .collect()
         },
      )
   }
}

fn first_image(node: &Node) -> Option<String> {
   if node.kind == ViewKind::Image
      && let Some(Value::Str(uri)) = get_prop(&node.props, Prop::URI)
   {
      return Some(uri.clone());
   }
   node.kids.iter().find_map(first_image)
}

fn first_text_color(node: &Node) -> Option<i64> {
   if node.kind == ViewKind::Text {
      match get_prop(&node.props, Prop::COLOR) {
         Some(Value::I64(color)) => return Some(*color),
         Some(Value::I32(color)) => return Some(i64::from(*color)),
         _ => {},
      }
   }
   node.kids.iter().find_map(first_text_color)
}

/// A decoration's icons in a row, or with `text` and no icons, its text.
fn decoration(rendered: &Rendered, text: bool) -> Option<Draft> {
   const SIZE: f64 = 16.0;
   let draft = |kind, props| {
      Draft {
         kind,
         props,
         kids: Vec::new(),
         press: None,
         src: None,
      }
   };
   if rendered.icons.is_empty() {
      if !text || rendered.text.is_empty() {
         return None;
      }
      let mut props = vec![
         (Prop::TEXT, Value::Str(rendered.text.clone())),
         (Prop::FONT_SIZE, Value::F64(12.0)),
         (prop!("maxLines"), Value::I32(1)),
      ];
      if let Some(color) = rendered.color {
         props.push((Prop::COLOR, Value::I64(color)));
      }
      return Some(draft(ViewKind::Text, props));
   }
   let mut row = draft(ViewKind::View, vec![
      (prop!("direction"), Value::Str("row".to_owned())),
      (prop!("gap"), Value::F64(2.0)),
      (prop!("align"), Value::Str("center".to_owned())),
   ]);
   row.kids = rendered
      .icons
      .iter()
      .map(|uri| {
         draft(ViewKind::Image, vec![
            (Prop::URI, Value::Str(uri.clone())),
            (prop!("width"), Value::F64(SIZE)),
            (Prop::HEIGHT, Value::F64(SIZE)),
            (prop!("scale"), Value::Str("fit".to_owned())),
         ])
      })
      .collect();
   Some(row)
}

pub fn text(props: &Props, rendered: &Rendered) -> Draft {
   let mut node = Draft {
      kind:  ViewKind::Text,
      props: props.clone(),
      kids:  Vec::new(),
      press: None,
      src:   None,
   };
   node.set(prop!("spans"), None);
   node.set(Prop::TEXT, Some(Value::Str(rendered.text.clone())));
   if let Some(color) = rendered.color {
      node.set(Prop::COLOR, Some(Value::I64(color)));
   }
   node
}
