//! `ruby.exe`, the Windows launcher, also `phoenix-inject [phoenix.dll] [--
//! discord args]`.
//!
//! `Discord.exe` resolves its imports from System32 only (`DependentLoadFlags`
//! 0x800), so a DLL planted beside it is never loaded. This starts it
//! suspended and queues `LoadLibraryW(phoenix.dll)` as an APC on its main
//! thread, which runs once the OS loader finishes and before Discord's own
//! code. Quit Discord first: a second instance hands off to the running one
//! and exits.
//!
//! Without a DLL argument it installs itself to `%LOCALAPPDATA%\Ruby` with
//! Start Menu and Desktop shortcuts, and fetches the site's newest
//! `phoenix.dll` there before each launch. Discord's own files stay untouched,
//! so its updates never undo Ruby. `--uninstall` removes both.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod http;
#[cfg(windows)]
mod shortcuts;

#[cfg(windows)]
fn main() {
   misstep::install_panic_hook();
   if let Err(err) = windows::run() {
      windows::alert(&format!("{err:?}"));
      std::process::exit(1);
   }
}

#[cfg(not(windows))]
fn main() {
   eprintln!("phoenix-inject only runs on Windows");
}

#[cfg(windows)]
mod windows {
   use std::{
      env,
      ffi::{OsStr, OsString, c_void},
      fs, io,
      os::windows::ffi::{OsStrExt, OsStringExt},
      path::{Path, PathBuf},
      process::Command,
      ptr, thread,
      time::Duration,
   };

   use misstep::{OptionExt, ResultExt};

   type Handle = *mut c_void;

   #[repr(C)]
   struct ProcessInformation {
      process: Handle,
      thread: Handle,
      process_id: u32,
      thread_id: u32,
   }

   const CREATE_SUSPENDED: u32 = 0x4;
   const MEM_COMMIT_RESERVE: u32 = 0x3000;
   const PAGE_READWRITE: u32 = 0x4;

   #[link(name = "kernel32")]
   unsafe extern "system" {
      fn CreateProcessW(
         application: *const u16,
         command_line: *mut u16,
         process_attributes: *const c_void,
         thread_attributes: *const c_void,
         inherit_handles: i32,
         flags: u32,
         environment: *const c_void,
         directory: *const u16,
         startup: *const c_void,
         info: *mut ProcessInformation,
      ) -> i32;
      fn VirtualAllocEx(
         process: Handle,
         address: *const c_void,
         size: usize,
         kind: u32,
         protect: u32,
      ) -> *mut c_void;
      fn WriteProcessMemory(
         process: Handle,
         address: *mut c_void,
         buffer: *const c_void,
         size: usize,
         written: *mut usize,
      ) -> i32;
      fn GetModuleHandleW(name: *const u16) -> Handle;
      fn GetProcAddress(module: Handle, name: *const u8) -> *mut c_void;
      fn QueueUserAPC(function: *mut c_void, thread: Handle, data: usize) -> u32;
      fn ResumeThread(thread: Handle) -> u32;
      fn TerminateProcess(process: Handle, code: u32) -> i32;
      fn CloseHandle(handle: Handle) -> i32;
      fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> Handle;
      fn Process32FirstW(snapshot: Handle, entry: *mut c_void) -> i32;
      fn Process32NextW(snapshot: Handle, entry: *mut c_void) -> i32;
      fn Module32FirstW(snapshot: Handle, entry: *mut c_void) -> i32;
      fn Module32NextW(snapshot: Handle, entry: *mut c_void) -> i32;
      fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> Handle;
      fn QueryFullProcessImageNameW(
         process: Handle,
         flags: u32,
         name: *mut u16,
         size: *mut u32,
      ) -> i32;
      fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
   }

   #[link(name = "user32")]
   unsafe extern "system" {
      fn MessageBoxW(window: Handle, text: *const u16, caption: *const u16, kind: u32) -> i32;
   }

   /// Where a server hosts `version.json` and the loaders, if `RUBY_SITE` names one. Ruby hosts
   /// none, so without it the launcher runs the `phoenix.dll` already installed. Anything but a
   /// localhost server must be https, since what it serves is loaded into Discord.
   fn site() -> misstep::Result<Option<String>> {
      let Ok(site) = env::var("RUBY_SITE") else {
         return Ok(None);
      };
      if !site_allowed(&site) {
         misstep::bail!("RUBY_SITE must be https:// or a localhost server, not {site}");
      }
      Ok(Some(site))
   }

   fn site_allowed(site: &str) -> bool {
      site.starts_with("https://")
         || ["http://127.0.0.1", "http://localhost"]
            .iter()
            .any(|local| {
               site
                  .strip_prefix(local)
                  .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/']))
            })
   }

   /// Shows `text` in a message box, since the launcher has no console.
   pub fn alert(text: &str) {
      // SAFETY: both strings are NUL-terminated and outlive the call.
      unsafe {
         MessageBoxW(
            ptr::null_mut(),
            wide(text).as_ptr(),
            wide("Ruby").as_ptr(),
            0x10,
         )
      };
   }

   /// Asks a yes or no question, true for yes.
   fn ask(text: &str) -> bool {
      // `MB_YESNO | MB_ICONQUESTION`, and `IDYES`.
      // SAFETY: both strings are NUL-terminated and outlive the call.
      unsafe {
         MessageBoxW(
            ptr::null_mut(),
            wide(text).as_ptr(),
            wide("Ruby").as_ptr(),
            0x24,
         ) == 6
      }
   }

   const SNAPSHOT_PROCESSES: u32 = 0x2;
   const SNAPSHOT_MODULES: u32 = 0x8 | 0x10;
   /// `TERMINATE | QUERY_LIMITED_INFORMATION | SYNCHRONIZE`.
   const PROCESS_ACCESS: u32 = 0x0010_1001;

   /// The UTF-16 text in `entry` from byte `at`, up to its NUL.
   fn entry_text(entry: &[u8], at: usize) -> OsString {
      let units: Vec<u16> = entry[at..]
         .as_chunks::<2>()
         .0
         .iter()
         .map(|pair| u16::from_le_bytes(*pair))
         .take_while(|&unit| unit != 0)
         .collect();
      OsString::from_wide(&units)
   }

   /// Every running process of `exe`, opened to end it: Discord's main
   /// process and its helpers.
   fn running(exe: &Path) -> Vec<(Handle, u32)> {
      let name = exe.file_name().unwrap_or_default();
      let want = exe.as_os_str().to_string_lossy().to_lowercase();
      // SAFETY: a process snapshot of the whole system.
      let snapshot = unsafe { CreateToolhelp32Snapshot(SNAPSHOT_PROCESSES, 0) };
      if snapshot as isize == -1 {
         return Vec::new();
      }
      // `PROCESSENTRY32W`: its size first, the pid at 8, the file name at 44.
      let mut entry = [0u64; 71];
      entry[0] = 568;
      let mut found = Vec::new();
      // SAFETY: `entry` is a 568-byte `PROCESSENTRY32W` with its size set.
      let mut more = unsafe { Process32FirstW(snapshot, entry.as_mut_ptr().cast()) } != 0;
      while more {
         let bytes: &[u8] = as_bytes(&entry);
         let pid = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
         if entry_text(bytes, 44).eq_ignore_ascii_case(name) {
            // SAFETY: opens by pid, checked for null below.
            let process = unsafe { OpenProcess(PROCESS_ACCESS, 0, pid) };
            if !process.is_null() {
               let mut path = [0u16; 1024];
               let mut len = u32::try_from(path.len()).unwrap_or(1024);
               // SAFETY: `path` holds `len` units.
               let named = unsafe {
                  QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &raw mut len)
               } != 0;
               let path = String::from_utf16_lossy(&path[..len as usize]).to_lowercase();
               if named && path == want {
                  found.push((process, pid));
               } else {
                  // SAFETY: opened above, closed once.
                  unsafe { CloseHandle(process) };
               }
            }
         }
         // SAFETY: as above.
         more = unsafe { Process32NextW(snapshot, entry.as_mut_ptr().cast()) } != 0;
      }
      // SAFETY: the snapshot is closed once.
      unsafe { CloseHandle(snapshot) };
      found
   }

   /// Whether process `pid` has a module named `dll` loaded.
   fn has_module(pid: u32, dll: &OsStr) -> bool {
      // SAFETY: a module snapshot of one process.
      let snapshot = unsafe { CreateToolhelp32Snapshot(SNAPSHOT_MODULES, pid) };
      if snapshot as isize == -1 {
         return false;
      }
      // `MODULEENTRY32W`: its size first, the module name at 48.
      let mut entry = [0u64; 135];
      entry[0] = 1080;
      let mut found = false;
      // SAFETY: `entry` is a 1080-byte `MODULEENTRY32W` with its size set.
      let mut more = unsafe { Module32FirstW(snapshot, entry.as_mut_ptr().cast()) } != 0;
      while more && !found {
         found = entry_text(as_bytes(&entry), 48).eq_ignore_ascii_case(dll);
         // SAFETY: as above.
         more = unsafe { Module32NextW(snapshot, entry.as_mut_ptr().cast()) } != 0;
      }
      // SAFETY: the snapshot is closed once.
      unsafe { CloseHandle(snapshot) };
      found
   }

   const fn as_bytes(words: &[u64]) -> &[u8] {
      // SAFETY: any `u64`s are valid bytes, and the length covers them.
      unsafe { std::slice::from_raw_parts(words.as_ptr().cast(), size_of_val(words)) }
   }

   fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
      text.as_ref().encode_wide().chain([0]).collect()
   }

   fn append_quoted(command: &mut Vec<u16>, arg: &OsStr) {
      const QUOTE: u16 = b'"' as u16;
      const SLASH: u16 = b'\\' as u16;
      command.push(QUOTE);
      let mut slashes = 0;
      for unit in arg.encode_wide() {
         if unit == SLASH {
            slashes += 1;
            continue;
         }
         // Windows treats backslashes before a quote as escapes.
         let count = if unit == QUOTE {
            slashes * 2 + 1
         } else {
            slashes
         };
         command.extend(std::iter::repeat_n(SLASH, count));
         command.push(unit);
         slashes = 0;
      }
      command.extend(std::iter::repeat_n(SLASH, slashes * 2));
      command.push(QUOTE);
   }

   fn check(ok: bool) -> io::Result<()> {
      if ok {
         Ok(())
      } else {
         Err(io::Error::last_os_error())
      }
   }

   pub fn run() -> misstep::Result<()> {
      let local = PathBuf::from(env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unset")?);
      let home = local.join("Ruby");
      let mut args = env::args_os().skip(1).peekable();
      if args.peek().is_some_and(|arg| arg == "--uninstall") {
         uninstall(&local, &home);
         return Ok(());
      }
      let mut no_runtime = false;
      while args
         .next_if(|arg| arg == "--no-runtime" || arg == "--gate-only")
         .is_some()
      {
         no_runtime = true;
      }
      let app = local.join("DiscordPhoenix").join("app");
      let exe = app.join("Discord.exe");
      if !exe.is_file() {
         misstep::bail!(
            "Discord Phoenix is not installed at {}. Install it first, then open Ruby again.",
            exe.display()
         );
      }
      let explicit = args.next_if(|arg| {
         Path::new(arg)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"))
      });
      while args
         .next_if(|arg| arg == "--no-runtime" || arg == "--gate-only")
         .is_some()
      {
         no_runtime = true;
      }
      let launcher = if explicit.is_some() {
         env::current_exe()?
      } else {
         home.join("ruby.exe")
      };
      let dll = match &explicit {
         Some(path) => PathBuf::from(path),
         None => install(&local, &home)?,
      };
      let dll = dll.canonicalize()?;
      args.next_if(|arg| arg == "--");
      let args: Vec<OsString> = args.collect();

      // A second Discord hands off to the running one and exits, so Ruby
      // would never run in the Discord the user sees.
      let open = running(&exe);
      if !open.is_empty() {
         let dll_name = dll.file_name().unwrap_or_default();
         let with_ruby = open.iter().any(|&(_, pid)| has_module(pid, dll_name));
         let restart = if no_runtime {
            ask(
               "Discord is already running.\n\nRestart it without the Ruby plugin runtime? A \
                call you are in will end.",
            )
         } else {
            !with_ruby
               && ask(
                  "Discord is already running without Ruby.\n\nRestart it with Ruby? A call \
                   you are in will end.",
               )
         };
         for &(process, _) in &open {
            if restart {
               // SAFETY: opened with TERMINATE by `running`.
               unsafe { TerminateProcess(process, 0) };
            }
         }
         for (process, _) in open {
            if restart {
               // SAFETY: opened with SYNCHRONIZE by `running`.
               unsafe { WaitForSingleObject(process, 5000) };
            }
            // SAFETY: opened by `running`, closed once.
            unsafe { CloseHandle(process) };
         }
         if !restart {
            // Brings the running Discord forward, or leaves it as the user chose.
            if with_ruby {
               Command::new(&exe).args(&args).current_dir(&app).spawn()?;
            }
            return Ok(());
         }
      }

      set_restart_env(
         &launcher,
         explicit.as_ref().map(|_| dll.as_path()),
         no_runtime,
      );
      launch(&exe, &app, &dll, &args)
   }

   fn set_restart_env(launcher: &Path, explicit_dll: Option<&Path>, no_runtime: bool) {
      let mut restart_args = Vec::new();
      if no_runtime {
         restart_args.extend("--no-runtime ".encode_utf16());
      }
      if let Some(dll) = explicit_dll {
         append_quoted(&mut restart_args, dll.as_os_str());
         restart_args.push(u16::from(b' '));
      }
      restart_args.extend("--".encode_utf16());
      // SAFETY: the launcher is single-threaded. Discord and its restart helper
      // inherit these settings with the rest of the environment.
      unsafe {
         env::set_var("RUBY_RESTART_EXE", launcher);
         env::set_var("RUBY_RESTART_ARGS", OsString::from_wide(&restart_args));
         if no_runtime {
            env::set_var("RUBY_GATE_ONLY", "1");
         }
      }
   }

   fn launch(exe: &Path, app: &Path, dll: &Path, args: &[OsString]) -> misstep::Result<()> {
      let mut command = Vec::new();
      append_quoted(&mut command, exe.as_os_str());
      for arg in args {
         command.push(u16::from(b' '));
         append_quoted(&mut command, arg);
      }
      command.push(0);

      // STARTUPINFOW is 104 bytes on x64 and only `cb` must be set.
      let mut startup = [0u64; 13];
      startup[0] = 104;
      let mut info = ProcessInformation {
         process: ptr::null_mut(),
         thread: ptr::null_mut(),
         process_id: 0,
         thread_id: 0,
      };
      // SAFETY: every pointer is valid for the call and the strings are
      // NUL-terminated.
      check(unsafe {
         CreateProcessW(
            wide(exe).as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            CREATE_SUSPENDED,
            ptr::null(),
            wide(app).as_ptr(),
            startup.as_ptr().cast(),
            &raw mut info,
         ) != 0
      })?;

      let queued = queue_load(&info, &wide(dll));
      if queued.is_err() {
         // SAFETY: `info.process` is the suspended child created above.
         unsafe { TerminateProcess(info.process, 1) };
      } else {
         // SAFETY: `info.thread` is the child's suspended main thread.
         unsafe { ResumeThread(info.thread) };
      }
      // SAFETY: both handles came from CreateProcessW and are closed once.
      unsafe {
         CloseHandle(info.thread);
         CloseHandle(info.process);
      }
      queued?;
      println!(
         "started Discord (pid {}) with {}",
         info.process_id,
         dll.display()
      );
      Ok(())
   }

   /// Copies `dll` into the child and queues `LoadLibraryW` on its main
   /// thread. kernel32 sits at the same address in every process of a boot.
   fn queue_load(info: &ProcessInformation, dll: &[u16]) -> io::Result<()> {
      let size = size_of_val(dll);
      // SAFETY: `info.process` is a live process handle with full access.
      let remote = unsafe {
         VirtualAllocEx(
            info.process,
            ptr::null(),
            size,
            MEM_COMMIT_RESERVE,
            PAGE_READWRITE,
         )
      };
      check(!remote.is_null())?;
      // SAFETY: `remote` holds `size` bytes and `dll` is `size` bytes long.
      check(unsafe {
         WriteProcessMemory(
            info.process,
            remote,
            dll.as_ptr().cast(),
            size,
            ptr::null_mut(),
         ) != 0
      })?;
      // SAFETY: kernel32 is always loaded and the names are NUL-terminated.
      let load = unsafe {
         GetProcAddress(
            GetModuleHandleW(wide("kernel32.dll").as_ptr()),
            c"LoadLibraryW".as_ptr().cast(),
         )
      };
      check(!load.is_null())?;
      // SAFETY: LoadLibraryW takes one pointer argument, as an APC does.
      check(unsafe { QueueUserAPC(load, info.thread, remote as usize) } != 0)
   }

   /// Copies this launcher into `home`, adds its shortcuts once, and brings
   /// `phoenix.dll` up to the site's version. A failed update keeps the DLL
   /// already there, so Ruby still starts offline.
   fn install(local: &Path, home: &Path) -> misstep::Result<PathBuf> {
      fs::create_dir_all(home)?;
      let launcher = home.join("ruby.exe");
      let me = env::current_exe()?;
      let copied = !same_file(&me, &launcher);
      if copied {
         fs::copy(&me, &launcher).context("copying the launcher")?;
      }
      let dll = home.join("phoenix.dll");
      // Without a server the loader ships beside the launcher.
      adopt(&me, &dll).context("installing the phoenix.dll beside the launcher")?;
      if let Err(err) = update(home, &dll)
         && !dll.is_file()
      {
         return Err(err.context("downloading phoenix.dll"));
      }
      if !dll.is_file() {
         misstep::bail!(
            "no phoenix.dll in {}: pass one as an argument or set RUBY_SITE",
            home.display()
         );
      }
      // Missing shortcuts never stop Discord from starting, and a shortcut the
      // user deleted stays deleted until they run the installer again.
      if copied {
         let _ = crate::shortcuts::sync(local, &launcher, false);
      }
      Ok(dll)
   }

   /// The loader built into this launcher by CI, empty in a plain build.
   static EMBEDDED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/phoenix.dll"));

   /// Puts `write`'s file at `dll`. A running Discord keeps the old one mapped, which Windows
   /// lets us rename but not overwrite.
   fn swap(dll: &Path, write: impl FnOnce(&Path) -> io::Result<()>) -> io::Result<()> {
      let old = dll.with_extension("dll.old");
      let _ = fs::remove_file(&old);
      if dll.is_file() {
         fs::rename(dll, &old)?;
      }
      write(dll)?;
      let _ = fs::remove_file(old);
      Ok(())
   }

   /// Installs the loader built into this launcher, or else the `phoenix.dll` beside it when
   /// that one is newer.
   fn adopt(launcher: &Path, dll: &Path) -> io::Result<()> {
      if !EMBEDDED.is_empty() {
         if fs::read(dll).is_ok_and(|have| have == EMBEDDED) {
            return Ok(());
         }
         return swap(dll, |path| fs::write(path, EMBEDDED));
      }
      let Some(beside) = launcher.parent().map(|dir| dir.join("phoenix.dll")) else {
         return Ok(());
      };
      if !beside.is_file() || same_file(&beside, dll) {
         return Ok(());
      }
      let modified = |path: &Path| fs::metadata(path).and_then(|meta| meta.modified());
      if dll.is_file() && modified(&beside)? <= modified(dll)? {
         return Ok(());
      }
      swap(dll, |path| fs::copy(&beside, path).map(drop))
   }

   fn same_file(a: &Path, b: &Path) -> bool {
      a.canonicalize()
         .ok()
         .zip(b.canonicalize().ok())
         .is_some_and(|(a, b)| a == b)
   }

   /// Downloads `phoenix.dll` when the site's loader version differs from the
   /// one recorded beside it. A running Discord keeps the old DLL mapped,
   /// which Windows lets us rename but not overwrite.
   fn update(home: &Path, dll: &Path) -> misstep::Result<()> {
      let Some(site) = site()? else {
         return Ok(());
      };
      let manifest = crate::http::text(&format!("{site}/version.json"), Duration::from_secs(10))?;
      let version = loader_version(&manifest).context("version.json has no loader")?;
      let stamp = home.join("version.txt");
      if dll.is_file() && fs::read_to_string(&stamp).is_ok_and(|have| have.trim() == version) {
         return Ok(());
      }
      // Per process, so a second launch mid-download writes its own file.
      let fresh = home.join(format!("phoenix.dll.{}.new", std::process::id()));
      // Discord waits on this download. With a DLL to fall back on, a slow one
      // gives up sooner; a first install has nothing else, so it keeps trying.
      let attempts = if dll.is_file() { 1 } else { 3 };
      let url = format!("{site}/plugins/phoenix-windows-x86_64.dll");
      let mut downloaded = crate::http::download(&url, &fresh, Duration::from_secs(60));
      for _ in 1..attempts {
         if downloaded.is_ok() {
            break;
         }
         thread::sleep(Duration::from_secs(2));
         downloaded = crate::http::download(&url, &fresh, Duration::from_secs(60));
      }
      if let Err(err) = downloaded {
         let _ = fs::remove_file(&fresh);
         return Err(err);
      }
      // A captive portal or a wrong deploy still writes its body here;
      // swapping that in would leave Discord without a working DLL.
      if !is_loader(&fresh) {
         let _ = fs::remove_file(&fresh);
         misstep::bail!("the downloaded phoenix.dll is not a Windows loader");
      }
      let old = home.join("phoenix.dll.old");
      let _ = fs::remove_file(&old);
      let replaced = if dll.is_file() {
         fs::rename(dll, &old)?;
         true
      } else {
         false
      };
      if let Err(install) = fs::rename(&fresh, dll) {
         // A failed swap must not leave Ruby without a DLL: the one moved
         // aside goes back into place, and this launch carries on with it.
         let _ = fs::remove_file(&fresh);
         if replaced && fs::rename(&old, dll).is_ok() {
            return Ok(());
         }
         misstep::bail!("installing phoenix.dll failed: {install}");
      }
      let _ = fs::remove_file(&old);
      fs::write(stamp, version)?;
      Ok(())
   }

   /// Whether `path` is a Windows PE image carrying the version marker every
   /// loader is built with (`crates/runtime/src/update.rs`).
   fn is_loader(path: &Path) -> bool {
      fs::read(path).is_ok_and(|bytes| {
         bytes.starts_with(b"MZ")
            && bytes
               .windows(16)
               .any(|window| window == b"phoenix-version:")
      })
   }

   /// `"loader": "<version>"` from `version.json`.
   fn loader_version(manifest: &str) -> Option<&str> {
      let rest = &manifest[manifest.find("\"loader\"")? + 8..];
      let rest = &rest[rest.find('"')? + 1..];
      rest.get(..rest.find('"')?)
   }

   /// Removes the shortcuts and `home`. Files a running Discord or launcher
   /// still holds stay until they exit, and the user is told so.
   fn uninstall(local: &Path, home: &Path) {
      let _ = crate::shortcuts::sync(local, &home.join("ruby.exe"), true);
      match fs::remove_dir_all(home) {
         Err(err) if err.kind() != io::ErrorKind::NotFound => {
            alert(&format!(
               "The shortcuts are removed, but {} could not be deleted ({err}). Quit Discord and \
                delete it.",
               home.display()
            ));
         },
         _ => alert("Ruby is uninstalled. Discord itself was not changed."),
      }
   }

   #[cfg(test)]
   mod tests {
      #[test]
      fn quotes_windows_arguments_for_restart() {
         for (arg, quoted) in [
            ("", r#""""#),
            ("two words", r#""two words""#),
            (r"C:\a b\", r#""C:\a b\\""#),
            (r#"a\"b"#, r#""a\\\"b""#),
         ] {
            let mut command = Vec::new();
            super::append_quoted(&mut command, arg.as_ref());
            assert_eq!(String::from_utf16(&command).unwrap(), quoted);
         }
      }

      #[test]
      fn reads_the_loader_version() {
         let manifest = "{\n  \"loader\": \"2026.11.23.2353-b51d77c\",\n  \"plugins\": {}\n}";
         assert_eq!(
            super::loader_version(manifest),
            Some("2026.11.23.2353-b51d77c")
         );
         assert_eq!(super::loader_version("{}"), None);
      }

      #[test]
      fn allows_https_or_localhost_sites() {
         for ok in [
            "https://assets.ruby.tools",
            "http://127.0.0.1:8765",
            "http://localhost",
            "http://localhost/x",
         ] {
            assert!(super::site_allowed(ok), "{ok}");
         }
         for bad in [
            "http://assets.ruby.tools",
            "http://localhost.evil.com",
            "http://127.0.0.1.evil.com",
            "ftp://x",
         ] {
            assert!(!super::site_allowed(bad), "{bad}");
         }
      }
   }
}
