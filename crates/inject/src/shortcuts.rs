//! The Start Menu and Desktop shortcuts, through the shell's `IShellLinkW`.
//! A hidden PowerShell child is one of the dropper patterns Defender flags.

use std::{
   ffi::{OsStr, OsString, c_void},
   fs,
   os::windows::ffi::{OsStrExt as _, OsStringExt as _},
   path::{Path, PathBuf},
   ptr, slice,
};

#[repr(C)]
struct Guid(u32, u16, u16, [u8; 8]);

impl Guid {
   #[expect(
      clippy::cast_possible_truncation,
      reason = "splits the u128 into its fields"
   )]
   const fn new(value: u128) -> Self {
      Self(
         (value >> 96) as u32,
         (value >> 80) as u16,
         (value >> 64) as u16,
         (value as u64).to_be_bytes(),
      )
   }
}

const FOLDER_PROGRAMS: Guid = Guid::new(0xA77F_5D77_2E2B_44C3_A6A2_ABA6_0105_4A51);
const FOLDER_DESKTOP: Guid = Guid::new(0xB4BF_CC3A_DB2C_424C_B029_7FE9_9A87_C641);
const SHELL_LINK: Guid = Guid::new(0x0002_1401_0000_0000_C000_0000_0000_0046);
const ISHELL_LINK_W: Guid = Guid::new(0x0002_14F9_0000_0000_C000_0000_0000_0046);
const IPERSIST_FILE: Guid = Guid::new(0x0000_010B_0000_0000_C000_0000_0000_0046);

const APARTMENT_THREADED: u32 = 2;
const INPROC_SERVER: u32 = 1;

type SetText = unsafe extern "system" fn(*mut c_void, *const u16) -> i32;

/// `IShellLinkW`'s vtable, with the slots it does not call left as padding.
#[repr(C)]
struct ShellLink {
   query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
   _add_ref: usize,
   release: unsafe extern "system" fn(*mut c_void) -> u32,
   _ids: [usize; 4],
   set_description: SetText,
   _working_dir: [usize; 3],
   set_arguments: SetText,
   _hotkey_show: [usize; 5],
   set_icon: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32,
   _relative: [usize; 2],
   set_path: SetText,
}

/// `IPersistFile`'s vtable up to `Save`.
#[repr(C)]
struct PersistFile {
   _unknown: [usize; 6],
   save: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32,
}

#[link(name = "ole32")]
unsafe extern "system" {
   fn CoInitializeEx(reserved: *mut c_void, model: u32) -> i32;
   fn CoCreateInstance(
      class: *const Guid,
      outer: *mut c_void,
      context: u32,
      interface: *const Guid,
      out: *mut *mut c_void,
   ) -> i32;
   fn CoTaskMemFree(ptr: *mut c_void);
}

#[link(name = "shell32")]
unsafe extern "system" {
   fn SHGetKnownFolderPath(
      folder: *const Guid,
      flags: u32,
      token: *mut c_void,
      path: *mut *mut u16,
   ) -> i32;
}

/// A COM object, released on drop. Every interface's vtable starts with
/// `IUnknown`, so `release` is always the third slot.
struct Com(*mut c_void);

impl Com {
   fn vtable<T>(&self) -> &T {
      // SAFETY: a COM object starts with a pointer to its vtable.
      unsafe { &**self.0.cast::<*const T>() }
   }
}

impl Drop for Com {
   fn drop(&mut self) {
      // SAFETY: the object is live and this guard holds its reference.
      unsafe { (self.vtable::<ShellLink>().release)(self.0) };
   }
}

fn check(result: i32, call: &str) -> misstep::Result<()> {
   if result < 0 {
      misstep::bail!("{call} failed ({result:#x})");
   }
   Ok(())
}

fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
   text.as_ref().encode_wide().chain([0]).collect()
}

/// A known folder's path, which follows a OneDrive-redirected desktop.
fn known_folder(id: &Guid) -> misstep::Result<PathBuf> {
   let mut raw = ptr::null_mut::<u16>();
   // SAFETY: `raw` receives a CoTaskMem string, freed below.
   check(
      unsafe { SHGetKnownFolderPath(id, 0, ptr::null_mut(), &raw mut raw) },
      "SHGetKnownFolderPath",
   )?;
   // SAFETY: the call succeeded, so `raw` is a NUL-terminated string.
   let path = unsafe {
      let mut len = 0;
      while *raw.add(len) != 0 {
         len += 1;
      }
      OsString::from_wide(slice::from_raw_parts(raw, len))
   };
   // SAFETY: allocated by `SHGetKnownFolderPath`.
   unsafe { CoTaskMemFree(raw.cast()) };
   Ok(path.into())
}

fn create(
   shortcut: &Path,
   target: &Path,
   args: &str,
   description: &str,
   icon: &Path,
) -> misstep::Result<()> {
   let mut raw = ptr::null_mut();
   // SAFETY: COM is initialized on this thread.
   check(
      unsafe {
         CoCreateInstance(
            &SHELL_LINK,
            ptr::null_mut(),
            INPROC_SERVER,
            &ISHELL_LINK_W,
            &raw mut raw,
         )
      },
      "CoCreateInstance",
   )?;
   let link = Com(raw);
   let shell = link.vtable::<ShellLink>();
   let (target, args, description, icon) =
      (wide(target), wide(args), wide(description), wide(icon));
   // SAFETY: a live `IShellLinkW` and NUL-terminated strings.
   unsafe {
      check((shell.set_path)(link.0, target.as_ptr()), "SetPath")?;
      check((shell.set_arguments)(link.0, args.as_ptr()), "SetArguments")?;
      check(
         (shell.set_description)(link.0, description.as_ptr()),
         "SetDescription",
      )?;
      check(
         (shell.set_icon)(link.0, icon.as_ptr(), 0),
         "SetIconLocation",
      )?;
   }
   let mut raw = ptr::null_mut();
   // SAFETY: a live `IShellLinkW`.
   check(
      unsafe { (shell.query_interface)(link.0, &IPERSIST_FILE, &raw mut raw) },
      "QueryInterface",
   )?;
   let file = Com(raw);
   let shortcut = wide(shortcut);
   // SAFETY: a live `IPersistFile` and a NUL-terminated path.
   check(
      unsafe { (file.vtable::<PersistFile>().save)(file.0, shortcut.as_ptr(), 1) },
      "Save",
   )
}

/// The Start Menu and Desktop shortcuts, and a Start Menu one for access gate
/// only, for when the runtime crashes on a Discord build it doesn't know yet.
/// Adds them when missing, or removes them.
pub fn sync(local: &Path, launcher: &Path, remove: bool) -> misstep::Result<()> {
   let programs = known_folder(&FOLDER_PROGRAMS)?;
   let desktop = known_folder(&FOLDER_DESKTOP)?;
   let links = [
      (programs.join("Ruby.lnk"), "", "Discord with Ruby"),
      (desktop.join("Ruby.lnk"), "", "Discord with Ruby"),
      (
         programs.join("Ruby (access gate only).lnk"),
         "--no-runtime",
         "Discord past the access gate, without plugins",
      ),
   ];
   // SAFETY: the launcher's only thread, which never initializes COM another
   // way. It stays initialized until the process exits.
   check(
      unsafe { CoInitializeEx(ptr::null_mut(), APARTMENT_THREADED) },
      "CoInitializeEx",
   )?;
   let discord_icon = local.join(r"DiscordPhoenix\app\Discord.exe");
   // The Ruby logo the launcher writes beside itself, or Discord's own icon without it.
   let logo = launcher.with_file_name("ruby.ico");
   let icon = if logo.is_file() { &logo } else { &discord_icon };
   // shortcut: Start Menu and Desktop only. A pinned taskbar icon keeps its old target until
   // it is pinned again from the Start Menu, and Discord's updater may recreate its shortcut,
   // so opening the launcher again redoes this.
   let phoenix = [
      programs.join("Discord Phoenix.lnk"),
      desktop.join("Discord Phoenix.lnk"),
   ];
   for lnk in phoenix.iter().filter(|lnk| lnk.exists()) {
      if remove {
         create(lnk, &discord_icon, "", "Discord Phoenix", &discord_icon)?;
      } else {
         create(lnk, launcher, "", "Discord Phoenix", icon)?;
      }
   }
   if remove {
      for (lnk, ..) in &links {
         let _ = fs::remove_file(lnk);
      }
      return Ok(());
   }
   // Recreated each time, which also gives the ones from an older install the new icon.
   for (lnk, args, description) in &links {
      create(lnk, launcher, args, description, icon)?;
   }
   Ok(())
}
