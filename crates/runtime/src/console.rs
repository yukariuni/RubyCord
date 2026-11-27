//! The last lines phoenix logged, kept for the Console page.

use std::{
   collections::VecDeque,
   sync::{
      Mutex,
      PoisonError,
   },
};

use log::{
   Log,
   Metadata,
   Record,
};

const REDACTED: &str = "[redacted]";

const CAPACITY: usize = 200;

static LINES: Mutex<VecDeque<Line>> = Mutex::new(VecDeque::new());

/// A kept log line: its level, local time as `HH:MM:SS`, and text.
#[derive(Clone)]
pub struct Line {
   pub level: log::Level,
   pub time:  String,
   pub text:  String,
}
static SECRETS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Sets the values every later line is scrubbed of, each also in its
/// URL-encoded form, since plugins put tokens in query strings.
pub fn redact(secrets: impl IntoIterator<Item = String>) {
   let mut all = Vec::new();
   for secret in secrets.into_iter().filter(|secret| !secret.is_empty()) {
      let encoded = url_encoded(&secret);
      if encoded != secret {
         all.push(encoded);
      }
      all.push(secret);
   }
   *SECRETS.lock().unwrap_or_else(PoisonError::into_inner) = all;
}

/// `text` as `encodeURIComponent` writes it.
#[must_use]
pub fn url_encoded(text: &str) -> String {
   text
      .bytes()
      .map(|byte| {
         if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            char::from(byte).to_string()
         } else {
            format!("%{byte:02X}")
         }
      })
      .collect()
}

fn scrub(mut line: String) -> String {
   for secret in SECRETS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .iter()
   {
      if line.contains(secret.as_str()) {
         line = line.replace(secret.as_str(), REDACTED);
      }
   }
   line
}

/// Logs through `L` and keeps a copy of each line.
pub struct Tee<L>(pub L);

impl<L: Log> Log for Tee<L> {
   fn enabled(&self, metadata: &Metadata<'_>) -> bool {
      self.0.enabled(metadata)
   }

   fn log(&self, record: &Record<'_>) {
      if !self.enabled(record.metadata()) {
         return;
      }
      let text = scrub(record.args().to_string());
      self.0.log(
         &Record::builder()
            .args(format_args!("{text}"))
            .metadata(record.metadata().clone())
            .module_path(record.module_path())
            .file(record.file())
            .line(record.line())
            .build(),
      );

      let line = Line {
         level: record.level(),
         time: clock(),
         text,
      };
      {
         let mut lines = LINES.lock().unwrap_or_else(PoisonError::into_inner);
         if lines.len() == CAPACITY {
            lines.pop_front();
         }
         lines.push_back(line);
      }
      if let Some(platform) = crate::platform() {
         platform.logged(record.target());
      }
   }

   fn flush(&self) {
      self.0.flush();
   }
}

/// The local time of day.
#[cfg_attr(not(any(unix, windows)), allow(clippy::missing_const_for_fn))]
fn clock() -> String {
   #[cfg(unix)]
   {
      // SAFETY: `time` accepts null, and `localtime_r` only writes `tm`.
      let tm = unsafe {
         let now = libc::time(std::ptr::null_mut());
         let mut tm = std::mem::zeroed::<libc::tm>();
         libc::localtime_r(&raw const now, &raw mut tm);
         tm
      };
      format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
   }
   #[cfg(windows)]
   {
      #[repr(C)]
      struct SystemTime {
         year:         u16,
         month:        u16,
         day_of_week:  u16,
         day:          u16,
         hour:         u16,
         minute:       u16,
         second:       u16,
         milliseconds: u16,
      }

      #[link(name = "kernel32")]
      unsafe extern "system" {
         fn GetLocalTime(out: *mut SystemTime);
      }

      let mut st = SystemTime {
         year:         0,
         month:        0,
         day_of_week:  0,
         day:          0,
         hour:         0,
         minute:       0,
         second:       0,
         milliseconds: 0,
      };
      // SAFETY: `GetLocalTime` fills the `SYSTEMTIME` struct.
      unsafe { GetLocalTime(&raw mut st) };
      format!("{:02}:{:02}:{:02}", st.hour, st.minute, st.second)
   }
   #[cfg(not(any(unix, windows)))]
   String::new()
}

/// Every kept line, newest first.
pub fn lines() -> Vec<Line> {
   LINES
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .iter()
      .rev()
      .cloned()
      .collect()
}
