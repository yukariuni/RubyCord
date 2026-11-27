//! Injects a native GPUI **Ruby** tab directly into Desktop Phoenix's User
//! Settings sidebar and renders native Mana GPUI settings controls when
//! selected.
//!
//! All patches are build-independent anchor lookups run from [`crate::prepare`]
//! inside `DllMain` before Discord initializes:
//! 1. Relabel settings page `24` (`Other -> Phoenix`, right above `Log Out`) to
//!    `Ruby` by replacing the unique 22-byte `.rdata` string
//!    `PhoenixDESKTOP_PHOENIX` in-place with `Ruby   DESKTOP_RUBY_UI`; the string
//!    lengths live in the code that reads them, so the new text pads with spaces to match.
//! 2. Unconditionally show page `24` in `is_page_visible` by tightening the
//!    `cmp r8d, 0xf` range guard (`pages 9..=24`) to `0x0e` (`pages 9..=23`),
//!    so page `24` falls through to `return true`.
//! 3. Resolve Discord's native GPUI/Mana widget builders ([`crate::gpui`]) and
//!    inline-hook `host::section` (located via the unique `account-loading`
//!    string xref and `.pdata` function bounds) so when active page `== 24`, it
//!    builds and returns a native GPUI `AnyElement` directly into Discord's
//!    User Settings scroll viewport.

use std::{
   ffi::c_void,
   sync::{
      OnceLock,
      atomic::{AtomicUsize, Ordering},
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::desktop;

use crate::gpui::{self, RawAnyElement};

const RUBY_PAGE: u8 = 24;

type SectionFn = unsafe extern "C" fn(
   sret: *mut RawAnyElement,
   host_state: *mut c_void,
   host_data: *mut c_void,
   host_vtable: *const c_void,
   window: *mut c_void,
   cx: *mut c_void,
) -> *mut RawAnyElement;

static ORIG_SECTION: OnceLock<SectionFn> = OnceLock::new();
static PAGE_OFFSET: AtomicUsize = AtomicUsize::new(0);
/// The page the last section render showed, so the arm64 page opens the
/// settings window once per visit rather than once per frame.
static LAST_PAGE: AtomicUsize = AtomicUsize::new(usize::MAX);

/// Installs the Desktop User Settings sidebar label, visibility patch, native
/// GPUI builder bindings, and `host::section` render hook.
pub fn install(image: &Image<'static>) {
   if !crate::X86_CODE {
      #[cfg(target_os = "macos")]
      install_arm64(image);
      return;
   }
   patch_sidebar_label(image);
   patch_page_visibility(image);
   if gpui::locate(image) {
      hook_section_render(image);
   }
}

/// macOS Phoenix, arm64: the same sidebar entry, unhidden, whose page is
/// drawn with Discord's own builders ([`gpui::locate`]), or opens Ruby's
/// settings window when they don't decode. Each part is located before
/// anything is written, so a build that moved one changes nothing.
#[cfg(target_os = "macos")]
fn install_arm64(image: &Image<'static>) {
   use phoenix_sigs::macos;

   const NEW_NAME: &[u8] = b"Ruby   ";
   const NEW_ID: &[u8] = b"DESKTOP_RUBY_UI";
   const _: () = assert!(NEW_NAME.len() == b"Phoenix".len());
   const _: () = assert!(NEW_ID.len() == b"DESKTOP_PHOENIX".len());

   let (label, limits, section) = match (
      macos::settings_label_parts(image),
      macos::settings_visibility(image),
      macos::settings_section(image),
   ) {
      (Ok(label), Ok(limits), Ok(section)) => (label, limits, section),
      (label, limits, section) => {
         let err = [label.err(), limits.err(), section.err()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; ");
         log::warn!("desktop settings: {err}, leaving the sidebar");
         return;
      },
   };
   let Some(guards) = limits
      .iter()
      .map(|&limit| {
         image
            .read_u32(limit)
            .map(|cmp| (limit, macos::lowered_guard(cmp)))
      })
      .collect::<Option<Vec<_>>>()
   else {
      log::warn!("desktop settings: visibility guard unmapped");
      return;
   };
   PAGE_OFFSET.store(section.page_offset, Ordering::Release);
   let native = gpui::locate(image);
   let Some(detour) = phoenix_hook::image::sret_callee(section_detour as *const () as usize) else {
      log::warn!("desktop settings: no memory for the section hook");
      return;
   };
   // SAFETY: runs from `prepare` before Discord's threads; the callee thunk
   // hands `section_detour` the call the x86-64 way.
   let trampoline = match unsafe {
      // arm64 decodes the displaced instructions itself, so the x86 prologue
      // every caller passes is ignored.
      let pro = desktop::Prologues::of(phoenix_hook::x86::Abi::of(image)).frame;
      image.hook_entry(section.func, pro.pattern, pro.len, detour)
   } {
      Ok(trampoline) => trampoline,
      Err(err) => {
         log::warn!("desktop settings: section hook failed: {err}");
         return;
      },
   };
   let Some(orig) = phoenix_hook::image::sret_caller(trampoline) else {
      log::warn!("desktop settings: no memory for the section call");
      return;
   };
   // SAFETY: the caller thunk takes `SectionFn`'s x86-64 arguments and calls
   // the trampoline the arm64 way.
   let _ = ORIG_SECTION.set(unsafe { std::mem::transmute::<usize, SectionFn>(orig) });
   // SAFETY: exact-length string and one-instruction patches from `prepare`,
   // before Discord's threads run.
   let written = unsafe {
      guards
         .iter()
         .try_for_each(|&(limit, lowered)| image.write_code(limit, &lowered.to_le_bytes()))
         .and_then(|()| image.write_code(label.name, NEW_NAME))
         .and_then(|()| image.write_code(label.id, NEW_ID))
   };
   match written {
      Ok(()) => {
         log::info!(
            "desktop settings: page 24 is Ruby, shown by {} visibility checks, {}",
            guards.len(),
            if native {
               "drawn natively"
            } else {
               "and opens the settings window"
            }
         );
      },
      Err(err) => log::warn!("desktop settings: {err}"),
   }
}

fn patch_sidebar_label(image: &Image<'static>) {
   const NEW_NAME: &[u8] = b"Ruby   ";
   const NEW_ID: &[u8] = b"RUBY_UI";
   const _: () = assert!(NEW_NAME.len() == b"Phoenix".len());
   const _: () = assert!(NEW_ID.len() == b"PHOENIX".len());

   let label = match desktop::settings_label_parts(image) {
      Ok(label) => label,
      Err(err) => {
         log::warn!("desktop settings: {err}, leaving sidebar label");
         return;
      },
   };
   // SAFETY: exact-length `.rodata`/`.rdata` string patches executed from
   // `prepare` before Discord threads run.
   if let Err(err) = unsafe { image.write_code(label.name, NEW_NAME) } {
      log::warn!("desktop settings: relabel name failed: {err}");
      return;
   }
   match unsafe { image.write_code(label.id, NEW_ID) } {
      Ok(()) => log::info!("desktop settings: relabeled page 24 to Ruby"),
      Err(err) => log::warn!("desktop settings: relabel id failed: {err}"),
   }
}

fn patch_page_visibility(image: &Image<'static>) {
   let limit = match desktop::settings_visibility(image) {
      Ok(limit) => limit,
      Err(err) => {
         log::warn!("desktop settings: {err}, leaving it");
         return;
      },
   };
   // Tightening `cmp r8d, 0xf` -> `0x0e` excludes page 24 (`24 - 9 = 15`) from
   // the gated switch so `is_page_visible(24)` returns `true` unconditionally.
   // SAFETY: single-byte immediate patch in `.text` executed from `DllMain`.
   match unsafe { image.write_code(limit, &[0x0E]) } {
      Ok(()) => log::info!("desktop settings: page 24 visibility unlocked"),
      Err(err) => log::warn!("desktop settings: visibility patch failed: {err}"),
   }
}

fn hook_section_render(image: &Image<'static>) {
   let (fn_start, offset) = match desktop::settings_section(image) {
      Ok(section) => (section.func, section.page_offset),
      Err(err) => {
         log::warn!("desktop settings: {err}, skipping section hook");
         return;
      },
   };
   PAGE_OFFSET.store(offset, Ordering::Release);

   let pro = desktop::Prologues::of(phoenix_hook::x86::Abi::of(image)).frame;
   // SAFETY: runs from `prepare` before Discord threads execute;
   // `section_detour` matches `host::section`'s exact 6-argument sret ABI.
   let trampoline = match unsafe {
      image.hook_entry(
         fn_start,
         pro.pattern,
         pro.len,
         section_detour as *const () as usize,
      )
   } {
      Ok(trampoline) => trampoline,
      Err(err) => {
         log::warn!("desktop settings: section hook failed: {err}");
         return;
      },
   };
   // SAFETY: `trampoline` replays `fn_start`'s prologue and jumps to `fn_start
   // + 19`.
   let orig = unsafe { std::mem::transmute::<usize, SectionFn>(trampoline) };
   let _ = ORIG_SECTION.set(orig);
   log::info!("desktop settings: hooked section render @ {fn_start:#x} (page offset {offset:#x})");
}

unsafe extern "C" fn section_detour(
   sret: *mut RawAnyElement,
   host_state: *mut c_void,
   host_data: *mut c_void,
   host_vtable: *const c_void,
   window: *mut c_void,
   cx: *mut c_void,
) -> *mut RawAnyElement {
   if !host_state.is_null() && !sret.is_null() {
      let offset = PAGE_OFFSET.load(Ordering::Acquire);
      // SAFETY: `host_state` is Discord's live `&mut HostState` for this render
      // call.
      let page = unsafe { *host_state.cast::<u8>().add(offset) };
      let last = LAST_PAGE.swap(usize::from(page), Ordering::AcqRel);
      if !crate::X86_CODE
         && page == RUBY_PAGE
         && last != usize::from(page)
         && gpui::layout().is_none()
      {
         // Opening spawns a browser, which must not run on GPUI's thread.
         let _ = std::thread::Builder::new()
            .name("ruby-settings".to_owned())
            .spawn(phoenix_ui_desktop::open_devtools);
      }
      if page == RUBY_PAGE {
         let rendered = std::panic::catch_unwind(|| gpui::render_ruby_page(sret)).unwrap_or(false);
         if rendered {
            return sret;
         }
      }
   }
   let Some(orig) = ORIG_SECTION.get().copied() else {
      return sret;
   };
   // SAFETY: forwards all arguments verbatim to the original `host::section`.
   unsafe { orig(sret, host_state, host_data, host_vtable, window, cx) }
}
