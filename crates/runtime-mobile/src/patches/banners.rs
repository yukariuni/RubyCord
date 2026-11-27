//! Native in-app notification banners over the root view.

use std::{
   sync::atomic::{
      AtomicU32,
      Ordering,
   },
   thread,
   time::{
      Duration,
      Instant,
   },
};

use phoenix_ui_mobile::{
   Op,
   Prop,
   ViewKind,
   prop,
};
use phoenix_wire::Value;

use crate::{
   bridge,
   js::ui::{
      banner_closed,
      banner_press,
   },
   patches::{
      ACTIVE,
      BACKGROUND,
      Before,
      Draft,
      Handler,
      Patches,
      Place,
      Slot,
      SlotKind,
      patches,
      worker::{
         Job,
         queue,
      },
   },
};

/// What happens when the user taps an in-app notification banner.
pub enum BannerAction {
   None,
   ApplyUpdate,
   JsCallback,
}

/// Specification for a native Discord in-app notification banner.
pub struct BannerSpec {
   pub id:          u32,
   pub title:       String,
   pub body:        Option<String>,
   pub icon:        Option<String>,
   pub accent:      Option<i64>,
   pub duration_ms: u64,
   pub action:      BannerAction,
}

/// Shows a native Discord in-app notification banner at the top of the screen.
/// Returns the banner id so it can be dismissed early with [`dismiss_banner`].
pub fn show_banner(
   title: String,
   body: Option<String>,
   icon: Option<String>,
   accent: Option<i64>,
   duration_ms: u64,
   action: BannerAction,
) -> u32 {
   static NEXT: AtomicU32 = AtomicU32::new(1);
   let id = NEXT.fetch_add(1, Ordering::Relaxed);
   ACTIVE.store(true, Ordering::Release);
   queue(Job::ShowBanner(BannerSpec {
      id,
      title,
      body,
      icon,
      accent,
      duration_ms,
      action,
   }));
   id
}

/// Dismisses an in-app notification banner early if it is still visible.
pub fn dismiss_banner(id: u32) {
   queue(Job::DismissBanner(id));
}

/// Delay after the first `Op::Root` before showing cold-start banners so they
/// appear over the live screen rather than behind the splash animation.
const BANNER_STARTUP_DELAY: Duration = Duration::from_millis(1500);

impl Patches {
   pub fn note_root_ready(&mut self) {
      if self.root_ready_at.is_none() {
         self.root_ready_at = Some(Instant::now());
      }
      if !self.pending_banners.is_empty() && !self.banner_flush_scheduled {
         self.banner_flush_scheduled = true;
         let delay = self.root_ready_at.map_or(Duration::ZERO, |at| {
            BANNER_STARTUP_DELAY.saturating_sub(at.elapsed())
         });
         thread::spawn(move || {
            if !delay.is_zero() {
               thread::sleep(delay);
            }
            queue(Job::FlushBanners);
         });
      }
   }

   pub fn show_banner(&mut self, spec: BannerSpec) -> Vec<Op> {
      let ready = self.mirror.root().is_some()
         && bridge::ready()
         && self
            .root_ready_at
            .is_some_and(|at| at.elapsed() >= BANNER_STARTUP_DELAY);
      if !ready {
         self.pending_banners.push(spec);
         if self.mirror.root().is_some() {
            self.note_root_ready();
         }
         return Vec::new();
      }
      self.open_banner(spec)
   }

   pub fn flush_banners(&mut self) -> Vec<Op> {
      self.banner_flush_scheduled = false;
      if self.mirror.root().is_none() || !bridge::ready() {
         return Vec::new();
      }
      if self
         .root_ready_at
         .is_some_and(|at| at.elapsed() < BANNER_STARTUP_DELAY)
      {
         self.note_root_ready();
         return Vec::new();
      }
      let queued = std::mem::take(&mut self.pending_banners);
      let mut out = Vec::new();
      for spec in queued {
         out.extend(self.open_banner(spec));
      }
      out
   }

   /// Attaches a native Phoenix `banner` overlay to the root view.
   fn open_banner(&mut self, spec: BannerSpec) -> Vec<Op> {
      let Some(root) = self.mirror.root() else {
         self.pending_banners.push(spec);
         return Vec::new();
      };
      // Replace any currently visible banner so notifications don't pile up.
      let mut out = Vec::new();
      let existing = self
         .slots
         .iter()
         .filter_map(|slot| {
            match slot.kind {
               SlotKind::Banner(id) => Some(id),
               _ => None,
            }
         })
         .collect::<Vec<_>>();
      for old_id in existing {
         let (close_ops, _) = self.close_banner(old_id);
         out.extend(close_ops);
      }

      let id = spec.id;
      let announce = match spec.body.as_deref().filter(|text| !text.is_empty()) {
         Some(body) => format!("{}. {body}", spec.title),
         None => spec.title.clone(),
      };
      let icon_uri = spec
         .icon
         .unwrap_or_else(|| "res://ic_mana_puzzle_piece_plus".to_owned());
      let accent = spec.accent.unwrap_or(0xFF58_65F2);

      let banner = banner_view(id, announce, spec.title, spec.body, icon_uri, accent);

      let mut all = Vec::new();
      let banner_root = self.emit(banner, None, &mut out, &mut all);
      self.slots.push(Slot {
         kind: SlotKind::Banner(id),
         anchor: root,
         parent: root,
         before: Before::End,
         place: Place::Screen,
         roots: vec![banner_root],
         all,
      });
      self.banner_actions.insert(id, spec.action);
      out.push(Op::Children {
         id:  root,
         ids: self.merged(root),
      });
      if spec.duration_ms > 0 {
         let dur = Duration::from_millis(spec.duration_ms);
         thread::spawn(move || {
            thread::sleep(dur);
            queue(Job::DismissBanner(id));
         });
      }
      out
   }

   fn close_banner(&mut self, id: u32) -> (Vec<Op>, bool) {
      let mut had_js = false;
      self.pending_banners.retain(|spec| {
         if spec.id == id {
            if matches!(spec.action, BannerAction::JsCallback) {
               had_js = true;
            }
            false
         } else {
            true
         }
      });
      if matches!(
         self.banner_actions.remove(&id),
         Some(BannerAction::JsCallback)
      ) {
         had_js = true;
      }
      let Some(at) = self
         .slots
         .iter()
         .position(|slot| slot.kind == SlotKind::Banner(id))
      else {
         return (Vec::new(), had_js);
      };
      let slot = self.slots.remove(at);
      self.forget_ids(&slot.all);
      let mut out = Vec::new();
      if self.mirror.has_kids(slot.parent) {
         out.push(Op::Children {
            id:  slot.parent,
            ids: self.merged(slot.parent),
         });
      }
      out.push(Op::Delete { ids: slot.all });
      (out, had_js)
   }

   fn press_banner(&mut self, id: u32) -> (Vec<Op>, BannerAction) {
      let action = self
         .banner_actions
         .remove(&id)
         .unwrap_or(BannerAction::None);
      let (ops, _) = self.close_banner(id);
      (ops, action)
   }
}

fn banner_view(
   id: u32,
   announce: String,
   title: String,
   body: Option<String>,
   icon_uri: String,
   accent: i64,
) -> Draft {
   let badge = banner_badge(icon_uri, accent);
   let text_col = banner_text(title, body);
   let dismiss_btn = banner_dismiss(id);

   let body_hit = Draft {
      kind:  ViewKind::View,
      props: vec![
         (prop!("direction"), Value::Str("row".to_owned())),
         (prop!("flex"), Value::F64(1.0)),
         (prop!("align"), Value::Str("center".to_owned())),
         (Prop::GAP, Value::F64(12.0)),
         (Prop::PADDING, Value::Rect([12.0, 12.0, 6.0, 12.0])),
         (prop!("activeBackground"), Value::I64(0xFF2B_2D31)),
         (Prop::ROLE, Value::Str("button".to_owned())),
         (Prop::ACCESSIBILITY_LABEL, Value::Str(announce.clone())),
         (
            Prop::ACCESSIBILITY_ID,
            Value::Str("phoenix.in-app-notification.body".to_owned()),
         ),
         (Prop::ON, Value::Arr(vec![Value::Str("press".to_owned())])),
      ],
      kids:  vec![badge, text_col],
      press: Some(Handler::BannerPress(id)),
      src:   None,
   };

   let card = Draft {
      kind:  ViewKind::View,
      props: vec![
         (prop!("direction"), Value::Str("row".to_owned())),
         (prop!("width"), Value::Str("fill".to_owned())),
         (prop!("align"), Value::Str("center".to_owned())),
         (Prop::MARGIN, Value::Rect([12.0, 8.0, 12.0, 0.0])),
         (BACKGROUND, Value::I64(0xFF23_2428)),
         (Prop::RADIUS, Value::F64(16.0)),
         (prop!("clip"), Value::Bool(true)),
         (prop!("borderWidth"), Value::F64(1.0)),
         (prop!("borderColor"), Value::I64(0x26FF_FFFF)),
         (
            Prop::ACCESSIBILITY_ID,
            Value::Str("phoenix.in-app-notification".to_owned()),
         ),
      ],
      kids:  vec![body_hit, dismiss_btn],
      press: None,
      src:   None,
   };

   Draft {
      kind:  ViewKind::Banner,
      props: vec![
         (prop!("edge"), Value::Str("top".to_owned())),
         (prop!("passthrough"), Value::Bool(false)),
         (prop!("announce"), Value::Str(announce)),
         (prop!("announcePolite"), Value::Bool(true)),
      ],
      kids:  vec![card],
      press: None,
      src:   None,
   }
}

fn banner_badge(icon_uri: String, accent: i64) -> Draft {
   let icon = Draft {
      kind:  ViewKind::Image,
      props: vec![
         (Prop::URI, Value::Str(icon_uri)),
         (prop!("width"), Value::F64(22.0)),
         (Prop::HEIGHT, Value::F64(22.0)),
         (Prop::TINT, Value::I64(0xFFFF_FFFF)),
         (prop!("scale"), Value::Str("fit".to_owned())),
      ],
      kids:  Vec::new(),
      press: None,
      src:   None,
   };
   Draft {
      kind:  ViewKind::View,
      props: vec![
         (prop!("width"), Value::F64(40.0)),
         (Prop::HEIGHT, Value::F64(40.0)),
         (Prop::RADIUS, Value::F64(20.0)),
         (BACKGROUND, Value::I64(accent)),
         (prop!("align"), Value::Str("center".to_owned())),
         (prop!("justify"), Value::Str("center".to_owned())),
      ],
      kids:  vec![icon],
      press: None,
      src:   None,
   }
}

fn banner_text(title: String, body: Option<String>) -> Draft {
   let mut text_kids = vec![Draft {
      kind:  ViewKind::Text,
      props: vec![
         (Prop::TEXT, Value::Str(title)),
         (Prop::COLOR, Value::I64(0xFFF2_F3F5)),
         (Prop::FONT_SIZE, Value::F64(15.0)),
         (prop!("fontWeight"), Value::I32(700)),
         (Prop::LINE_HEIGHT, Value::F64(19.0)),
         (prop!("maxLines"), Value::I32(1)),
         (prop!("ellipsize"), Value::Str("end".to_owned())),
      ],
      kids:  Vec::new(),
      press: None,
      src:   None,
   }];
   if let Some(body) = body.filter(|text| !text.is_empty()) {
      text_kids.push(Draft {
         kind:  ViewKind::Text,
         props: vec![
            (Prop::TEXT, Value::Str(body)),
            (Prop::COLOR, Value::I64(0xFFB5_BAC1)),
            (Prop::FONT_SIZE, Value::F64(13.0)),
            (prop!("fontWeight"), Value::I32(400)),
            (Prop::LINE_HEIGHT, Value::F64(17.0)),
            (prop!("maxLines"), Value::I32(2)),
            (prop!("ellipsize"), Value::Str("end".to_owned())),
         ],
         kids:  Vec::new(),
         press: None,
         src:   None,
      });
   }
   Draft {
      kind:  ViewKind::View,
      props: vec![
         (prop!("direction"), Value::Str("column".to_owned())),
         (prop!("flex"), Value::F64(1.0)),
         (Prop::GAP, Value::F64(2.0)),
      ],
      kids:  text_kids,
      press: None,
      src:   None,
   }
}

fn banner_dismiss(id: u32) -> Draft {
   let dismiss_icon = Draft {
      kind:  ViewKind::Image,
      props: vec![
         (Prop::URI, Value::Str("res://ic_mana_x_small".to_owned())),
         (prop!("width"), Value::F64(16.0)),
         (Prop::HEIGHT, Value::F64(16.0)),
         (Prop::TINT, Value::I64(0xFF94_9BA4)),
         (prop!("scale"), Value::Str("fit".to_owned())),
      ],
      kids:  Vec::new(),
      press: None,
      src:   None,
   };
   Draft {
      kind:  ViewKind::View,
      props: vec![
         (prop!("width"), Value::F64(36.0)),
         (Prop::HEIGHT, Value::F64(36.0)),
         (Prop::MARGIN, Value::Rect([0.0, 0.0, 8.0, 0.0])),
         (Prop::RADIUS, Value::F64(18.0)),
         (prop!("align"), Value::Str("center".to_owned())),
         (prop!("justify"), Value::Str("center".to_owned())),
         (prop!("activeBackground"), Value::I64(0x1AFF_FFFF)),
         (Prop::ROLE, Value::Str("button".to_owned())),
         (Prop::ACCESSIBILITY_LABEL, Value::Str("Dismiss".to_owned())),
         (
            Prop::ACCESSIBILITY_ID,
            Value::Str("phoenix.in-app-notification.dismiss".to_owned()),
         ),
         (Prop::ON, Value::Arr(vec![Value::Str("press".to_owned())])),
      ],
      kids:  vec![dismiss_icon],
      press: Some(Handler::BannerDismiss(id)),
      src:   None,
   }
}

pub fn dismiss(id: u32) -> Vec<Op> {
   let (ops, notify_js) = patches().close_banner(id);
   if notify_js && let Some(engine) = phoenix_runtime::engine() {
      banner_closed(&engine, id);
   }
   ops
}

pub fn pressed(id: u32) -> Vec<Op> {
   let (ops, action) = patches().press_banner(id);
   match action {
      BannerAction::None => {},
      BannerAction::ApplyUpdate => {
         phoenix_runtime::update::apply();
      },
      BannerAction::JsCallback => {
         if let Some(engine) = phoenix_runtime::engine() {
            banner_press(&engine, id);
         }
      },
   }
   ops
}
