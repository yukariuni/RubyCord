//! Read-only SQL over Phoenix's cache, `<files>/store/<account>/cache.sqlite`.
//!
//! Every Phoenix shares the `(k TEXT PRIMARY KEY NOT NULL, v BLOB NOT NULL)
//! WITHOUT ROWID` schema, and the platform runs the query.

#[cfg(feature = "sqlite")] use std::time::Duration;
use std::{
   fs,
   path::{
      Path,
      PathBuf,
   },
};

#[cfg(feature = "sqlite")]
use rusqlite::{
   Connection,
   OpenFlags,
   params_from_iter,
   types::ValueRef,
};
use serde::{
   Serialize,
   Serializer,
   ser::{
      SerializeMap,
      SerializeSeq,
   },
};

/// Keeps a careless `select *` from copying a whole message table into JS.
pub const ROW_LIMIT: usize = 10_000;

/// One column of one row.
#[derive(Serialize)]
#[serde(untagged)]
pub enum Cell {
   Null,
   Integer(i64),
   Float(f64),
   Text(String),
   Blob(Vec<u8>),
}

impl Cell {
   /// The text of a string column, or of a BLOB holding UTF-8, which is how
   /// Phoenix stores its JSON values.
   #[must_use]
   pub fn text(&self) -> Option<&str> {
      match self {
         Self::Text(text) => Some(text),
         Self::Blob(bytes) => str::from_utf8(bytes).ok(),
         Self::Null | Self::Integer(_) | Self::Float(_) => None,
      }
   }
}

/// A query result. Serializes as an array of objects keyed by column name.
pub struct Rows {
   pub columns: Vec<String>,
   pub rows:    Vec<Vec<Cell>>,
}

impl Rows {
   /// The `column` of the first row.
   #[must_use]
   pub fn first(&self, column: &str) -> Option<&Cell> {
      let at = self.columns.iter().position(|name| name == column)?;
      self.rows.first()?.get(at)
   }
}

impl Serialize for Rows {
   fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
      struct Row<'a>(&'a [String], &'a [Cell]);

      impl Serialize for Row<'_> {
         fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut map = serializer.serialize_map(Some(self.0.len()))?;
            for (name, cell) in self.0.iter().zip(self.1) {
               map.serialize_entry(name, cell)?;
            }
            map.end()
         }
      }

      let mut seq = serializer.serialize_seq(Some(self.rows.len()))?;
      for row in &self.rows {
         seq.serialize_element(&Row(&self.columns, row))?;
      }
      seq.end()
   }
}

#[derive(Debug, flub::Error)]
pub enum StoreError {
   #[error("no Phoenix store under {dir}")]
   Missing { dir: PathBuf },
   #[error("{message}")]
   Sql { message: String },
   #[error("phoenix has not started")]
   NotStarted,
   #[error(transparent)]
   Backend(Box<dyn std::error::Error + Send + Sync>),
}

/// The cache of the account Phoenix shows.
///
/// Desktop keeps every signed-in account syncing, so the newest write can be
/// any of them: it names the one in front in `device_prefs.json`. Otherwise,
/// the account written to last.
pub fn database(files: &Path) -> Result<PathBuf, StoreError> {
   let dir = files.join("store");
   if let Some(path) = main_account(files)
      .map(|id| dir.join(id).join("cache.sqlite"))
      .filter(|path| path.is_file())
   {
      return Ok(path);
   }
   fs::read_dir(&dir)
      .into_iter()
      .flatten()
      .filter_map(Result::ok)
      .map(|entry| entry.path().join("cache.sqlite"))
      .filter_map(|path| Some((fs::metadata(&path).ok()?.modified().ok()?, path)))
      .max()
      .map(|(_, path)| path)
      .ok_or(StoreError::Missing { dir })
}

/// Desktop's `desktop.main_account`, the account switched to last.
fn main_account(files: &Path) -> Option<String> {
   #[derive(serde::Deserialize)]
   struct Prefs {
      #[serde(rename = "desktop.main_account")]
      main_account: Option<String>,
   }
   let prefs: Prefs =
      serde_json::from_slice(&fs::read(files.join("device_prefs.json")).ok()?).ok()?;
   // An id, never a path.
   prefs
      .main_account
      .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
}

/// The JSON in row `key` of Phoenix's `meta` table, like `me` or `selected`.
pub fn meta(database: &Path, key: &str) -> Result<Option<String>, StoreError> {
   let rows = query(
      database,
      "SELECT v FROM meta WHERE k = ?",
      &[key.to_owned()],
   )?;
   Ok(rows.first("v").and_then(Cell::text).map(str::to_owned))
}

/// Runs `sql` with `args` bound to its `?`s.
pub fn query(database: &Path, sql: &str, args: &[String]) -> Result<Rows, StoreError> {
   crate::platform()
      .ok_or(StoreError::NotStarted)?
      .query(database, sql, args)
}

/// Opens Phoenix's database read-only beside Phoenix's own connection.
///
/// On Linux and iOS, file locks belong to the process, so this second SQLite
/// inside Phoenix's process would take Phoenix's WAL locks as free, reset the
/// shared `-shm` index Phoenix has mapped (a `SIGBUS` in Phoenix's database
/// thread on Linux), and drop Phoenix's locks on close. `immutable` takes no
/// locks and never opens the WAL or `-shm`, at the cost of not seeing writes
/// Phoenix has not checkpointed yet.
#[cfg(feature = "sqlite")]
fn open(database: &Path) -> rusqlite::Result<Connection> {
   let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
   if cfg!(windows) {
      return Connection::open_with_flags(database, flags);
   }
   let path = database
      .to_string_lossy()
      .replace('%', "%25")
      .replace('?', "%3f")
      .replace('#', "%23");
   Connection::open_with_flags(
      format!("file:{path}?immutable=1"),
      flags | OpenFlags::SQLITE_OPEN_URI,
   )
}

/// [`query`] over a bundled SQLite, for platforms that have no SQLite of their
/// own to reach.
#[cfg(feature = "sqlite")]
pub fn sqlite(database: &Path, sql: &str, args: &[String]) -> Result<Rows, StoreError> {
   let conn = open(database)?;
   let _ = conn.busy_timeout(Duration::from_secs(3));
   let mut stmt = conn.prepare(sql)?;
   let columns = stmt.column_names().map(str::to_owned).collect::<Vec<_>>();
   let col_count = columns.len();

   let mut cursor = stmt.query(params_from_iter(args.iter().map(String::as_str)))?;
   let mut rows = Vec::new();
   while rows.len() < ROW_LIMIT
      && let Some(row) = cursor.next()?
   {
      let mut cells = Vec::with_capacity(col_count);
      for index in 0..col_count {
         let cell = match row.get_ref(index)? {
            ValueRef::Null => Cell::Null,
            ValueRef::Integer(value) => Cell::Integer(value),
            ValueRef::Real(value) => Cell::Float(value),
            ValueRef::Text(bytes) => Cell::Text(String::from_utf8_lossy(bytes).into_owned()),
            ValueRef::Blob(bytes) => Cell::Blob(bytes.to_vec()),
         };
         cells.push(cell);
      }
      rows.push(cells);
   }
   Ok(Rows { columns, rows })
}

#[cfg(feature = "sqlite")]
impl From<rusqlite::Error> for StoreError {
   fn from(err: rusqlite::Error) -> Self {
      Self::Sql {
         message: err.to_string(),
      }
   }
}

#[cfg(test)]
mod tests {
   use std::time::{
      Duration,
      SystemTime,
   };

   use super::*;

   #[test]
   fn database_prefers_the_main_account() {
      let files = std::env::temp_dir().join(format!("phoenix-store-{}", std::process::id()));
      let _ = fs::remove_dir_all(&files);
      let cache = |id: &str, age: u64| {
         let dir = files.join("store").join(id);
         fs::create_dir_all(&dir).unwrap();
         let file = fs::File::create(dir.join("cache.sqlite")).unwrap();
         file
            .set_modified(SystemTime::now() - Duration::from_secs(age))
            .unwrap();
         dir.join("cache.sqlite")
      };
      let front = cache("1", 60);
      let newest = cache("2", 0);
      assert_eq!(database(&files).unwrap(), newest);
      let prefs = |main: &str| {
         fs::write(
            files.join("device_prefs.json"),
            format!(r#"{{"desktop.main_account":"{main}","other":1}}"#),
         )
         .unwrap();
      };
      prefs("1");
      assert_eq!(database(&files).unwrap(), front);
      for unusable in ["3", "../2", ""] {
         prefs(unusable);
         assert_eq!(database(&files).unwrap(), newest);
      }
      fs::remove_dir_all(&files).unwrap();
   }
}
