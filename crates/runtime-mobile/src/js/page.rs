//! Profile tab pages drawn from blocks, and full-screen pages from
//! `phoenix.ui.openPage`.

use phoenix_runtime::js::Engine;
use rquickjs::{
   CatchResultExt,
   Ctx,
   Exception,
   Function,
   Object,
   Value,
};

use crate::{
   js::Mobile,
   patches::{
      self,
      ContributionId,
      Outcome,
      Place,
   },
};

/// Profiles whose page callbacks a tab keeps.
const PAGES_CAP: usize = 64;

/// Runs handler `index` of the page contribution `id` drew at `place`,
/// with the text of the box it belongs to, if any.
pub fn ui_block(
   engine: &Engine,
   id: ContributionId,
   place: &Place,
   index: u32,
   text: Option<String>,
) {
   engine.with(|ctx| {
      let Some((plugin, handler)) = ctx.userdata::<Mobile<'_>>().and_then(|registry| {
         let callbacks = registry.ui.borrow();
         let callbacks = callbacks.get(&id)?;
         let handler = callbacks
            .pages
            .get(place)?
            .get(usize::try_from(index).ok()?)?;
         Some((callbacks.plugin.clone(), handler.clone()))
      }) else {
         return;
      };
      if let Err(err) = handler.call::<_, Value<'_>>((text,)).catch(&ctx) {
         log::error!("[{plugin}] page: {err}");
      }
   });
}

/// Runs button `index` on plugin full-screen page `page`.
pub fn page_press(engine: &Engine, page: u32, index: u32) {
   engine.with(|ctx| {
      let Some((plugin, handler)) = ctx.userdata::<Mobile<'_>>().and_then(|registry| {
         let pages = registry.open_pages.borrow();
         let (plugin, handlers) = pages.get(&page)?;
         let handler = handlers.get(usize::try_from(index).ok()?)?;
         Some((plugin.clone(), handler.clone()))
      }) else {
         return;
      };
      if let Err(err) = handler.call::<_, Value<'_>>(()).catch(&ctx) {
         log::error!("[{plugin}] page button: {err}");
      }
   });
}

/// Drops button callbacks for a closed plugin full-screen page.
pub fn page_closed(engine: &Engine, page: u32) {
   engine.with(|ctx| {
      if let Some(registry) = ctx.userdata::<Mobile<'_>>() {
         registry.open_pages.borrow_mut().remove(&page);
      }
   });
}

/// A profile tab's `render` result as the page it draws. Its callbacks stay
/// here, numbered in the order the blocks name them.
pub fn page_of<'js>(
   ctx: &Ctx<'js>,
   id: ContributionId,
   place: &Place,
   value: Option<&Value<'js>>,
) -> Outcome {
   let mut handlers = Vec::new();
   let blocks = value.and_then(Value::as_array).map_or_else(
      || {
         vec![patches::Block::Text {
            title: None,
            text:  "This tab could not be drawn.".to_owned(),
            color: None,
         }]
      },
      |array| {
         array
            .iter::<Value<'js>>()
            .filter_map(|block| block_of(block.ok()?.as_object()?, &mut handlers))
            .collect()
      },
   );
   if let Some(registry) = ctx.userdata::<Mobile<'js>>()
      && let Some(callbacks) = registry.ui.borrow_mut().get_mut(&id)
   {
      if callbacks.pages.len() >= PAGES_CAP {
         callbacks.pages.clear();
      }
      callbacks.pages.insert(place.clone(), handlers);
   }
   Outcome::Page(blocks)
}

/// The `{label, danger?, color?, onPress?}` objects in `object[key]`.
fn actions_of<'js>(
   object: &Object<'js>,
   key: &str,
   handler: &mut impl FnMut(&Object<'js>, &str) -> Option<u32>,
) -> Vec<patches::BlockAction> {
   let Some(array) = object
      .get::<_, Option<rquickjs::Array<'js>>>(key)
      .ok()
      .flatten()
   else {
      return Vec::new();
   };
   array
      .iter::<Object<'js>>()
      .filter_map(|action| {
         let action = action.ok()?;
         Some(patches::BlockAction {
            label:   action.get::<_, Option<String>>("label").ok().flatten()?,
            danger:  action
               .get::<_, Option<bool>>("danger")
               .ok()
               .flatten()
               .unwrap_or_default(),
            color:   action
               .get::<_, Option<f64>>("color")
               .ok()
               .flatten()
               .map(|color| {
                  #[expect(clippy::cast_possible_truncation, reason = "colors are 32-bit ARGB")]
                  let color = color as i64;
                  color
               }),
            handler: handler(&action, "onPress"),
         })
      })
      .collect()
}

fn block_of<'js>(
   object: &Object<'js>,
   handlers: &mut Vec<Function<'js>>,
) -> Option<patches::Block> {
   let text = |object: &Object<'js>, key: &str| object.get::<_, Option<String>>(key).ok().flatten();
   let flag = |object: &Object<'js>, key: &str| {
      object
         .get::<_, Option<bool>>(key)
         .ok()
         .flatten()
         .unwrap_or_default()
   };
   let mut handler = |object: &Object<'js>, key: &str| {
      let function = object.get::<_, Option<Function<'js>>>(key).ok().flatten()?;
      handlers.push(function);
      u32::try_from(handlers.len() - 1).ok()
   };
   let block = match text(object, "type")?.as_str() {
      "text" => {
         patches::Block::Text {
            title: text(object, "title"),
            text:  text(object, "text")?,
            color: object
               .get::<_, Option<f64>>("color")
               .ok()
               .flatten()
               .map(|color| {
                  #[expect(clippy::cast_possible_truncation, reason = "colors are 32-bit ARGB")]
                  let color = color as i64;
                  color
               }),
         }
      },
      "entry" => {
         let actions = actions_of(object, "actions", &mut handler);
         let strings = |key: &str| {
            object
               .get::<_, Option<Vec<String>>>(key)
               .ok()
               .flatten()
               .unwrap_or_default()
         };
         patches::Block::Entry {
            avatar: text(object, "avatar"),
            title: text(object, "title")?,
            subtitle: text(object, "subtitle"),
            text: text(object, "text").unwrap_or_default(),
            badges: strings("badges"),
            images: strings("images"),
            actions,
            side: actions_of(object, "side", &mut handler),
            hold: handler(object, "onLongPress"),
         }
      },
      "buttons" => patches::Block::Buttons(actions_of(object, "buttons", &mut handler)),
      "input" => {
         let submit = handler(object, "onSubmit")?;
         patches::Block::Input {
            placeholder: text(object, "placeholder").unwrap_or_default(),
            handler:     submit,
            send:        flag(object, "sendButton"),
         }
      },
      "button" => {
         patches::Block::Button {
            label:   text(object, "label")?,
            danger:  flag(object, "danger"),
            handler: handler(object, "onPress")?,
         }
      },
      _ => return None,
   };
   Some(block)
}

pub fn add_page<'js>(ctx: &Ctx<'js>, plugin: &str, ui: &Object<'js>) -> rquickjs::Result<()> {
   let owner = plugin.to_owned();
   ui.set(
      "openPage",
      Function::new(ctx.clone(), move |ctx: Ctx<'js>, options: Object<'js>| {
         static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
         let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
         let title = options
            .get::<_, Option<String>>("title")?
            .unwrap_or_else(|| owner.clone());
         let raw_sections = options
            .get::<_, Option<Vec<Object<'js>>>>("sections")?
            .unwrap_or_default();
         let mut handlers = Vec::new();
         let mut sections = Vec::new();
         for sec in raw_sections {
            let kind = sec.get::<_, Option<String>>("type")?.unwrap_or_default();
            let heading = sec.get::<_, Option<String>>("title")?;
            match kind.as_str() {
               "code" => {
                  sections.push(patches::pages::PageSection::Code {
                     title:     heading,
                     text:      sec.get::<_, Option<String>>("text")?.unwrap_or_default(),
                     monospace: sec.get::<_, Option<bool>>("monospace")?.unwrap_or(true),
                     highlight: sec.get::<_, Option<bool>>("highlight")?.unwrap_or(false),
                  });
               },
               "text" => {
                  sections.push(patches::pages::PageSection::Text {
                     title: heading,
                     text:  sec.get::<_, Option<String>>("text")?.unwrap_or_default(),
                  });
               },
               "buttons" => {
                  let raw_buttons = sec
                     .get::<_, Option<Vec<Object<'js>>>>("buttons")?
                     .unwrap_or_default();
                  let mut buttons = Vec::new();
                  for btn in raw_buttons {
                     let Some(label) = btn.get::<_, Option<String>>("label")? else {
                        continue;
                     };
                     let Some(on_press) = btn.get::<_, Option<Function<'js>>>("onPress")? else {
                        continue;
                     };
                     #[expect(clippy::cast_possible_truncation, reason = "pages have few buttons")]
                     let index = handlers.len() as u32;
                     handlers.push(on_press);
                     buttons.push(patches::pages::PageButton {
                        label,
                        icon: btn.get::<_, Option<String>>("icon")?,
                        danger: btn.get::<_, Option<bool>>("danger")?.unwrap_or(false),
                        index,
                     });
                  }
                  sections.push(patches::pages::PageSection::Buttons {
                     title: heading,
                     buttons,
                  });
               },
               _ => {},
            }
         }
         ctx.userdata::<Mobile<'js>>()
            .ok_or_else(|| Exception::throw_internal(&ctx, "no registry"))?
            .open_pages
            .borrow_mut()
            .insert(id, (owner.clone(), handlers));
         patches::pages::open_page(id, title, sections);
         let handle = Object::new(ctx.clone())?;
         handle.set(
            "close",
            Function::new(ctx, move || patches::pages::close_page(id))?,
         )?;
         Ok::<_, rquickjs::Error>(handle)
      })?,
   )?;
   Ok(())
}
