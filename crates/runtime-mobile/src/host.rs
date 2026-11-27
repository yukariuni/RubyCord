//! Numbers of the Phoenix host calls, events and fields phoenix uses. A
//! Discord build can renumber them, so they are read at startup, on Android
//! from the `"<name> <number>"` strings in the installed APK's dex and on iOS
//! from the host's Swift name table.

#[cfg(target_os = "android")]
use std::{
   fs::File,
   os::fd::AsRawFd,
};
use std::{
   sync::{
      Mutex,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicI32,
         Ordering,
      },
      mpsc::{
         self,
         Receiver,
         Sender,
      },
   },
   time::Duration,
};

use phoenix_wire::{
   DispatchEvent,
   HostCall,
   Key,
   Value,
   decode_dispatch,
   edit_frames,
};

use crate::bridge::{
   self,
   Action,
};

/// A host module's method, named `Module.method`.
#[derive(Clone, Copy)]
pub enum Method {
   NetworkingRequest,
   LinkingOpenUrl,
   SystemOverlayToast,
   ClipboardSetString,
   MediaPickerPick,
   MediaPickerRead,
   MediaPickerRelease,
   TimingSet,
}

impl Method {
   const ALL: [Self; 8] = [
      Self::NetworkingRequest,
      Self::LinkingOpenUrl,
      Self::SystemOverlayToast,
      Self::ClipboardSetString,
      Self::MediaPickerPick,
      Self::MediaPickerRead,
      Self::MediaPickerRelease,
      Self::TimingSet,
   ];

   const fn name(self) -> &'static str {
      match self {
         Self::NetworkingRequest => "Networking.request",
         Self::LinkingOpenUrl => "Linking.openURL",
         Self::SystemOverlayToast => "SystemOverlay.toast",
         Self::ClipboardSetString => "Clipboard.setString",
         Self::MediaPickerPick => "MediaPicker.pick",
         Self::MediaPickerRead => "MediaPicker.read",
         Self::MediaPickerRelease => "MediaPicker.release",
         Self::TimingSet => "Timing.set",
      }
   }

   fn module(self) -> &'static str {
      let name = self.name();
      name.split_once('.').map_or(name, |(module, _)| module)
   }
}

/// A host event Phoenix's Java side dispatches to the guest.
#[derive(Clone, Copy)]
pub enum Event {
   Http,
   Picked,
   Read,
   Timer,
   Ws,
}

impl Event {
   const ALL: [Self; 5] = [Self::Http, Self::Picked, Self::Read, Self::Timer, Self::Ws];

   const fn name(self) -> &'static str {
      match self {
         Self::Http => "@http",
         Self::Picked => "@picked",
         Self::Read => "@read",
         Self::Timer => "@timer",
         Self::Ws => "@ws",
      }
   }
}

/// A field of a host call's or event's arguments.
#[derive(Clone, Copy)]
pub enum Field {
   Id,
   Error,
   Method,
   Url,
   Headers,
   Status,
   Text,
   Kind,
   Max,
   Extensions,
   Files,
   Handle,
   Handles,
   Name,
   Denied,
   Delay,
}

impl Field {
   const ALL: [Self; 16] = [
      Self::Id,
      Self::Error,
      Self::Method,
      Self::Url,
      Self::Headers,
      Self::Status,
      Self::Text,
      Self::Kind,
      Self::Max,
      Self::Extensions,
      Self::Files,
      Self::Handle,
      Self::Handles,
      Self::Name,
      Self::Denied,
      Self::Delay,
   ];

   const fn name(self) -> &'static str {
      match self {
         Self::Id => "id",
         Self::Error => "error",
         Self::Method => "method",
         Self::Url => "url",
         Self::Headers => "headers",
         Self::Status => "status",
         Self::Text => "text",
         Self::Kind => "kind",
         Self::Max => "max",
         Self::Extensions => "extensions",
         Self::Files => "files",
         Self::Handle => "handle",
         Self::Handles => "handles",
         Self::Name => "name",
         Self::Denied => "denied",
         Self::Delay => "delay",
      }
   }
}

/// A host module whose calls phoenix matches by module number alone.
#[derive(Clone, Copy)]
pub enum Module {
   Haptics,
}

impl Module {
   const ALL: [Self; 1] = [Self::Haptics];

   const fn name(self) -> &'static str {
      match self {
         Self::Haptics => "Haptics",
      }
   }
}

/// The numbers, laid out as the module and method of each [`Method`], then
/// each [`Event`], then each [`Field`], then each [`Module`], `None` where the
/// build lacks the name.
pub struct Table(Vec<Option<u16>>);

impl Table {
   /// The module and method numbers of `method`.
   pub fn call(&self, method: Method) -> Option<(u16, u16)> {
      let at = 2 * method as usize;
      Some((self.0[at]?, self.0[at + 1]?))
   }

   pub fn event(&self, event: Event) -> Option<u16> {
      self.0[2 * Method::ALL.len() + event as usize]
   }

   pub fn key(&self, field: Field) -> Option<Key> {
      self.0[2 * Method::ALL.len() + Event::ALL.len() + field as usize].map(Key::new)
   }

   pub fn module(&self, module: Module) -> Option<u16> {
      self.0[2 * Method::ALL.len() + Event::ALL.len() + Field::ALL.len() + module as usize]
   }
}

#[derive(Debug, flub::Error)]
pub enum LoadError {
   #[cfg(target_os = "android")]
   #[error("no base.apk is mapped")]
   NoApk,
   #[cfg(target_os = "android")]
   #[error("{path}: {source}")]
   Io {
      path:   String,
      source: std::io::Error,
   },
   #[cfg(target_os = "android")]
   #[error(transparent)]
   Names(patcher::Error),
   #[cfg(target_os = "ios")]
   #[error("phoenix has not started")]
   NotStarted,
}

static TABLE: OnceLock<Table> = OnceLock::new();

/// The numbers, once [`load`] has read them.
pub fn table() -> Option<&'static Table> {
   TABLE.get()
}

/// Reads the numbers from the APK the app runs from. Takes about 100 ms, so
/// it runs off the threads that wait on it.
#[cfg(target_os = "android")]
pub fn load() -> Result<(), LoadError> {
   let path = base_apk()?;
   let map = Map::open(&path).map_err(|source| LoadError::Io { path, source })?;
   let names = names();
   let numbers = patcher::host_names(map.bytes(), &names).map_err(LoadError::Names)?;
   keep(&names, numbers);
   Ok(())
}

/// Reads the numbers from the host's name table in `PhoenixFatClient`.
#[cfg(target_os = "ios")]
pub fn load() -> Result<(), LoadError> {
   let state = phoenix_runtime::state().ok_or(LoadError::NotStarted)?;
   let names = names();
   keep(&names, phoenix_sigs::ios::host_names(&state.image, &names));
   Ok(())
}

/// The names [`Table`] holds, in its order.
fn names() -> Vec<&'static str> {
   let mut names = Vec::new();
   for method in Method::ALL {
      names.extend([method.module(), method.name()]);
   }
   names.extend(Event::ALL.map(Event::name));
   names.extend(Field::ALL.map(Field::name));
   names.extend(Module::ALL.map(Module::name));
   names
}

/// Whether an `invoke` frame starting with `head` targets `module`.
pub fn is_module(head: &[u8], module: Module) -> bool {
   let Some((called, _)) = HostCall::target(head) else {
      return false;
   };
   table().and_then(|host| host.module(module)) == Some(called)
}

fn keep(names: &[&str], numbers: Vec<Option<u16>>) {
   for (name, number) in names.iter().zip(&numbers) {
      if number.is_none() {
         log::warn!("host names: this build has no {name}");
      }
   }
   let _ = TABLE.set(Table(numbers));
}

/// The path of the APK the app runs from, which ART keeps mapped.
#[cfg(target_os = "android")]
pub fn base_apk() -> Result<String, LoadError> {
   std::fs::read_to_string("/proc/self/maps")
      .map_err(|source| {
         LoadError::Io {
            path: "/proc/self/maps".to_owned(),
            source,
         }
      })?
      .lines()
      .filter_map(|line| line.split_whitespace().nth(5))
      .find(|path| path.ends_with("/base.apk"))
      .map(str::to_owned)
      .ok_or(LoadError::NoApk)
}

#[derive(Debug, flub::Error)]
pub enum CallError {
   #[error("Phoenix's host names are not read yet")]
   NotLoaded,
   #[error("this Discord build has no {0}")]
   Missing(&'static str),
   #[error("Phoenix's bridge is not up yet")]
   NoBridge,
   #[error("{0} did not answer")]
   NoAnswer(&'static str),
}

/// Calls `method` with `args` and `body`, as Phoenix's guest would.
pub fn send(method: Method, args: &[(Field, Value)], body: Vec<u8>) -> Result<(), CallError> {
   let host = table().ok_or(CallError::NotLoaded)?;
   let (module, number) = host
      .call(method)
      .ok_or_else(|| CallError::Missing(method.name()))?;
   let args = args
      .iter()
      .map(|(field, value)| {
         let key = host
            .key(*field)
            .ok_or_else(|| CallError::Missing(field.name()))?;
         Ok((key, value.clone()))
      })
      .collect::<Result<Vec<_>, CallError>>()?;
   let call = HostCall {
      module,
      method: number,
      args: Value::Obj(args),
      body,
   };
   if bridge::invoke(&call.encode()) {
      Ok(())
   } else {
      Err(CallError::NoBridge)
   }
}

/// `wanted` of a call's or event's arguments.
pub fn field(args: &Value, wanted: Field) -> Option<&Value> {
   args.field(table()?.key(wanted)?)
}

/// An event's arguments and body.
pub struct Reply {
   pub args: Value,
   pub body: Vec<u8>,
}

/// Far above the ids Phoenix's guest numbers its own calls with.
const FIRST_ID: i32 = 0x6000_0000;

static NEXT_ID: AtomicI32 = AtomicI32::new(FIRST_ID);
/// Who waits for which event with which id.
static WAITING: Mutex<Vec<(u16, i32, Sender<Reply>)>> = Mutex::new(Vec::new());

/// Calls `method` with an `id` field and waits up to `timeout` for the
/// `event` that answers it.
pub fn ask(
   method: Method,
   args: &[(Field, Value)],
   event: Event,
   timeout: Duration,
) -> Result<Reply, CallError> {
   let (id, receiver) = call_with_id(method, args, event)?;
   let reply = receiver.recv_timeout(timeout);
   forget(id);
   reply.map_err(|_| CallError::NoAnswer(method.name()))
}

/// Calls `method` like [`ask`] without waiting. The answering `event` still
/// never reaches the guest.
#[cfg(target_os = "ios")]
pub fn poke(method: Method, args: &[(Field, Value)], event: Event) -> Result<(), CallError> {
   call_with_id(method, args, event).map(|_| ())
}

fn call_with_id(
   method: Method,
   args: &[(Field, Value)],
   event: Event,
) -> Result<(i32, Receiver<Reply>), CallError> {
   let number = table()
      .ok_or(CallError::NotLoaded)?
      .event(event)
      .ok_or_else(|| CallError::Missing(event.name()))?;
   let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
   let (sender, receiver) = mpsc::channel();
   WAITING
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .push((number, id, sender));
   let mut with_id = vec![(Field::Id, Value::I32(id))];
   with_id.extend_from_slice(args);
   if let Err(err) = send(method, &with_id, Vec::new()) {
      forget(id);
      return Err(err);
   }
   Ok((id, receiver))
}

fn forget(id: i32) {
   WAITING
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .retain(|(_, waiting, _)| *waiting != id);
}

/// Takes the events answering [`ask`] out of a dispatch, since
/// Phoenix's guest knows nothing of their ids.
pub fn filter(bytes: &[u8]) -> Action {
   if WAITING
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .is_empty()
   {
      return Action::Pass;
   }
   let Some(id_key) = table().and_then(|host| host.key(Field::Id)) else {
      return Action::Pass;
   };
   let Ok(items) = decode_dispatch(bytes) else {
      return Action::Pass;
   };
   let count = items.len();
   let mut taken = Vec::new();
   for item in items {
      let DispatchEvent::Host { event, args, body } = item.event else {
         continue;
      };
      let Some(id) = args.field(id_key).and_then(Value::as_i32) else {
         continue;
      };
      let mut waiting = WAITING.lock().unwrap_or_else(PoisonError::into_inner);
      let Some(at) = waiting
         .iter()
         .position(|(number, waiting_id, _)| *number == event && *waiting_id == id)
      else {
         continue;
      };
      let (_, _, sender) = waiting.swap_remove(at);
      drop(waiting);
      let _ = sender.send(Reply {
         args,
         body: body.to_vec(),
      });
      taken.push((item.offset, None));
   }
   match taken.len() {
      0 => Action::Pass,
      len if len == count => Action::Drop,
      _ => Action::Replace(edit_frames(bytes, &taken)),
   }
}

/// A read-only mapping of a whole file.
#[cfg(target_os = "android")]
pub struct Map {
   at:  *mut libc::c_void,
   len: usize,
}

#[cfg(target_os = "android")]
impl Map {
   pub fn open(path: &str) -> std::io::Result<Self> {
      let file = File::open(path)?;
      let len = usize::try_from(file.metadata()?.len()).map_err(std::io::Error::other)?;
      // SAFETY: a fresh private read-only mapping of an open file, released in
      // `Drop`.
      let at = unsafe {
         libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
         )
      };
      if at == libc::MAP_FAILED {
         return Err(std::io::Error::last_os_error());
      }
      Ok(Self { at, len })
   }

   pub const fn bytes(&self) -> &[u8] {
      // SAFETY: `at` maps `len` readable bytes until `self` drops. The APK is
      // the installed app's, which is not rewritten in place.
      unsafe { std::slice::from_raw_parts(self.at.cast(), self.len) }
   }
}

#[cfg(target_os = "android")]
impl Drop for Map {
   fn drop(&mut self) {
      // SAFETY: `at` and `len` came from the `mmap` in `new`.
      unsafe { libc::munmap(self.at, self.len) };
   }
}
