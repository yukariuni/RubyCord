fn main() {
   // GNU ld omits the note unless asked. sigs keys its build table on it, as
   // for Phoenix.
   println!("cargo:rustc-cdylib-link-arg=-Wl,--build-id=sha1");
}
