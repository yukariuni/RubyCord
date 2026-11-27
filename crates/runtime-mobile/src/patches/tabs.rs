//! Plugin tabs on profiles: the tab next to Phoenix's own, and the page of
//! blocks it shows.
//!
//! - The tabs sit in a row with Phoenix's, before its sliding indicator, and
//!   the pages go after Phoenix's in the paging `hscroll`
//!   (`profile-tab-pages`).
//! - Phoenix's tab presses set the pager's `page`; a plugin tab sets it too,
//!   past Phoenix's pages. The pager's `pageSelect`/`pageSettle`/`pageScroll`
//!   events about those pages never reach Rust, which knows only its own.
//! - The indicator follows the pager on the host side and has no stop for a
//!   plugin tab, so it hides while one is open and the tab draws its own bar.
//! - Rust sizes the pager to its open page. A plugin page reports its height
//!   with `layout`, and Rust's height comes back when one of its tabs opens.

use phoenix_ui_mobile::{
   Node,
   Prop,
   Props,
   ViewKind,
   prop,
   templates,
};
use phoenix_wire::Value;

use crate::patches::{
   Block,
   BlockAction,
   Contribution,
   ContributionId,
   Draft,
   Handler,
   Outcome,
};

const DANGER: i64 = 4_294_475_386;
pub const ACCENT: i64 = 4_286_156_281;
pub const MUTED: i64 = 4_289_440_946;
pub const TEXT: i64 = 4_294_704_123;

/// Colors from the profile's own tabs, so pages follow its theme.
#[derive(Clone, Copy)]
pub struct Palette {
   /// The open tab's label.
   pub text:  i64,
   /// The other tabs' labels.
   pub muted: i64,
}

impl Palette {
   /// `muted` at `alpha`, for fills that work on light and dark themes.
   const fn wash(self, alpha: i64) -> Value {
      Value::I64((self.muted & 0x00FF_FFFF) | (alpha << 24))
   }
}

/// Props Phoenix animates a tab with, which a plugin tab must not share.
const ANIMATED: [Prop; 3] = [
   Prop::named("viewMotion"),
   Prop::named("worklet"),
   Prop::named("portalSlot"),
];

fn str(text: &str) -> Value {
   Value::Str(text.to_owned())
}

const fn node(kind: ViewKind, props: Props) -> Draft {
   Draft {
      kind,
      props,
      kids: Vec::new(),
      press: None,
      src: None,
   }
}

fn view(props: Props, kids: Vec<Draft>) -> Draft {
   let mut view = node(ViewKind::View, props);
   view.kids = kids;
   view
}

fn label(text: &str, size: f64, weight: i32, color: i64) -> Draft {
   node(ViewKind::Text, vec![
      (prop!("text"), str(text)),
      (prop!("color"), Value::I64(color)),
      (prop!("fontSize"), Value::F64(size)),
      (prop!("fontWeight"), Value::I32(weight)),
      (prop!("lineHeight"), Value::F64(size + 4.0)),
      (prop!("lineHeightExact"), Value::Bool(true)),
   ])
}

fn pressable(mut draft: Draft, press: Handler) -> Draft {
   draft.set(Prop::ROLE, Some(str("button")));
   draft.set(Prop::ON, Some(Value::Arr(vec![str("press")])));
   draft.press = Some(press);
   draft
}

/// A plugin tab: Phoenix's unselected `tab` without its animation, holding
/// its label and a bar under it, `[inner > text, bar]`.
pub fn tab(template: &Node, contribution: &Contribution) -> Draft {
   let mut tab = Draft::from_node(template);
   for key in ANIMATED {
      tab.set(key, None);
   }
   tab.set(prop!("accessibilitySelected"), Some(Value::Bool(false)));
   tab.set(Prop::ACCESSIBILITY_LABEL, Some(str(&contribution.label)));
   tab.set(
      Prop::ACCESSIBILITY_ID,
      Some(str(&format!(
         "phoenix.{}.{}",
         contribution.plugin, contribution.id
      ))),
   );
   tab.set(prop!("direction"), Some(str("column")));
   tab.set(prop!("align"), Some(str("center")));
   tab.set(Prop::PADDING, None);
   let mut tab = pressable(tab, Handler::Tab {
      contribution: contribution.id,
   });
   let mut inner = tab.kids.swap_remove(0);
   inner.kids.truncate(1);
   if let Some(text) = inner.kids.first_mut() {
      for key in ANIMATED {
         text.set(key, None);
      }
      text.set(prop!("spans"), None);
      text.set(Prop::TEXT, Some(str(&contribution.label)));
   }
   let bar = view(
      vec![
         (prop!("width"), str("fill")),
         (prop!("height"), Value::F64(2.0)),
         (prop!("margin"), Value::Rect([0.0, 12.0, 0.0, 0.0])),
         (prop!("radiusTopLeft"), Value::F64(4.0)),
         (prop!("radiusTopRight"), Value::F64(4.0)),
         (prop!("background"), Value::I64(0)),
      ],
      Vec::new(),
   );
   tab.kids = vec![inner, bar];
   tab
}

/// The page of `contribution`'s tab: `page > scroll > content > blocks`.
/// `content` reports its height.
pub fn page(
   width: Option<Value>,
   colors: Palette,
   contribution: &Contribution,
   outcome: Option<Outcome>,
) -> Draft {
   let blocks = match outcome {
      Some(Outcome::Page(blocks)) => blocks,
      None => {
         vec![Block::Text {
            title: None,
            text:  "Loading…".to_owned(),
            color: Some(colors.muted),
         }]
      },
      Some(_) => Vec::new(),
   };
   let id = contribution.id;
   let mut kids = Vec::new();
   let mut after_entry = false;
   for block in blocks {
      let entry = matches!(block, Block::Entry { .. });
      if entry && after_entry {
         kids.push(view(
            vec![
               (prop!("width"), str("fill")),
               (prop!("height"), Value::F64(1.0)),
               (prop!("background"), colors.wash(0x26)),
            ],
            Vec::new(),
         ));
      }
      after_entry = entry;
      kids.push(draw(block, id, colors));
   }
   let mut content = view(
      vec![
         (prop!("direction"), str("column")),
         (prop!("width"), str("fill")),
         (prop!("padding"), Value::Rect([16.0, 16.0, 16.0, 16.0])),
         (prop!("gap"), Value::F64(12.0)),
         (prop!("align"), str("stretch")),
         (prop!("on"), Value::Arr(vec![str("layout")])),
      ],
      kids,
   );
   content.press = Some(Handler::Page { contribution: id });
   // Like Phoenix's pages: a locked scroll lets the content take its full
   // height, which it reports, instead of being squeezed to the pager's.
   let mut scroll = node(ViewKind::Scroll, vec![
      (prop!("maxHeight"), str("100%")),
      (prop!("scrollIndicator"), Value::Bool(false)),
      (prop!("bounces"), Value::Bool(false)),
      (prop!("scrollLocked"), Value::Bool(true)),
      (prop!("keyboardShouldPersistTaps"), str("handled")),
   ]);
   scroll.kids = vec![content];
   view(
      vec![
         (prop!("direction"), str("column")),
         (prop!("width"), width.unwrap_or_else(|| str("fill"))),
         (prop!("height"), str("fill")),
         (prop!("align"), str("stretch")),
      ],
      vec![scroll],
   )
}

const fn block_handler(contribution: ContributionId, index: u32) -> Handler {
   Handler::Block {
      contribution,
      index,
   }
}

fn draw(block: Block, contribution: ContributionId, colors: Palette) -> Draft {
   let (text_color, muted) = (colors.text, colors.muted);
   match block {
      Block::Text { title, text, color } => {
         let mut kids = Vec::new();
         if let Some(title) = title {
            kids.push(label(&title, 12.0, 700, muted));
         }
         kids.push(label(&text, 14.0, 400, color.unwrap_or(text_color)));
         view(
            vec![
               (prop!("direction"), str("column")),
               (prop!("gap"), Value::F64(4.0)),
               (prop!("align"), str("stretch")),
            ],
            kids,
         )
      },
      Block::Entry {
         avatar,
         title,
         subtitle,
         text,
         badges,
         images,
         actions,
         side,
         hold,
      } => {
         let head = entry_head(&title, badges, subtitle, colors);
         let body = entry_body(head, &text, images, &actions, contribution, colors);
         entry_card(avatar, body, &side, hold, contribution, muted)
      },
      Block::Buttons(buttons) => {
         let kids = buttons
            .into_iter()
            .filter_map(|action| {
               let mut button = button(&action.label, action.danger, colors);
               button.set(prop!("flex"), Some(Value::F64(1.0)));
               Some(pressable(
                  button,
                  block_handler(contribution, action.handler?),
               ))
            })
            .collect();
         view(
            vec![
               (prop!("direction"), str("row")),
               (prop!("gap"), Value::F64(8.0)),
            ],
            kids,
         )
      },
      Block::Button {
         label: text,
         danger,
         handler,
      } => {
         pressable(
            button(&text, danger, colors),
            block_handler(contribution, handler),
         )
      },
      Block::Input {
         placeholder,
         handler,
         send,
      } => input(&placeholder, contribution, handler, send, colors),
   }
}

fn entry_head(
   title: &str,
   badges: Vec<String>,
   subtitle: Option<String>,
   colors: Palette,
) -> Draft {
   // Like Vencord's: avatar, then name, badges and date over the text.
   let mut head = vec![{
      let mut name = label(title, 15.0, 600, colors.text);
      name.set(prop!("maxLines"), Some(Value::I32(1)));
      name.set(prop!("shrink"), Some(Value::Bool(true)));
      name
   }];
   for badge in badges {
      head.push(image(&badge, 16.0, 0.0));
   }
   if let Some(subtitle) = subtitle {
      head.push(label(&subtitle, 12.0, 500, colors.muted));
   }
   view(
      vec![
         (prop!("direction"), str("row")),
         (prop!("gap"), Value::F64(6.0)),
         (prop!("align"), str("center")),
      ],
      head,
   )
}

fn entry_body(
   head: Draft,
   text: &str,
   images: Vec<String>,
   actions: &[BlockAction],
   contribution: ContributionId,
   colors: Palette,
) -> Draft {
   let mut kids = vec![head, rich(text, colors.text)];
   for uri in images {
      let mut picture = image(&uri, 0.0, 8.0);
      picture.set(prop!("width"), Some(Value::F64(220.0)));
      picture.set(prop!("height"), Some(Value::F64(160.0)));
      picture.set(prop!("scale"), Some(str("fit")));
      picture.set(prop!("align"), Some(str("start")));
      kids.push(picture);
   }
   if !actions.is_empty() {
      let row = actions
         .iter()
         .map(|action| {
            action_label(
               action,
               13.0,
               600,
               [2.0, 4.0, 2.0, 4.0],
               contribution,
               colors.muted,
            )
         })
         .collect();
      kids.push(view(
         vec![
            (prop!("direction"), str("row")),
            (prop!("gap"), Value::F64(14.0)),
            (prop!("align"), str("center")),
         ],
         row,
      ));
   }
   view(
      vec![
         (prop!("direction"), str("column")),
         (prop!("flex"), Value::F64(1.0)),
         (prop!("gap"), Value::F64(4.0)),
         (prop!("align"), str("stretch")),
      ],
      kids,
   )
}

fn entry_card(
   avatar: Option<String>,
   body: Draft,
   side: &[BlockAction],
   hold: Option<u32>,
   contribution: ContributionId,
   muted: i64,
) -> Draft {
   let mut row = Vec::new();
   if let Some(avatar) = avatar {
      row.push(image(&avatar, 40.0, 20.0));
   }
   row.push(body);
   if !side.is_empty() {
      let column = side
         .iter()
         .map(|action| action_label(action, 14.0, 700, [8.0, 2.0, 8.0, 2.0], contribution, muted))
         .collect();
      row.push(view(
         vec![
            (prop!("direction"), str("column")),
            (prop!("align"), str("center")),
            (prop!("gap"), Value::F64(2.0)),
         ],
         column,
      ));
   }
   let mut card = view(
      vec![
         (prop!("direction"), str("row")),
         (prop!("padding"), Value::Rect([0.0, 4.0, 0.0, 4.0])),
         (prop!("gap"), Value::F64(12.0)),
         (prop!("align"), str("start")),
      ],
      row,
   );
   if let Some(index) = hold {
      card.set(Prop::ON, Some(Value::Arr(vec![str("longPress")])));
      card.press = Some(Handler::Hold {
         contribution,
         index,
      });
   }
   card
}

fn action_label(
   action: &BlockAction,
   size: f64,
   weight: i32,
   padding: [f32; 4],
   contribution: ContributionId,
   muted: i64,
) -> Draft {
   let color = action
      .color
      .unwrap_or(if action.danger { DANGER } else { muted });
   let mut piece = label(&action.label, size, weight, color);
   piece.set(Prop::PADDING, Some(Value::Rect(padding)));
   match action.handler {
      Some(handler) => pressable(piece, block_handler(contribution, handler)),
      None => piece,
   }
}

fn image(uri: &str, size: f64, radius: f64) -> Draft {
   let mut props = vec![(prop!("uri"), str(uri))];
   if size > 0.0 {
      props.push((prop!("width"), Value::F64(size)));
      props.push((prop!("height"), Value::F64(size)));
   }
   if radius > 0.0 {
      props.push((prop!("radius"), Value::F64(radius)));
   }
   node(ViewKind::Image, props)
}

/// `text` with Discord's custom emoji (`<:name:id>`, `<a:name:id>`) drawn as
/// images, the words wrapping around them.
fn rich(text: &str, color: i64) -> Draft {
   let pieces = emoji_pieces(text);
   if !pieces.iter().any(|piece| matches!(piece, Piece::Emoji(_))) {
      return label(text, 14.0, 400, color);
   }
   let mut kids = Vec::new();
   for piece in pieces {
      match piece {
         Piece::Text(text) => {
            for word in text.split_inclusive(' ') {
               kids.push(label(word, 14.0, 400, color));
            }
         },
         Piece::Emoji(uri) => kids.push(image(&uri, 20.0, 0.0)),
      }
   }
   let mut flow = view(
      vec![
         (prop!("direction"), str("row")),
         (prop!("align"), str("center")),
      ],
      kids,
   );
   flow.kind = ViewKind::Flow;
   flow
}

#[derive(Debug, PartialEq)]
enum Piece {
   Text(String),
   /// The emoji's image URL.
   Emoji(String),
}

fn emoji_pieces(text: &str) -> Vec<Piece> {
   let mut pieces = Vec::new();
   let mut rest = text;
   while let Some(start) = rest.find('<') {
      let tail = &rest[start + 1..];
      let parsed = tail.split_once('>').and_then(|(inner, after)| {
         let (animated, inner) = match inner.strip_prefix("a:") {
            Some(inner) => (true, inner),
            None => (false, inner.strip_prefix(':')?),
         };
         let (name, id) = inner.split_once(':')?;
         let valid =
            !name.is_empty() && !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit());
         valid.then(|| {
            let ext = if animated { "gif" } else { "webp" };
            (
               format!("https://cdn.discordapp.com/emojis/{id}.{ext}?size=48"),
               after,
            )
         })
      });
      if let Some((uri, after)) = parsed {
         if start > 0 {
            pieces.push(Piece::Text(rest[..start].to_owned()));
         }
         pieces.push(Piece::Emoji(uri));
         rest = after;
      } else {
         pieces.push(Piece::Text(rest[..=start].to_owned()));
         rest = tail;
      }
   }
   if !rest.is_empty() {
      pieces.push(Piece::Text(rest.to_owned()));
   }
   pieces
}

fn button(text: &str, danger: bool, colors: Palette) -> Draft {
   view(
      vec![
         (prop!("direction"), str("row")),
         (prop!("padding"), Value::Rect([16.0, 10.0, 16.0, 10.0])),
         (prop!("minHeight"), Value::F64(40.0)),
         (prop!("align"), str("center")),
         (prop!("justify"), str("center")),
         (prop!("background"), colors.wash(0x1F)),
         (prop!("activeBackground"), colors.wash(0x33)),
         (prop!("radius"), Value::F64(8.0)),
         (prop!("borderColor"), colors.wash(0x26)),
         (prop!("borderWidth"), Value::F64(1.0)),
         (prop!("accessibilityLabel"), str(text)),
      ],
      vec![label(
         text,
         14.0,
         600,
         if danger { DANGER } else { colors.text },
      )],
   )
}

/// Phoenix's search box without its icon, `root > frame > row > input`.
fn input(
   placeholder: &str,
   contribution: ContributionId,
   index: u32,
   send: bool,
   colors: Palette,
) -> Draft {
   let mut root = Draft::from_node(&templates().input);
   let Some(row) = root
      .kids
      .first_mut()
      .and_then(|frame| frame.kids.first_mut())
   else {
      return root;
   };
   if row.kids.len() > 1 {
      row.kids.remove(0);
   }
   if let Some(input) = row.kids.first_mut() {
      input.set(Prop::MARGIN, None);
      input.set(Prop::PLACEHOLDER, Some(str(placeholder)));
      input.set(Prop::VALUE, Some(str("")));
      input.set(Prop::RETURN_KEY, Some(str("send")));
      input.set(Prop::COLOR, Some(Value::I64(colors.text)));
      input.set(prop!("placeholderColor"), Some(Value::I64(colors.muted)));
      input.set(Prop::ROLE, None);
      input.set(Prop::ACCESSIBILITY_ID, None);
      input.set(prop!("autoCapitalize"), Some(str("sentences")));
      input.set(prop!("autoCorrect"), Some(Value::Bool(true)));
      input.set(
         Prop::ON,
         Some(Value::Arr(vec![str("changeText"), str("submit")])),
      );
      input.press = Some(Handler::Input {
         contribution,
         index,
      });
   }
   if send {
      let mut icon = image("res://ic_mana_send_message", 22.0, 0.0);
      icon.set(Prop::TINT, Some(Value::I64(colors.text)));
      let wrap = view(
         vec![
            (prop!("padding"), Value::Rect([8.0, 8.0, 10.0, 8.0])),
            (prop!("align"), str("center")),
            (prop!("justify"), str("center")),
            (prop!("radius"), Value::F64(16.0)),
            (prop!("activeBackground"), colors.wash(0x33)),
            (prop!("accessibilityLabel"), str("Send")),
         ],
         vec![icon],
      );
      row.kids.push(pressable(wrap, Handler::Send {
         contribution,
         index,
      }));
   }
   root
}

#[cfg(test)]
mod tests {
   use crate::patches::tabs::{
      Piece,
      emoji_pieces,
   };

   #[test]
   fn splits_custom_emoji() {
      assert_eq!(emoji_pieces("hi <:wave:123> a<b <a:spin:9>"), vec![
         Piece::Text("hi ".to_owned()),
         Piece::Emoji("https://cdn.discordapp.com/emojis/123.webp?size=48".to_owned()),
         Piece::Text(" a<".to_owned()),
         Piece::Text("b ".to_owned()),
         Piece::Emoji("https://cdn.discordapp.com/emojis/9.gif?size=48".to_owned()),
      ]);
   }
}
