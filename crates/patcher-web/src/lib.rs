//! The patcher for the site, run in a worker. Each split is copied into wasm
//! memory once, and the APK comes back as views of that memory wherever it
//! reuses input bytes, so the page must turn them into a `Blob` before calling
//! in again.

use std::{
   borrow::Cow,
   collections::BTreeMap,
   ops::Range,
};

use js_sys::{
   Array,
   Function,
   Uint8Array,
};
use patcher::{
   Error,
   Frida,
   Library,
   PackageName,
   Payload,
   Prepared,
   Runtime,
   ios::Part,
   zip::Source,
};
use wasm_bindgen::{
   JsError,
   JsValue,
   prelude::wasm_bindgen,
};
use web_sys::{
   Blob,
   BlobPropertyBag,
   FileReaderSync,
};

#[wasm_bindgen]
#[derive(Default)]
pub struct Build {
   apks:      Vec<Vec<u8>>,
   libraries: Vec<Library>,
   plugins:   BTreeMap<String, String>,
   scripts:   Vec<(String, String)>,
   frida:     Option<Frida>,
   prepared:  Option<Prepared>,
}

#[wasm_bindgen]
impl Build {
   #[wasm_bindgen(constructor)]
   #[must_use]
   pub fn new() -> Self {
      Self::default()
   }

   /// A split of any kind, told apart by its contents.
   #[wasm_bindgen(js_name = addApk)]
   pub fn add_apk(&mut self, bytes: Vec<u8>) {
      self.apks.push(bytes);
   }

   /// A library for the plugin runtime, the loader under `patcher::LOADER`.
   /// Without any, only the access patches go in.
   #[wasm_bindgen(js_name = addLibrary)]
   pub fn add_library(&mut self, name: String, bytes: Vec<u8>) -> Result<(), JsError> {
      self.libraries.push(Library::new(name, bytes)?);
      Ok(())
   }

   #[wasm_bindgen(js_name = addPlugin)]
   pub fn add_plugin(&mut self, name: String, source: String) {
      self.plugins.insert(name, source);
   }

   #[wasm_bindgen(js_name = addFridaScript)]
   pub fn add_frida_script(&mut self, name: String, code: String) {
      self.scripts.push((name, code));
   }

   /// Embeds frida-gadget, gzipped or not, running `bootstrap` with the
   /// scripts added so far.
   #[wasm_bindgen(js_name = setFrida)]
   pub fn set_frida(&mut self, gadget: Vec<u8>, bootstrap: &str) -> Result<(), JsError> {
      let script = patcher::frida::script(&self.scripts, bootstrap);
      self.frida = Some(Frida::new(gadget, script)?);
      Ok(())
   }

   /// Patches and merges the splits into one signed APK. `log` gets each
   /// step. The returned views stay valid until the next call into wasm.
   #[expect(
      clippy::needless_pass_by_value,
      reason = "wasm-bindgen hands strings over owned"
   )]
   pub fn merge(
      &mut self,
      package: Option<String>,
      label: Option<String>,
      log: &Function,
   ) -> Result<Array, JsError> {
      let package = match package {
         Some(name) => name.parse::<PackageName>()?,
         None => PackageName::default(),
      };
      let runtime = (!self.libraries.is_empty()).then(|| {
         Runtime {
            libraries: std::mem::take(&mut self.libraries),
            plugins:   std::mem::take(&mut self.plugins),
         }
      });
      let payload = Payload {
         runtime,
         frida: self.frida.take(),
      };
      let prepared = self.prepared.insert(payload.prepare()?);
      let mut log = |line: &str| {
         let _ = log.call1(&JsValue::NULL, &JsValue::from_str(line));
      };
      let apk = patcher::merge(
         &mut self.apks,
         prepared,
         &package,
         label.as_deref(),
         &mut log,
      )?;
      Ok(apk
         .into_parts()
         .into_iter()
         .map(|part| {
            match part {
               // SAFETY: the bytes belong to `self`, and the caller copies the
               // view before calling into wasm again, which could grow memory.
               Cow::Borrowed(bytes) => unsafe { Uint8Array::view(bytes) },
               Cow::Owned(bytes) => Uint8Array::from(&bytes[..]),
            }
         })
         .collect())
   }
}

/// A decrypted IPA to patch, with the iOS loader and the plugins to bundle.
#[wasm_bindgen]
#[derive(Default)]
pub struct IpaBuild {
   loader:  Vec<u8>,
   plugins: BTreeMap<String, String>,
}

#[wasm_bindgen]
impl IpaBuild {
   #[wasm_bindgen(constructor)]
   #[must_use]
   pub fn new() -> Self {
      Self::default()
   }

   #[wasm_bindgen(js_name = setLoader)]
   pub fn set_loader(&mut self, bytes: Vec<u8>) {
      self.loader = bytes;
   }

   #[wasm_bindgen(js_name = addPlugin)]
   pub fn add_plugin(&mut self, name: String, source: String) {
      self.plugins.insert(name, source);
   }

   /// Opens the access gate, offers the Phoenix switch and adds the loader and
   /// plugins. Reads `ipa` in pieces with `FileReaderSync`, so it runs only in
   /// a worker, and the returned `Blob` reuses `ipa`'s unchanged slices.
   #[expect(
      clippy::cast_possible_truncation,
      clippy::cast_sign_loss,
      reason = "a Blob's size is a whole number of bytes, and zip32 caps it at 4 GiB"
   )]
   pub fn patch(&self, ipa: &Blob) -> Result<Blob, JsError> {
      if self.loader.is_empty() {
         return Err(JsError::new("no iOS loader set"));
      }
      let size = ipa.size();
      if size > f64::from(u32::MAX) {
         return Err(Error::Overflow.into());
      }
      let source = PageBlob {
         blob:   ipa,
         size:   size as usize,
         reader: FileReaderSync::new().map_err(|err| page_error("FileReaderSync", &err))?,
      };

      let parts = Array::new();
      patcher::ios::build_patched_ipa(&source, &self.loader, &self.plugins, |part| {
         let blob = match part {
            Part::Copy(range) => source.slice(range),
            Part::Bytes(bytes) => {
               // SAFETY: the Blob constructor copies the view before wasm runs
               // again, which could grow memory.
               let view = unsafe { Uint8Array::view(&bytes) };
               Blob::new_with_u8_array_sequence(&Array::of1(&view))
                  .map_err(|err| page_error("building the IPA", &err))
            },
         }?;
         parts.push(&blob);
         Ok(())
      })?;

      let options = BlobPropertyBag::new();
      options.set_type("application/octet-stream");
      Ok(Blob::new_with_blob_sequence_and_options(&parts, &options)
         .map_err(|err| page_error("building the IPA", &err))?)
   }
}

/// The IPA the page picked, read as the patcher needs it.
struct PageBlob<'a> {
   blob:   &'a Blob,
   size:   usize,
   reader: FileReaderSync,
}

impl PageBlob<'_> {
   fn slice(&self, range: Range<usize>) -> Result<Blob, Error> {
      let offset = |at: usize| {
         u32::try_from(at)
            .map(f64::from)
            .map_err(|_| Error::Overflow)
      };
      self
         .blob
         .slice_with_f64_and_f64(offset(range.start)?, offset(range.end)?)
         .map_err(|err| page_error("slicing the IPA", &err))
   }
}

impl Source for PageBlob<'_> {
   fn size(&self) -> usize {
      self.size
   }

   fn read(&self, range: Range<usize>) -> Result<Cow<'_, [u8]>, Error> {
      let buffer = self
         .reader
         .read_as_array_buffer(&self.slice(range)?)
         .map_err(|err| page_error("reading the IPA", &err))?;
      Ok(Cow::Owned(Uint8Array::new(&buffer).to_vec()))
   }
}

fn page_error(what: &str, err: &JsValue) -> Error {
   Error::Input(format!("{what}: {err:?}"))
}
