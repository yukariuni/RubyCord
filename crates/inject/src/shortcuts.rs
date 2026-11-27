//! The user's Discord Phoenix shortcuts, edited in place through the shell's `IShellLinkW`.
//! A hidden PowerShell child is one of the dropper patterns Defender flags.

use std::{
   env,
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
const FOLDER_STARTUP: Guid = Guid::new(0xB97D_20BB_F46A_4C97_BA10_5E36_0843_0854);
const FOLDER_DESKTOP: Guid = Guid::new(0xB4BF_CC3A_DB2C_424C_B029_7FE9_9A87_C641);
const SHELL_LINK: Guid = Guid::new(0x0002_1401_0000_0000_C000_0000_0000_0046);
const ISHELL_LINK_W: Guid = Guid::new(0x0002_14F9_0000_0000_C000_0000_0000_0046);
const IPERSIST_FILE: Guid = Guid::new(0x0000_010B_0000_0000_C000_0000_0000_0046);

const APARTMENT_THREADED: u32 = 2;
const INPROC_SERVER: u32 = 1;
/// `STGM_READWRITE`, to edit a shortcut where it is.
const READ_WRITE: u32 = 2;

type SetText = unsafe extern "system" fn(*mut c_void, *const u16) -> i32;
type GetText = unsafe extern "system" fn(*mut c_void, *mut u16, i32) -> i32;

/// `IShellLinkW`'s vtable, with the slots it does not call left as padding.
#[repr(C)]
struct ShellLink {
   query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
   _add_ref: usize,
   release: unsafe extern "system" fn(*mut c_void) -> u32,
   get_path: unsafe extern "system" fn(*mut c_void, *mut u16, i32, *mut c_void, u32) -> i32,
   _ids: [usize; 3],
   set_description: SetText,
   _working_dir: [usize; 2],
   get_arguments: GetText,
   set_arguments: SetText,
   _hotkey_show: [usize; 5],
   set_icon: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32,
   _relative: [usize; 2],
   set_path: SetText,
}

/// `IPersistFile`'s vtable up to `Save`.
#[repr(C)]
struct PersistFile {
   _unknown: [usize; 5],
   load: unsafe extern "system" fn(*mut c_void, *const u16, u32) -> i32,
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

/// What a shortcut was doing when [`retarget`] looked at it.
#[derive(PartialEq)]
enum Outcome {
   /// It opens something else, or could not be read.
   Other,
   /// It opened one of the `from` programs and now opens `to`.
   Changed,
   /// It already opens `to`.
   Already,
}

fn read_text(read: impl FnOnce(*mut u16, i32) -> i32) -> String {
   let mut buffer = [0u16; 2048];
   if read(buffer.as_mut_ptr(), buffer.len() as i32) < 0 {
      return String::new();
   }
   let len = buffer
      .iter()
      .position(|&unit| unit == 0)
      .unwrap_or(buffer.len());
   String::from_utf16_lossy(&buffer[..len])
}

fn is(path: &str, program: &Path) -> bool {
   path.eq_ignore_ascii_case(&program.to_string_lossy())
}

/// Edits the shortcut at `lnk` where it is, so its start folder, window mode and the rest stay as
/// they were, when it opens one of `from`: it then opens `to` with `icon`. With `wrap`, the
/// arguments it had (Discord's own, like `--start-minimized`) go after a `--` for the launcher to
/// hand on; without it, that `--` is taken off again.
fn retarget(
   lnk: &Path,
   from: &[&Path],
   to: &Path,
   icon: &Path,
   wrap: bool,
) -> misstep::Result<Outcome> {
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
   let mut raw = ptr::null_mut();
   // SAFETY: a live `IShellLinkW`.
   check(
      unsafe { (shell.query_interface)(link.0, &IPERSIST_FILE, &raw mut raw) },
      "QueryInterface",
   )?;
   let file = Com(raw);
   let path = wide(lnk);
   // SAFETY: a live `IPersistFile` and a NUL-terminated path. A shortcut that does not load is
   // not one to change.
   if unsafe { (file.vtable::<PersistFile>().load)(file.0, path.as_ptr(), READ_WRITE) } < 0 {
      return Ok(Outcome::Other);
   }
   // SAFETY: a live, loaded `IShellLinkW`, and a buffer of the length passed.
   let target =
      read_text(|buffer, len| unsafe { (shell.get_path)(link.0, buffer, len, ptr::null_mut(), 0) });
   if is(&target, to) {
      return Ok(Outcome::Already);
   }
   if !from.iter().any(|program| is(&target, program)) {
      return Ok(Outcome::Other);
   }
   // SAFETY: as above.
   let old = read_text(|buffer, len| unsafe { (shell.get_arguments)(link.0, buffer, len) });
   // The launcher takes Discord's own arguments after a lone `--`.
   let arguments = if wrap {
      // A launcher from an earlier name already carries its `--`.
      if old.is_empty() || old == "--" || old.starts_with("-- ") {
         old
      } else {
         format!("-- {old}")
      }
   } else if old == "--" {
      String::new()
   } else {
      old.strip_prefix("-- ").map_or(old.clone(), str::to_owned)
   };
   let (to_path, arguments, icon) = (wide(to), wide(arguments), wide(icon));
   // SAFETY: a live `IShellLinkW` and NUL-terminated strings.
   unsafe {
      check((shell.set_path)(link.0, to_path.as_ptr()), "SetPath")?;
      check(
         (shell.set_arguments)(link.0, arguments.as_ptr()),
         "SetArguments",
      )?;
      check(
         (shell.set_icon)(link.0, icon.as_ptr(), 0),
         "SetIconLocation",
      )?;
      check(
         (file.vtable::<PersistFile>().save)(file.0, path.as_ptr(), 1),
         "Save",
      )?;
   }
   Ok(Outcome::Changed)
}

/// Every `.lnk` in `dir`, down a few levels (the Start Menu has folders).
fn shortcuts_in(dir: &Path, depth: u32, found: &mut Vec<PathBuf>) {
   for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
      let path = entry.path();
      if path.is_dir() && depth > 0 {
         shortcuts_in(&path, depth - 1, found);
      } else if path
         .extension()
         .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
      {
         found.push(path);
      }
   }
}

/// Points every shortcut the user has to Discord Phoenix (Start Menu, Desktop, pinned to the
/// taskbar or the Start Menu, Startup) at `launcher`, so opening Discord Phoenix the usual way
/// loads RubyCord, or points them back at Discord Phoenix when `remove` is set. Finding them by
/// where they point, not by name, covers the ones Discord Phoenix's own installer made and the
/// ones the user made. No shortcut of RubyCord's own is created, and the ones older versions made
/// are deleted. Returns how many shortcuts open the launcher afterwards.
///
/// shortcut: only the user's own shortcuts, so a Discord Phoenix opened from anywhere else (a
/// `discord://` link, a Windows startup entry in the registry) starts without RubyCord. Discord's
/// updater may recreate its Start Menu shortcut, so Inject again after an update that does.
pub fn sync(local: &Path, launcher: &Path, remove: bool) -> misstep::Result<usize> {
   // SAFETY: the launcher's only thread, which never initializes COM another
   // way. It stays initialized until the process exits.
   check(
      unsafe { CoInitializeEx(ptr::null_mut(), APARTMENT_THREADED) },
      "CoInitializeEx",
   )?;
   let programs = known_folder(&FOLDER_PROGRAMS)?;
   let desktop = known_folder(&FOLDER_DESKTOP)?;
   // The shortcuts of RubyCord's own that earlier versions made, and the ones from when it was
   // called Ruby.
   for old in [
      "RubyCord.lnk",
      "RubyCord (access gate only).lnk",
      "Ruby.lnk",
      "Ruby (access gate only).lnk",
   ] {
      let _ = fs::remove_file(programs.join(old));
      let _ = fs::remove_file(desktop.join(old));
   }

   let discord = local.join("DiscordPhoenix").join("app").join("Discord.exe");
   let old_launcher = local.join("Ruby").join("ruby.exe");
   let logo = launcher.with_file_name("rubycord.ico");
   let (from, to, icon, wrap): (Vec<&Path>, &Path, &Path, bool) = if remove {
      (vec![launcher, &old_launcher], &discord, &discord, false)
   } else {
      let icon = if logo.is_file() { &logo } else { &discord };
      (vec![&discord, &old_launcher], launcher, icon, true)
   };

   let mut folders = vec![programs, desktop];
   if let Ok(startup) = known_folder(&FOLDER_STARTUP) {
      folders.push(startup);
   }
   if let Some(appdata) = env::var_os("APPDATA") {
      let pinned = PathBuf::from(appdata)
         .join("Microsoft")
         .join("Internet Explorer")
         .join("Quick Launch")
         .join("User Pinned");
      folders.extend([pinned.join("TaskBar"), pinned.join("StartMenu")]);
   }
   let mut links = Vec::new();
   for folder in &folders {
      shortcuts_in(folder, 3, &mut links);
   }
   let mut opening_launcher = 0;
   for lnk in &links {
      match retarget(lnk, &from, to, icon, wrap) {
         Ok(Outcome::Changed | Outcome::Already) => opening_launcher += 1,
         Ok(Outcome::Other) | Err(_) => {},
      }
   }
   Ok(if remove { 0 } else { opening_launcher })
}

#[cfg(test)]
mod tests {
   use std::process::Command;

   use super::*;

   fn powershell(script: &str) -> String {
      let output = Command::new("powershell")
         .args(["-NoProfile", "-Command", script])
         .output()
         .expect("powershell runs");
      String::from_utf8_lossy(&output.stdout).trim().to_owned()
   }

   fn read(lnk: &Path, property: &str) -> String {
      powershell(&format!(
         "(New-Object -ComObject WScript.Shell).CreateShortcut('{}').{property}",
         lnk.display()
      ))
   }

   #[test]
   fn retargets_a_shortcut_in_place_and_back() {
      let dir = env::temp_dir().join(format!("rubycord-lnk-{}", std::process::id()));
      fs::create_dir_all(&dir).unwrap();
      let (discord, launcher) = (dir.join("Discord.exe"), dir.join("rubycord.exe"));
      fs::write(&discord, b"").unwrap();
      fs::write(&launcher, b"").unwrap();
      let lnk = dir.join("Phoenix.lnk");
      powershell(&format!(
         "$s = (New-Object -ComObject WScript.Shell).CreateShortcut('{}'); $s.TargetPath = '{}'; \
          $s.Arguments = '--start-minimized'; $s.WorkingDirectory = '{}'; $s.Save()",
         lnk.display(),
         discord.display(),
         dir.display()
      ));
      // SAFETY: this test thread owns its COM apartment.
      assert!(unsafe { CoInitializeEx(ptr::null_mut(), APARTMENT_THREADED) } >= 0);

      let other = dir.join("Other.exe");
      assert!(retarget(&lnk, &[&other], &launcher, &discord, true).unwrap() == Outcome::Other);
      assert_eq!(
         read(&lnk, "TargetPath"),
         discord.display().to_string(),
         "untouched"
      );

      assert!(retarget(&lnk, &[&discord], &launcher, &discord, true).unwrap() == Outcome::Changed);
      assert_eq!(read(&lnk, "TargetPath"), launcher.display().to_string());
      assert_eq!(read(&lnk, "Arguments"), "-- --start-minimized");
      assert_eq!(
         read(&lnk, "WorkingDirectory"),
         dir.display().to_string(),
         "kept as it was"
      );
      assert!(retarget(&lnk, &[&discord], &launcher, &discord, true).unwrap() == Outcome::Already);

      assert!(retarget(&lnk, &[&launcher], &discord, &discord, false).unwrap() == Outcome::Changed);
      assert_eq!(read(&lnk, "TargetPath"), discord.display().to_string());
      assert_eq!(read(&lnk, "Arguments"), "--start-minimized");
      fs::remove_dir_all(&dir).unwrap();
   }
}
