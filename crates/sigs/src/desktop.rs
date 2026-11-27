//! Where Desktop Phoenix (`Discord.exe` on Windows, `discord-phoenix` on
//! Linux) keeps what the runtime patches.
//!
//! Found from strings, call graphs and instruction shapes, never fixed
//! addresses. The runtime installs from these, and `sigscan check` runs them on
//! a new build before anyone ships against it.
//!
//! Both builds compile the same Rust, so every locator runs on both. What
//! differs is isolated here: argument registers and prologues come from the
//! image's [`Abi`], [`call_target`] follows the GOT calls ELF builds make, and
//! [`literal`] finds strings ELF packs without a trailing NUL.

use std::{
   collections::{
      HashMap,
      HashSet,
   },
   sync::{
      Arc,
      Mutex,
      PoisonError,
   },
};

use phoenix_hook::{
   image::Image,
   pattern::Pattern,
   x86::{
      Abi,
      Register,
      call_target,
      jump_target,
   },
};

use crate::{
   Found,
   StaffField,
   one,
};

/// Win64 `MessageRow::render` and `build_message_body` prologue: 8 pushes and
/// `mov eax, imm32`, before `call __chkstk`.
const ROW_PROLOGUE: Pattern = Pattern::new("55 41 57 41 56 41 55 41 54 56 57 53 B8 ?? ?? ?? ??");
const ROW_PROLOGUE_LEN: usize = 17;
/// Win64 `host::section`, `WindowRoot::render`, and `Store::delete_message`
/// prologue: 8 pushes and `sub rsp, imm32`.
const FRAME_PROLOGUE: Pattern =
   Pattern::new("55 41 57 41 56 41 55 41 54 56 57 53 48 81 EC ?? ?? ?? ??");
const FRAME_PROLOGUE_LEN: usize = 19;
/// Win64 `libdiscord_media::fetch` prologue: 6 pushes and `sub rsp, imm32`.
const MEDIA_FETCH_PROLOGUE: Pattern = Pattern::new("55 41 57 41 56 56 57 53 48 81 EC ?? ?? ?? ??");
const MEDIA_FETCH_PROLOGUE_LEN: usize = 15;

/// GPUI's Windows restart script.
pub const WINDOWS_RESTART_SCRIPT: &str = r#"
            $pidToWaitFor = $env:ZED_RESTART_PID
            $exePath = $env:ZED_RESTART_EXECUTABLE
            $argumentList = $env:ZED_RESTART_ARGUMENTS

            [Environment]::SetEnvironmentVariable("ZED_RESTART_PID", $null)
            [Environment]::SetEnvironmentVariable("ZED_RESTART_EXECUTABLE", $null)
            [Environment]::SetEnvironmentVariable("ZED_RESTART_ARGUMENTS", $null)

            while ($true) {
                $process = Get-Process -Id $pidToWaitFor -ErrorAction SilentlyContinue
                if (-not $process) {
                    if ([string]::IsNullOrEmpty($argumentList)) {
                        Start-Process -FilePath $exePath
                    } else {
                        Start-Process -FilePath $exePath -ArgumentList $argumentList
                    }
                    break
                }
                Start-Sleep -Seconds 0.1
            }
            "#;

pub fn windows_restart_script(image: &Image<'_>) -> Found<usize> {
   one(
      "Windows restart script",
      &image.find_bytes(WINDOWS_RESTART_SCRIPT.as_bytes()),
   )
}

/// A function entry [`Image::hook_entry`] can displace: whole instructions
/// with no RIP-relative operand, at least 14 bytes.
#[derive(Clone, Copy)]
pub struct Prologue {
   pub pattern: Pattern,
   pub len:     usize,
}

impl Prologue {
   #[must_use]
   pub fn at(self, image: &Image<'_>, func: usize) -> bool {
      image
         .read(func, self.len)
         .is_some_and(|head| self.pattern.find_in(head).first() == Some(&0))
   }
}

/// The prologues of the functions the runtime hooks.
#[derive(Clone, Copy)]
pub struct Prologues {
   /// `MessageRow::render`, `build_message_body` and `MessageContextMenu`.
   pub row:         Prologue,
   /// `host::section`, `Workspace::render_overlays`, `Store::delete_message`
   /// and `ReqwestTransport::send`.
   pub frame:       Prologue,
   pub media_fetch: Prologue,
   /// The asset loader, [`asset_load`].
   pub asset_load:  Prologue,
   /// `getEmojiUnavailableReason`, a leaf: its first four instructions on
   /// Win64, five on System V, up to the `jne`.
   pub emoji_check: Prologue,
}

impl Prologues {
   /// Win64 saves `rsi` and `rdi` too, and probes frames over a page with
   /// `__chkstk`. System V probes them inline after the same `sub rsp`.
   #[must_use]
   pub const fn of(abi: Abi) -> Self {
      const SYSV_FRAME: Prologue = Prologue {
         pattern: Pattern::new("55 41 57 41 56 41 55 41 54 53 48 81 EC ?? ?? ?? ??"),
         len:     17,
      };
      match abi {
         Abi::Win64 => {
            Self {
               row:         Prologue {
                  pattern: ROW_PROLOGUE,
                  len:     ROW_PROLOGUE_LEN,
               },
               frame:       Prologue {
                  pattern: FRAME_PROLOGUE,
                  len:     FRAME_PROLOGUE_LEN,
               },
               media_fetch: Prologue {
                  pattern: MEDIA_FETCH_PROLOGUE,
                  len:     MEDIA_FETCH_PROLOGUE_LEN,
               },
               asset_load:  Prologue {
                  pattern: MEDIA_FETCH_PROLOGUE,
                  len:     MEDIA_FETCH_PROLOGUE_LEN,
               },
               emoji_check: Prologue {
                  pattern: Pattern::new("44 0F B6 ?? ?? B0 03 45 89 ?? 41 83 ?? 02"),
                  len:     14,
               },
            }
         },
         Abi::SysV => {
            Self {
               row:         SYSV_FRAME,
               frame:       SYSV_FRAME,
               media_fetch: SYSV_FRAME,
               // Four pushes and `sub rsp, imm32`.
               asset_load:  Prologue {
                  pattern: Pattern::new("41 57 41 56 41 54 53 48 81 EC ?? ?? ?? ??"),
                  len:     14,
               },
               emoji_check: Prologue {
                  pattern: Pattern::new("0F B6 ?? ?? B0 03 89 ?? 83 ?? 02 83 ?? 0E"),
                  len:     14,
               },
            }
         },
      }
   }
}

const fn prologues(image: &Image<'_>) -> Prologues {
   Prologues::of(Abi::of(image))
}

/// The 3-bit register number and its REX extension bit.
fn reg(register: Register) -> (u8, u8) {
   let number = u8::try_from(register.number()).unwrap_or(0);
   (number & 7, number >> 3 & 1)
}

/// `mov r32, imm32`, up to the immediate.
fn mov_imm32(register: Register) -> Vec<u8> {
   match reg(register) {
      (number, 0) => vec![0xB8 | number],
      (number, _) => vec![0x41, 0xB8 | number],
   }
}

/// The two ways a register gets a small constant: `mov r32, imm32`, and
/// `push imm8; pop r64`, which size-optimized builds prefer.
pub(crate) fn imm_loads(register: Register, value: u8) -> [Vec<u8>; 2] {
   let mut mov = mov_imm32(register);
   mov.extend([value, 0, 0, 0]);
   let pop = match reg(register) {
      (number, 0) => vec![0x6A, value, 0x58 | number],
      (number, _) => vec![0x6A, value, 0x41, 0x58 | number],
   };
   [mov, pop]
}

/// `test r8, r8` on a register's low byte, and `test r32, r32`.
fn test_self(register: Register) -> [Vec<u8>; 2] {
   let (number, ext) = reg(register);
   let modrm = 0xC0 | number << 3 | number;
   match ext {
      1 => [vec![0x45, 0x84, modrm], vec![0x45, 0x85, modrm]],
      _ if number >= 4 => [vec![0x40, 0x84, modrm], vec![0x85, modrm]],
      _ => [vec![0x84, modrm], vec![0x85, modrm]],
   }
}

/// `cmp r8, imm8` on a register's low byte.
fn cmp_byte(register: Register, value: u8) -> Vec<u8> {
   match reg(register) {
      (number, 1) => vec![0x41, 0x80, 0xF8 | number, value],
      (number, _) if number >= 4 => vec![0x40, 0x80, 0xF8 | number, value],
      (number, _) => vec![0x80, 0xF8 | number, value],
   }
}

/// The `disp8` of `lea register, [base + disp8]` at the start of `code`,
/// whatever the base.
fn lea_disp8(code: &[u8], register: Register) -> Option<u8> {
   let (number, ext) = reg(register);
   let [rex, 0x8D, modrm, disp, ..] = *code else {
      return None;
   };
   // `rsp` and `rbp` bases address the stack, not a struct.
   (rex & 0xFC == 0x48 | ext << 2
      && modrm & 0xF8 == 0x40 | number << 3
      && !matches!(modrm & 7, 4 | 5))
   .then_some(disp)
}

/// The source register number of `mov register, src` at the start of
/// `code`, in either encoding.
fn mov_from(code: &[u8], register: Register) -> Option<u8> {
   let (number, ext) = reg(register);
   match *code {
      [rex, 0x89, modrm, ..] if rex & 0xF9 == 0x48 | ext && modrm & 0xC7 == 0xC0 | number => {
         Some((rex >> 2 & 1) << 3 | modrm >> 3 & 7)
      },
      [rex, 0x8B, modrm, ..]
         if rex & 0xFC == 0x48 | ext << 2 && modrm & 0xF8 == 0xC0 | number << 3 =>
      {
         Some((rex & 1) << 3 | modrm & 7)
      },
      _ => None,
   }
}

/// The `disp8` of `mov [base + disp8], src` (`REX.W 89 /r`) at the start of
/// `code`, for a source register number.
fn store_disp8(code: &[u8], src: u8) -> Option<u8> {
   let [rex, 0x89, modrm, disp, ..] = *code else {
      return None;
   };
   (rex & 0xFC == 0x48 | (src >> 3) << 2 && modrm & 0xF8 == 0x40 | (src & 7) << 3 && modrm & 7 != 4)
      .then_some(disp)
}

/// The size of the first `alloc(size, 8)` in `code`: `mov arg0, size` and
/// then the alignment into `arg1`.
fn alloc_size(abi: Abi, code: &[u8]) -> Option<usize> {
   let size = mov_imm32(abi.arg(0));
   let aligns = imm_loads(abi.arg(1), 8);
   (0..code.len()).find_map(|at| {
      let rest = code.get(at..)?;
      let imm = rest.strip_prefix(size.as_slice())?.get(..4)?;
      aligns
         .iter()
         .any(|align| rest[size.len() + 4..].starts_with(align))
         .then(|| u32::from_le_bytes(imm.try_into().unwrap_or_default()) as usize)
   })
}

/// `lea r64, [rip + disp32]`, up to the displacement.
fn lea_rip(register: Register) -> [u8; 3] {
   let (number, ext) = reg(register);
   [0x48 | ext << 2, 0x8D, 0x05 | number << 3]
}

/// `op r64, [base + disp8]` with `REX.W`, up to the displacement. `base` is
/// never `rsp` or `r12`, which need a SIB byte.
fn rm_disp8(op: u8, register: Register, base: Register) -> [u8; 3] {
   let ((number, ext), (base, base_ext)) = (reg(register), reg(base));
   [0x48 | ext << 2 | base_ext, op, 0x40 | number << 3 | base]
}

/// `mov dst, src` in its two encodings.
fn mov_rr(dst: Register, src: Register) -> [[u8; 3]; 2] {
   let ((dst, dst_ext), (src, src_ext)) = (reg(dst), reg(src));
   [
      [0x48 | src_ext << 2 | dst_ext, 0x89, 0xC0 | src << 3 | dst],
      [0x48 | dst_ext << 2 | src_ext, 0x8B, 0xC0 | dst << 3 | src],
   ]
}

/// Copies of the string literal `text`. COFF builds end each literal with a
/// NUL, which tells `text` from a longer literal starting with it. LLD packs
/// ELF literals back to back, so there every copy counts.
fn literal(image: &Image<'_>, text: &[u8]) -> Vec<usize> {
   match Abi::of(image) {
      Abi::Win64 => image.find_bytes(&[text, b"\0"].concat()),
      Abi::SysV => image.find_bytes(text),
   }
}

/// What the runtime fixes at compile time. Every Mana and GPUI struct it builds
/// is decoded from the binary instead, and must fit the scratch buffer it is
/// built in.
pub mod layout {
   /// Bytes reserved for a `gpui::Div`, which is moved by value everywhere.
   pub const DIV_CAP: usize = 0x100;
   /// Bytes reserved for each other Mana struct (`Switch`, `Button`, `Modal`,
   /// `TextInput`, `TextArea`, `MenuItem`).
   pub const SCRATCH_CAP: usize = 0x800;
   /// Bytes reserved for a `gpui::Svg`: its `Interactivity`, paths and
   /// transformation.
   pub const SVG_CAP: usize = 0x800;
   /// `gpui::AnyElement`, a boxed element and its vtable.
   pub const ANY_ELEMENT: usize = 0x20;
   /// `gpui::Entity<T>`: id, `Weak` and `TypeId`.
   pub const ENTITY: usize = 0x20;
   /// `MenuItem::Separator`, a variant with no payload.
   pub const MENU_ITEM_SEPARATOR_TAG: u64 = 4;
}

/// Code references as `target << 32 | site` RVAs sorted by target: every
/// [`call_target`] into code, or every RIP-relative `lea`. One pass replaces a
/// `.text` scan per lookup, about a second each on `Discord.exe`.
struct RefIndex {
   key:  (usize, usize),
   base: usize,
   refs: Vec<u64>,
}

static CALLS: Mutex<Option<Arc<RefIndex>>> = Mutex::new(None);
static LEAS: Mutex<Option<Arc<RefIndex>>> = Mutex::new(None);

/// The shared index in `cache` for `image`, built on first use from the
/// `(site, target)` pairs `refs` finds in each executable segment.
fn ref_index(
   image: &Image<'_>,
   cache: &Mutex<Option<Arc<RefIndex>>>,
   refs: impl Fn(usize, &[u8], &mut dyn FnMut(usize, usize)),
) -> Arc<RefIndex> {
   let base = image
      .segments
      .iter()
      .map(|segment| segment.addr)
      .min()
      .unwrap_or(0);
   let code_len = image
      .segments
      .iter()
      .filter(|segment| segment.executable)
      .map(|segment| segment.bytes.len())
      .sum();
   let key = (base, code_len);
   let mut cached = cache.lock().unwrap_or_else(PoisonError::into_inner);
   if let Some(index) = cached.as_ref().filter(|index| index.key == key) {
      return Arc::clone(index);
   }
   let rva = |addr: usize| {
      addr
         .checked_sub(base)
         .and_then(|rva| u32::try_from(rva).ok())
         .map(u64::from)
   };
   let mut all = Vec::new();
   for segment in image.segments.iter().filter(|segment| segment.executable) {
      refs(segment.addr, &segment.bytes, &mut |site, target| {
         if let (Some(target), Some(site)) = (rva(target), rva(site)) {
            all.push(target << 32 | site);
         }
      });
   }
   all.sort_unstable();
   let index = Arc::new(RefIndex {
      key,
      base,
      refs: all,
   });
   *cached = Some(Arc::clone(&index));
   index
}

/// Every call into code.
fn call_index(image: &Image<'_>) -> Arc<RefIndex> {
   ref_index(image, &CALLS, |addr, bytes, push| {
      for offset in memchr::memchr2_iter(0xE8, 0xFF, bytes) {
         if let Some((_, callee)) = call_target(image, &bytes[offset..], addr + offset)
            && image.is_code(callee)
         {
            push(addr + offset, callee);
         }
      }
   })
}

/// Every `lea r, [rip + disp32]`, any REX.
fn lea_index(image: &Image<'_>) -> Arc<RefIndex> {
   ref_index(image, &LEAS, |addr, bytes, push| {
      for pos in memchr::memchr_iter(0x8D, bytes) {
         let (Some(&rex), Some(&modrm), Some(disp)) = (
            pos.checked_sub(1).and_then(|at| bytes.get(at)),
            bytes.get(pos + 1),
            bytes.get(pos + 2..pos + 6),
         ) else {
            continue;
         };
         if rex & 0xF0 != 0x40 || modrm & 0xC7 != 0x05 {
            continue;
         }
         let disp = i32::from_le_bytes([disp[0], disp[1], disp[2], disp[3]]);
         if let Some(target) = (addr + pos + 6).checked_add_signed(disp as isize) {
            push(addr + pos - 1, target);
         }
      }
   })
}

/// Frees the shared indexes once every locator has run.
pub fn release_call_index() {
   *CALLS.lock().unwrap_or_else(PoisonError::into_inner) = None;
   *LEAS.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

impl RefIndex {
   /// The sites referencing a target in `targets`, as `(site, target)` in
   /// target then site order.
   fn range(&self, targets: std::ops::RangeInclusive<usize>) -> Vec<(usize, usize)> {
      let rva = |addr: usize| {
         u64::try_from(addr.saturating_sub(self.base))
            .unwrap_or(u64::MAX)
            .min(u64::from(u32::MAX))
      };
      let (low, high) = (rva(*targets.start()), rva(*targets.end()));
      if *targets.end() < self.base {
         return Vec::new();
      }
      let start = self.refs.partition_point(|&r| r >> 32 < low);
      let end = self.refs.partition_point(|&r| r >> 32 <= high);
      self.refs[start..end]
         .iter()
         .map(|&r| {
            (
               self.base + (r & 0xFFFF_FFFF) as usize,
               self.base + (r >> 32) as usize,
            )
         })
         .collect()
   }

   /// The sites referencing `target`, in address order.
   fn sites(&self, target: usize) -> impl Iterator<Item = usize> {
      self
         .range(target..=target)
         .into_iter()
         .map(|(site, _)| site)
   }

   /// `(site, target)` for every reference whose target passes `keep`.
   fn matching(&self, keep: impl Fn(usize) -> bool) -> Vec<(usize, usize)> {
      self
         .refs
         .iter()
         .map(|&r| {
            (
               self.base + (r & 0xFFFF_FFFF) as usize,
               self.base + (r >> 32) as usize,
            )
         })
         .filter(|&(_, target)| keep(target))
         .collect()
   }
}

/// `mov [base + d], ptr; lea vt, [rip + vtable]; mov [base + d + 8], vt`,
/// storing an `Rc<dyn Fn>`, and the `mov ecx, size` of the return copy that
/// may follow within 16 bytes.
fn rc_store(code: &[u8]) -> Option<(usize, Option<usize>)> {
   // `mov [base + disp], src` as `(base, src, disp, len)`.
   let mem = |at: usize| -> Option<(u8, u8, usize, usize)> {
      let (rex, op, modrm) = (*code.get(at)?, *code.get(at + 1)?, *code.get(at + 2)?);
      if rex & 0xFA != 0x48 || op != 0x89 || modrm & 7 == 4 {
         return None;
      }
      let (base, src) = (
         (rex & 1) << 3 | modrm & 7,
         (rex >> 2 & 1) << 3 | modrm >> 3 & 7,
      );
      match modrm >> 6 {
         1 => Some((base, src, usize::from(*code.get(at + 3)?), 4)),
         2 => {
            let disp = code.get(at + 3..at + 7)?;
            Some((
               base,
               src,
               u32::from_le_bytes(disp.try_into().ok()?) as usize,
               7,
            ))
         },
         _ => None,
      }
   };
   (0..code.len()).find_map(|at| {
      let (base, _, field, len) = mem(at)?;
      let lea = at + len;
      let [rex, 0x8D, modrm] = *code.get(lea..lea + 3)? else {
         return None;
      };
      if rex & 0xFB != 0x48 || modrm & 0xC7 != 0x05 {
         return None;
      }
      let vtable = (rex >> 2 & 1) << 3 | modrm >> 3 & 7;
      let (base2, src2, next, len2) = mem(lea + 7)?;
      if base2 != base || src2 != vtable || next != field + 8 {
         return None;
      }
      let tail = code.get(lea + 7 + len2..).unwrap_or_default();
      let tail = &tail[..tail.len().min(16)];
      let copy = tail
         .windows(5)
         .find(|w| w[0] == 0xB9)
         .filter(|_| tail.windows(2).any(|w| w == [0xF3, 0xA4]))
         .map(|w| u32::from_le_bytes([w[1], w[2], w[3], w[4]]) as usize);
      Some((field, copy))
   })
}

/// The field a `SharedString` setter replaces: `movzx eax, byte [reg + d];
/// cmp al, 0xff; ...; cmp al, 0x19`, the drop of the old value.
fn shared_string_setter(code: &[u8]) -> Option<usize> {
   (0..code.len()).find_map(|at| {
      let w = code.get(at..at + 9)?;
      if w[0..2] != [0x0F, 0xB6] || w[2] & 0xF8 != 0x80 || w[2] == 0x84 || w[7..9] != [0x3C, 0xFF] {
         return None;
      }
      let after = code.get(at + 9..(at + 21).min(code.len()))?;
      after
         .windows(2)
         .any(|w| w == [0x3C, 0x19])
         .then(|| u32::from_le_bytes([w[3], w[4], w[5], w[6]]) as usize)
   })
}

/// Offsets of `mov byte [reg + d8], 0xff` stores, a `None` written into an
/// `Option<SharedString>`.
fn none_stores(code: &[u8]) -> Vec<usize> {
   code
      .windows(4)
      .filter(|w| w[0] == 0xC6 && w[1] & 0xF8 == 0x40 && w[1] != 0x44 && w[3] == 0xFF)
      .map(|w| usize::from(w[2]))
      .collect()
}

/// Offsets of `lea reg, [base + d8]` followed within 16 bytes by
/// `mov byte [reg], 0xff`, the same `None` store where the build keeps the
/// slot's address to pass it to a drop call first.
fn none_stores_through(code: &[u8]) -> Vec<usize> {
   (0..code.len())
      .filter_map(|at| {
         let [rex, 0x8D, modrm, ..] = *code.get(at..at + 5)? else {
            return None;
         };
         if rex & 0xF8 != 0x48 || modrm & 0xC0 != 0x40 {
            return None;
         }
         let (disp, len) = if modrm & 7 == 4 {
            (code[at + 4], 5)
         } else {
            (code[at + 3], 4)
         };
         let dst = modrm >> 3 & 7;
         if dst == 4 || dst == 5 {
            return None;
         }
         let store: &[u8] = if rex & 4 == 0 {
            &[0xC6, dst, 0xFF]
         } else {
            &[0x41, 0xC6, dst, 0xFF]
         };
         let after = code.get(at + len..(at + len + 16).min(code.len()))?;
         after
            .windows(store.len())
            .enumerate()
            .any(|(pos, w)| {
               w == store && (rex & 4 != 0 || pos == 0 || after[pos - 1] & 0xF0 != 0x40)
            })
            .then_some(usize::from(disp))
      })
      .collect()
}

/// The 32-bit immediate after the first `prefix`.
fn imm_after(code: &[u8], prefix: &[u8]) -> Option<usize> {
   let at = code.windows(prefix.len()).position(|w| w == prefix)? + prefix.len();
   let imm = code.get(at..at + 4)?;
   Some(u32::from_le_bytes(imm.try_into().ok()?) as usize)
}

fn function_code<'a>(image: &'a Image<'_>, func: usize, name: &str) -> Found<&'a [u8]> {
   let (start, end) = image
      .function_bounds(func)
      .ok_or_else(|| format!("{name} has no unwind entry"))?;
   image
      .read(start, end - start)
      .ok_or_else(|| format!("{name} is unmapped"))
}

fn fits(name: &str, size: usize, cap: usize) -> Found<usize> {
   if size == 0 || size > cap {
      return Err(format!(
         "{name} is {size:#x} bytes, the runtime reserves {cap:#x}"
      ));
   }
   Ok(size)
}

fn imm32(image: &Image<'_>, at: usize) -> Option<usize> {
   image.read_u32(at).map(|value| value as usize)
}

fn rel32_target(image: &Image<'_>, field: usize) -> Option<usize> {
   let rel = image.read_u32(field)?.cast_signed();
   (field + 4).checked_add_signed(rel as isize)
}

/// Starts of the `lea reg, [rip + target]` instructions, in address order.
pub(crate) fn lea_refs(image: &Image<'_>, target: usize) -> Vec<usize> {
   lea_index(image).sites(target).collect()
}

/// [`lea_refs`] for every target.
fn lea_refs_many(image: &Image<'_>, targets: &[usize]) -> HashMap<usize, Vec<usize>> {
   let index = lea_index(image);
   targets
      .iter()
      .map(|&target| (target, index.sites(target).collect::<Vec<_>>()))
      .filter(|(_, sites)| !sites.is_empty())
      .collect()
}

/// `(site, callee)` for every `call rel32` whose callee passes `keep`.
fn call_sites(image: &Image<'_>, keep: impl Fn(usize) -> bool) -> Vec<(usize, usize)> {
   call_index(image).matching(keep)
}

/// The functions that call `callee`.
fn callers(image: &Image<'_>, callee: usize) -> HashSet<usize> {
   call_index(image)
      .sites(callee)
      .filter_map(|site| Some(image.function_bounds(site)?.0))
      .collect()
}

/// Calls in `[start, start + len)` to what looks like a function entry, in
/// order.
fn calls_in(image: &Image<'_>, start: usize, len: usize) -> Vec<(usize, usize)> {
   let Some(bytes) = image.read(start, len) else {
      return Vec::new();
   };
   // A `call rel32` decoded at the wrong offset lands anywhere in `.text`, so
   // its target must also open like a function, and not sit inside one the
   // unwind table bounds. Linux build 2233's TesterUpdates page holds such a
   // false call into the middle of a function right before `ActionRow`.
   let entry = |target: usize| {
      if image
         .function_bounds(target)
         .is_some_and(|(start, _)| start != target)
      {
         return false;
      }
      image.is_function(target)
         || image.read(target, 1).is_some_and(|first| {
            matches!(
               first[0],
               0xE9
                  | 0x48
                  | 0x4C
                  | 0x40
                  | 0x41
                  | 0x55
                  | 0x53
                  | 0x56
                  | 0x57
                  | 0x89
                  | 0x8B
                  | 0xB8
                  | 0x31
                  | 0x33
            )
         })
   };
   let mut out = Vec::new();
   let mut idx = 0;
   while idx < bytes.len() {
      if let Some((size, target)) = call_target(image, &bytes[idx..], start + idx)
         && image.is_code(target)
         && (bytes[idx] == 0xFF || entry(target))
      {
         out.push((start + idx, target));
         idx += size;
         continue;
      }
      idx += 1;
   }
   out
}

/// The calls `func` makes before its first `ret`, a tail jump to another
/// function counting as the last. ELF builds place their unwind landing pads
/// after the `ret`, where Win64 keeps them out of line.
fn straight_calls(image: &Image<'_>, func: usize) -> Vec<(usize, usize)> {
   let Ok(code) = function_code(image, func, "") else {
      return Vec::new();
   };
   let lines = phoenix_hook::x86::disasm(code, func, usize::MAX);
   let body = func..func + code.len();
   // A jump inside the function is a branch of it, not where it ends.
   let end = lines
      .iter()
      .position(|line| {
         line.text == "ret"
            || line.text.starts_with("jmp ")
               && line.target.is_none_or(|target| !body.contains(&target))
      })
      .unwrap_or(lines.len());
   let mut calls = calls_in(
      image,
      func,
      lines.get(end).map_or(code.len(), |line| line.addr - func),
   );
   if let Some(line) = lines.get(end)
      && let Some(target) = jump_target(image, &code[line.addr - func..], line.addr)
      && image.is_function(target)
   {
      calls.push((line.addr, target));
   }
   calls
}

/// Whether `a` and `b` run the same instructions up to their first `ret`.
/// ELF builds fold no identical functions, so a generic can exist twice.
fn same_code(image: &Image<'_>, a: usize, b: usize) -> bool {
   let body = |func: usize| -> Option<Vec<String>> {
      let code = function_code(image, func, "").ok()?;
      let lines = phoenix_hook::x86::disasm(code, func, 0x100);
      let end = lines.iter().position(|line| line.text == "ret")?;
      // Branches inside the function compare by their offset from its start.
      let local = func..func + code.len();
      Some(
         lines
            .into_iter()
            .take(end)
            .map(|line| {
               match line.target.filter(|target| local.contains(target)) {
                  Some(target) => format!("{} +{:x}", line.len, target - func),
                  None => line.text,
               }
            })
            .collect(),
      )
   };
   body(a).is_some_and(|lines| Some(lines) == body(b))
}

/// The distinct code targets of calls in `[start, end)`.
pub(crate) fn callees(image: &Image<'_>, start: usize, end: usize) -> Vec<usize> {
   let Some(bytes) = image.read(start, end.saturating_sub(start)) else {
      return Vec::new();
   };
   let mut out = Vec::new();
   for idx in 0..bytes.len() {
      if let Some((_, target)) = call_target(image, &bytes[idx..], start + idx)
         && image.is_code(target)
         && !out.contains(&target)
      {
         out.push(target);
      }
   }
   out
}

/// The function `ClientAccessGate` reads through, which the runtime makes
/// answer "no gate". Its one small caller is `signed_in && access(..) != 2`.
pub fn access_gate(image: &Image<'_>) -> Found<usize> {
   // The `call`'s offset in each: SysV aligns the stack with `push rax` first.
   let (caller, call) = match Abi::of(image) {
      Abi::Win64 => {
         (
            Pattern::new("83 79 ?? 03 75 0C E8 ?? ?? ?? ?? 3C 02 0F 95 C0 EB 02 31 C0"),
            6,
         )
      },
      Abi::SysV => {
         (
            Pattern::new("83 7F ?? 03 75 ?? 50 E8 ?? ?? ?? ?? 3C 02 0F 95 C0 48 83 C4 08 C3"),
            7,
         )
      },
   };
   let caller = one("access gate caller", &image.scan(caller))?;
   let access = rel32_target(image, caller + call + 1).ok_or("access gate call does not decode")?;
   if image.is_code(access) {
      Ok(access)
   } else {
      Err(format!(
         "access gate call at {:#x} leaves the code",
         caller + call
      ))
   }
}

/// Phoenix's `getEmojiUnavailableReason`, `u8 f(&Emoji, &EmojiContext)`,
/// where 0xFF is usable and 2 premium locked.
#[derive(Clone, Copy, Debug)]
pub struct EmojiCheck {
   pub func:      usize,
   /// `EmojiContext.intention`, 3 (`CHAT`) for the message box.
   pub intention: usize,
}

/// The same leaf Android has, answering 3 for profiles first.
///
/// `movzx r, byte [ctx + intention]; mov al, 3; mov r2, r; or r2, 2;
/// cmp r2, 14; jne; ret`. The intention's offset is read back from the
/// `movzx`, which must address the context argument.
pub fn emoji_check(image: &Image<'_>) -> Found<EmojiCheck> {
   let abi = Abi::of(image);
   // And where the `movzx`'s ModRM sits.
   let (check, modrm) = match abi {
      Abi::Win64 => {
         (
            Pattern::new("44 0F B6 ?? ?? B0 03 45 89 ?? 41 83 ?? 02 41 83 ?? 0E 75 01 C3"),
            3,
         )
      },
      Abi::SysV => {
         (
            Pattern::new("0F B6 ?? ?? B0 03 89 ?? 83 ?? 02 83 ?? 0E 75 01 C3"),
            2,
         )
      },
   };
   let func = one("emoji check", &image.scan(check))?;
   let (context, _) = reg(abi.arg(1));
   match image.read(func + modrm, 2) {
      Some(&[modrm, disp]) if modrm & 0xC7 == 0x40 | context => {
         Ok(EmojiCheck {
            func,
            intention: usize::from(disp),
         })
      },
      _ => Err(format!("emoji check at {func:#x} reads no context byte")),
   }
}

/// The id the DM list gives its pending friend requests badge.
pub const FRIENDS_BADGE: &[u8] = b"dm-friends-badge";
/// The id the DM list gives its message requests badge.
pub const MESSAGE_REQUESTS_BADGE: &[u8] = b"dm-message-requests-badge";

/// What draws Phoenix's pending friend and message request counts.
#[derive(Clone, Copy, Debug)]
pub struct PendingCounts {
   /// The DM list's row badge, `fn(out, id: &str, count: u32, _, _) -> out`,
   /// which draws [`FRIENDS_BADGE`] and [`MESSAGE_REQUESTS_BADGE`] only.
   pub badge:           usize,
   /// `u32 f(&RelationshipStore)`, the incoming friend and game friend
   /// requests the DM list's Friends row and the inbox's Friends tab show.
   pub friend_requests: usize,
}

/// The DM list draws both badges through one builder, each call just after the
/// `lea` of its id. The friends count comes from the call whose `eax` the
/// count argument was copied from.
pub fn pending_counts(image: &Image<'_>) -> Found<PendingCounts> {
   let (friends_call, badge) = badge_call(image, FRIENDS_BADGE)?;
   let (_, requests_badge) = badge_call(image, MESSAGE_REQUESTS_BADGE)?;
   if badge != requests_badge {
      return Err(format!(
         "the DM badges are drawn by {badge:#x} and {requests_badge:#x}"
      ));
   }
   let friend_requests = count_source(image, friends_call)?;
   function_code(image, friend_requests, "friend requests count")?;
   if !counts_incoming(image, friend_requests) {
      return Err(format!(
         "friend requests count at {friend_requests:#x} compares no type with 3"
      ));
   }
   Ok(PendingCounts {
      badge,
      friend_requests,
   })
}

/// `u32 f(&RelationshipStore)`, the number on the Home button.
///
/// It adds the DM mentions to the friend requests `friend_requests` counts,
/// inlined but for its one call, which counts the game friend requests. The
/// Home button is the other caller of that which counts incoming requests and
/// returns a plain count.
pub fn home_count(image: &Image<'_>, friend_requests: usize) -> Found<usize> {
   let game_requests = game_requests(image, friend_requests)?;
   let counts = callers(image, game_requests)
      .into_iter()
      .filter(|&func| {
         func != friend_requests && !returns_badge(image, func) && counts_incoming(image, func)
      })
      .collect::<Vec<_>>();
   one("Home count", &counts)
}

/// `(state, count) f(&Store, _)`, a badge built from the same counts as
/// [`home_count`].
///
/// State 2 is a count, state 1 an unread dot. The badge is the caller of the
/// game friend requests count which returns a count in `edx`.
pub fn home_badge(image: &Image<'_>, friend_requests: usize) -> Found<usize> {
   let game_requests = game_requests(image, friend_requests)?;
   let badges = callers(image, game_requests)
      .into_iter()
      .filter(|&func| func != friend_requests && returns_badge(image, func))
      .collect::<Vec<_>>();
   one("Home badge", &badges)
}

/// The one call `friend_requests` makes, which counts the game friend requests.
fn game_requests(image: &Image<'_>, friend_requests: usize) -> Found<usize> {
   let calls = straight_calls(image, friend_requests);
   let [(_, game_requests)] = calls[..] else {
      return Err(format!(
         "friend requests count at {friend_requests:#x} makes {} calls, not 1",
         calls.len()
      ));
   };
   Ok(game_requests)
}

/// Whether `func` compares a relationship type with 3, an incoming request, as
/// it opens.
fn counts_incoming(image: &Image<'_>, func: usize) -> bool {
   function_code(image, func, "").is_ok_and(|code| {
      phoenix_hook::x86::disasm(code, func, 0x80)
         .iter()
         .any(|line| line.text.starts_with("cmp byte ptr [") && line.text.ends_with("],3"))
   })
}

/// Whether `func` can answer state 2 in `eax` and sets `edx` just before its
/// epilogue.
fn returns_badge(image: &Image<'_>, func: usize) -> bool {
   let Ok(code) = function_code(image, func, "") else {
      return false;
   };
   let lines = phoenix_hook::x86::disasm(code, func, usize::MAX);
   let Some(ret) = lines.iter().position(|line| line.text == "ret") else {
      return false;
   };
   let body = &lines[..ret];
   let sets_count = body
      .iter()
      .rev()
      .find(|line| !line.text.starts_with("pop ") && !line.text.starts_with("add rsp,"))
      .is_some_and(|line| line.text.starts_with("mov edx,"));
   sets_count && body.iter().any(|line| line.text == "mov eax,2")
}

/// The one `lea` of the UI id `id`.
fn id_lea(image: &Image<'_>, id: &[u8]) -> Found<usize> {
   // The ids are packed back to back without a NUL, so `literal` misses them.
   let leas = image
      .find_bytes(id)
      .into_iter()
      .flat_map(|at| lea_refs(image, at))
      .collect::<Vec<_>>();
   one(&format!("{} lea", String::from_utf8_lossy(id)), &leas)
}

/// The site and callee of the `call` that draws the badge with id `id`, the
/// first after the `lea` of the id.
fn badge_call(image: &Image<'_>, id: &[u8]) -> Found<(usize, usize)> {
   let lea = id_lea(image, id)?;
   calls_in(image, lea, 0x40).first().copied().ok_or_else(|| {
      format!(
         "no call follows the {} lea at {lea:#x}",
         String::from_utf8_lossy(id)
      )
   })
}

/// Follows the count argument of the `call` at `site` back through register
/// copies to the call that returned it in `eax`.
fn count_source(image: &Image<'_>, site: usize) -> Found<usize> {
   let func = image
      .function_containing(site)
      .ok_or_else(|| format!("badge call at {site:#x} is in no function"))?;
   let code = image
      .read(func, site - func)
      .ok_or_else(|| format!("DM list at {func:#x} is unmapped"))?;
   let lines = phoenix_hook::x86::disasm(code, func, usize::MAX);
   let mut reg = reg32_name(Abi::of(image).arg(3));
   for (at, line) in lines.iter().enumerate().rev() {
      let Some(source) = line.text.strip_prefix(&format!("mov {reg},")) else {
         continue;
      };
      if source != "eax" {
         if source.contains('[') {
            return Err(format!(
               "the friends badge count is loaded from memory at {:#x}",
               line.addr
            ));
         }
         source.clone_into(&mut reg);
         continue;
      }
      // Win64 pads calls that can unwind with a `nop`.
      let before = lines[..at].iter().rev().find(|prev| prev.text != "nop");
      return match before {
         Some(call) if call.text.starts_with("call ") => {
            call
               .target
               .ok_or_else(|| format!("call at {:#x} has no target", call.addr))
         },
         _ => Err(format!("{reg} at {:#x} is no call's result", line.addr)),
      };
   }
   Err(format!("the badge call at {site:#x} has no count source"))
}

/// The 32-bit name of a 64-bit general register, as the disassembly prints it.
fn reg32_name(register: Register) -> String {
   let name = reg_name(register);
   match name.strip_prefix('r') {
      Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => format!("{name}d"),
      Some(rest) => format!("e{rest}"),
      None => name,
   }
}

/// The `jmp [rip]` and 8-byte target [`Image::hook_entry`] writes.
const ENTRY_JUMP_LEN: usize = 14;

/// The whole instructions opening `func` that an absolute jump covers, for a
/// hook on a function no fixed prologue describes.
pub fn entry_prologue(image: &Image<'_>, func: usize) -> Found<Prologue> {
   let head = image
      .read(func, 32)
      .ok_or_else(|| format!("{func:#x} is unmapped"))?;
   let mut len = 0;
   for line in phoenix_hook::x86::disasm(head, func, head.len()) {
      len += line.len;
      if len < ENTRY_JUMP_LEN {
         continue;
      }
      if !phoenix_hook::x86::relocatable(&head[..len], func) {
         return Err(format!("{func:#x} opens with code that cannot move"));
      }
      let hex = head[..len]
         .iter()
         .map(|byte| format!("{byte:02X}"))
         .collect::<Vec<_>>()
         .join(" ");
      let pattern = Pattern::leak(hex).ok_or("prologue bytes make no pattern")?;
      return Ok(Prologue { pattern, len });
   }
   Err(format!("{func:#x} opens with less than a jump's bytes"))
}

/// Settings page 24's sidebar name, `Phoenix`, and the `PHOENIX` that ends
/// its id, both relabeled in place.
#[derive(Clone, Copy, Debug)]
pub struct SettingsLabel {
   pub name: usize,
   pub id:   usize,
}

/// Page 24's label, from its one loader: `lea rax, [name]` with length 7,
/// then a load of the id.
///
/// The id is `DESKTOP_PHOENIX` through Linux build 2233 and `PHOENIX` after
/// it. COFF builds keep name and id as one literal, `PhoenixDESKTOP_PHOENIX`;
/// ELF builds place them apart.
pub fn settings_label_parts(image: &Image<'_>) -> Found<SettingsLabel> {
   const NAME: &[u8] = b"Phoenix";
   const ID: &[u8] = b"PHOENIX";
   const PREFIX: &[u8] = b"DESKTOP_";
   let suffixes = image.find_bytes(ID);
   let ids: Vec<usize> = suffixes
      .iter()
      .filter_map(|&suffix| {
         let start = suffix.checked_sub(PREFIX.len())?;
         (image.read(start, PREFIX.len())? == PREFIX).then_some(start)
      })
      .chain(suffixes.iter().copied())
      .collect();
   let mut found: Vec<(usize, usize)> = lea_refs_many(image, &ids)
      .into_iter()
      .flat_map(|(id, loads)| loads.into_iter().map(move |lea| (id, lea)))
      .filter_map(|(id, lea)| {
         let start = lea.checked_sub(0x30)?;
         let window = image.read(start, 0x30)?;
         let at = window
            .windows(3)
            .rposition(|w| w == lea_rip(Register::RAX))?;
         let name = rel32_target(image, start + at + 3)?;
         let suffix = if image.read(id, ID.len())? == ID {
            id
         } else {
            id + PREFIX.len()
         };
         (image.read(name, NAME.len())? == NAME).then_some((name, suffix))
      })
      .collect();
   found.sort_unstable();
   found.dedup();
   let (name, id) = one("settings label loader", &found)?;
   Ok(SettingsLabel { name, id })
}

/// The `cmp <arg2>, 0xf` immediate in `is_page_visible`, which the runtime
/// lowers so page 24 is always shown.
pub fn settings_visibility(image: &Image<'_>) -> Found<usize> {
   // `movzx ecx, <page>; mov al, 1; lea <arg2>, [rcx-9]; cmp <arg2>, 0xf; ja
   // ...`
   let (guard, imm) = match Abi::of(image) {
      Abi::Win64 => {
         (
            Pattern::new("0F B6 C9 B0 01 44 8D 41 F7 41 83 F8 0F 77"),
            12,
         )
      },
      Abi::SysV => (Pattern::new("40 0F B6 CF B0 01 8D 51 F7 83 FA 0F 77"), 11),
   };
   Ok(one("settings visibility guard", &image.scan(guard))? + imm)
}

/// `host::section`, which renders a settings page, and where its state keeps
/// the active page.
pub struct Section {
   pub func:        usize,
   pub page_offset: usize,
}

pub fn settings_section(image: &Image<'_>) -> Found<Section> {
   /// `mov bl, [r15 + disp32]`, loading `self.active_page`.
   const ACTIVE_PAGE_LOAD: Pattern = Pattern::new("41 8A 9F ?? ?? ?? ??");
   let anchor = one("account-loading", &image.find_bytes(b"account-loading"))?;
   let lea = one("account-loading load", &lea_refs(image, anchor))?;
   let (func, _) = image
      .function_bounds(lea)
      .ok_or("settings section has no unwind entry")?;
   if !prologues(image).frame.at(image, func) {
      return Err("settings section prologue changed".to_owned());
   }
   let head = image
      .read(func, 0x160)
      .ok_or("settings section is unmapped")?;
   let page_offset = ACTIVE_PAGE_LOAD
      .find_in(head)
      .into_iter()
      .filter_map(|at| imm32(image, func + at + 3))
      .find(|disp| (0x100..=0x2000).contains(disp))
      .ok_or("settings section active page load not found")?;
   Ok(Section { func, page_offset })
}

/// Discord's own GPUI and Mana builders the settings page calls.
#[derive(Debug, Clone)]
pub struct Gpui {
   /// The `Location` `div_new` takes, a static in `.rdata`.
   pub caller_loc:       usize,
   pub div_new:          usize,
   pub div_flex:         usize,
   pub div_flex_col:     usize,
   pub div_style_mut:    usize,
   pub div_child_div:    usize,
   pub div_child_button: usize,
   pub div_into_any:     usize,
   pub div_push_any:     Option<usize>,
   pub str_into_any:     Option<usize>,
   pub text_block:       usize,
   pub switch_new:       usize,
   pub switch_into_div:  usize,
   pub button_new:       usize,
   pub action_row:       usize,
   pub alloc_or_oom:     usize,
   /// Discord's `Switch` label, description and `on_change` setters, and a
   /// `Button::on_click`, which the layouts are read from.
   pub switch_label:     usize,
   pub switch_desc:      usize,
   pub switch_on_change: usize,
   pub button_on_click:  usize,
}

/// The builders, from the Developer Mode row of page 16 and the
/// `TesterUpdates` sign-in button, which call them in a fixed order.
#[expect(clippy::too_many_lines, reason = "one block per builder call site")]
pub fn gpui(image: &Image<'_>) -> Found<Gpui> {
   let abi = Abi::of(image);
   let dev = one(
      "set-devmode",
      &image.find_bytes(b"set-devmodeDeveloper Mode"),
   )?;
   let lea_dev = one("set-devmode load", &lea_refs(image, dev))?;
   let before_dev = calls_in(image, lea_dev - 0x70, 0x70);
   let after_dev = calls_in(image, lea_dev, 0x240);
   if before_dev.len() < 3 || after_dev.len() < 9 {
      return Err(format!(
         "page 16 call counts changed ({}, {})",
         before_dev.len(),
         after_dev.len()
      ));
   }
   let (div_new_call, div_new) = before_dev[0];
   let caller_loc =
      arg1_lea_before(image, abi, div_new_call).ok_or("div_new Location not found")?;
   let switch_on_change = after_dev[7].1;
   let alloc_or_oom = calls_in(image, switch_on_change, 0x50)
      .first()
      .ok_or("alloc_or_oom not found in Switch::on_change")?
      .1;

   let btn = one(
      "tester-updates-sign-in",
      &literal(image, b"tester-updates-sign-in"),
   )?;
   let lea_btn = one("tester-updates-sign-in load", &lea_refs(image, btn))?;
   let after_btn = calls_in(image, lea_btn, 0x1D0);
   if after_btn.len() < 6 {
      return Err(format!(
         "TesterUpdates call count changed ({})",
         after_btn.len()
      ));
   }
   // `ActionRow` is the callee that opens with `div_new` and `flex`,
   // `Button::on_click` the call before it and `Div::into_any_element` the one
   // after. ELF builds drop a temporary in between, so the order is all that
   // holds.
   let div_flex = before_dev[1].1;
   let (row_at, ar_calls) = after_btn
      .iter()
      .enumerate()
      .skip(1)
      .find_map(|(at, &(_, callee))| {
         let (start, end) = image.function_bounds(callee)?;
         let calls = calls_in(image, start, end - start);
         (calls.first().map(|&(_, first)| first) == Some(div_new)
            && calls.iter().take(4).any(|&(_, callee)| callee == div_flex))
         .then_some((at, calls))
      })
      .ok_or("ActionRow no longer calls div_new and flex first")?;
   let action_row = after_btn[row_at].1;
   let div_into_any = after_btn
      .get(row_at + 1)
      .ok_or("Div::into_any_element not found after ActionRow")?
      .1;
   // Two blocks, a `div_new` each, that end in `div_child_div`. The first
   // builds its text column three calls before that; the second's child
   // before it is the button.
   let second = ar_calls[1..]
      .iter()
      .position(|&(_, callee)| callee == div_new)
      .ok_or("ActionRow builds one Div")?
      + 1;
   let div_child_div = ar_calls[second - 1].1;
   let last_child = ar_calls
      .iter()
      .rposition(|&(_, callee)| callee == div_child_div)
      .filter(|&at| at > second + 1 && second >= 4)
      .ok_or("ActionRow no longer adds its children with div_child_div")?;
   let div_child_button = ar_calls[last_child - 1].1;
   let text_block = ar_calls[second - 4].1;
   // Every style setter fetches the style first, so `div_style_mut` is the
   // first call most of the row's setters make.
   let mut firsts: HashMap<usize, usize> = HashMap::new();
   for &(_, callee) in &ar_calls[..second] {
      if let Some(&(_, first)) = calls_in(image, callee, 0x40).first() {
         *firsts.entry(first).or_default() += 1;
      }
   }
   let mut ranked: Vec<(usize, usize)> = firsts
      .into_iter()
      .filter(|&(first, count)| first != div_new && count >= 2)
      .map(|(first, count)| (count, first))
      .collect();
   ranked.sort_unstable_by(|a, b| b.cmp(a));
   let div_style_mut = clear_winner("div_style_mut", &ranked, 2)?;

   // `SmallVec::<[AnyElement; 2]>::push`, after `lea arg0, [div + 0x40]`.
   let child_calls = calls_in(image, div_child_div, 0x60);
   let div_push_any = (child_calls.len() >= 2
      && image.read(div_child_div, 0x50).is_some_and(|code| {
         (0..code.len()).any(|at| lea_disp8(&code[at..], abi.arg(0)) == Some(0x40))
      }))
   .then(|| child_calls[1].1);
   // `SharedString::into_any_element`, beside `div_child_div`, drops the
   // `SharedString` it moved: `cmp r8, 0xff; je ..; cmp r8, 0x19`, on whichever
   // register the compiler loaded the tag into.
   let tag_checks: Vec<(Vec<u8>, Vec<u8>)> = [
      Register::RAX,
      Register::RCX,
      Register::RDX,
      Register::RBX,
      Register::RSI,
      Register::RDI,
      Register::R8,
      Register::R9,
      Register::R10,
      Register::R11,
      Register::R12,
      Register::R13,
      Register::R14,
      Register::R15,
   ]
   .into_iter()
   .map(|register| (cmp_byte(register, 0xFF), cmp_byte(register, 0x19)))
   .collect();
   let str_into_any = calls_in(image, div_child_div, 0x400)
      .into_iter()
      .map(|(_, target)| target)
      .find(|&target| {
         image.read(target, 0x100).is_some_and(|body| {
            tag_checks.iter().any(|(none, arc)| {
               body
                  .windows(2 * none.len() + 2)
                  .any(|w| w.starts_with(none) && w.ends_with(arc))
            })
         })
      });

   Ok(Gpui {
      caller_loc,
      div_new,
      div_flex,
      div_flex_col: before_dev[2].1,
      div_style_mut,
      div_child_div,
      div_child_button,
      div_into_any,
      div_push_any,
      str_into_any,
      text_block,
      switch_new: after_dev[0].1,
      switch_into_div: after_dev[8].1,
      button_new: after_btn[0].1,
      action_row,
      alloc_or_oom,
      switch_label: after_dev[2].1,
      switch_desc: after_dev[5].1,
      switch_on_change,
      button_on_click: after_btn[row_at - 1].1,
   })
}

/// The target of the last `lea arg1, [rip + disp32]` in the 32 bytes before
/// `call`.
fn arg1_lea_before(image: &Image<'_>, abi: Abi, call: usize) -> Option<usize> {
   let start = call.checked_sub(0x20)?;
   let bytes = image.read(start, 0x20)?;
   let lea = lea_rip(abi.arg(1));
   (0..=bytes.len() - 7)
      .rev()
      .find(|&idx| bytes[idx..idx + 3] == lea)
      .and_then(|idx| rel32_target(image, start + idx + 3))
}

/// `StyleRefinement` field offsets, read by name from its derived `Debug`.
/// Edges are `[top, right, bottom, left]`, corners `[top_left, top_right,
/// bottom_right, bottom_left]` and sizes `[width, height]`.
#[derive(Debug, Clone, Copy)]
pub struct StyleOffsets {
   /// `StyleRefinement` bytes, from `Interactivity::default`'s box.
   pub bytes:           usize,
   pub font_size:       usize,
   pub line_height:     usize,
   pub font_family:     usize,
   pub text_color:      usize,
   pub white_space:     usize,
   pub flex_shrink:     usize,
   pub background:      usize,
   pub border_color:    usize,
   pub overflow_x:      usize,
   pub position:        usize,
   pub flex_wrap:       usize,
   pub display:         usize,
   pub align_items:     usize,
   pub justify_content: usize,
   pub border_widths:   [usize; 4],
   pub corner_radii:    [usize; 4],
   pub padding:         [usize; 4],
   pub inset:           [usize; 4],
   pub gap:             [usize; 2],
   pub size:            [usize; 2],
   pub min_size:        [usize; 2],
}

/// `gpui::Div`: its size, `SmallVec<[AnyElement; 2]>` children, and boxed
/// `Interactivity`.
#[derive(Debug, Clone, Copy)]
pub struct DivLayout {
   pub size:               usize,
   pub children:           usize,
   pub interactivity:      usize,
   pub interactivity_size: usize,
}

/// `mana_gpui::components::switch::Switch`.
#[derive(Debug, Clone, Copy)]
pub struct SwitchLayout {
   pub size:        usize,
   pub label:       usize,
   pub description: usize,
   pub on_change:   usize,
}

/// `mana_gpui::components::button::Button`.
#[derive(Debug, Clone, Copy)]
pub struct ButtonLayout {
   pub size:     usize,
   pub on_click: usize,
   pub variant:  usize,
   /// The `ButtonSize` field.
   pub scale:    usize,
}

/// What the settings page and accessories build by hand.
#[derive(Debug, Clone, Copy)]
pub struct GpuiLayout {
   pub style:  StyleOffsets,
   pub div:    DivLayout,
   pub switch: SwitchLayout,
   pub button: ButtonLayout,
}

/// Field name to offset in a derived `Debug::fmt`, with nested structs as
/// `outer.inner`, following each field's `&dyn Debug` vtable `depth` deep.
fn debug_offsets(image: &Image<'_>, func: usize, depth: usize) -> HashMap<String, usize> {
   let mut out = HashMap::new();
   let Some(code) = image
      .function_bounds(func)
      .and_then(|(start, end)| image.read(start, end - start))
   else {
      return out;
   };
   for field in phoenix_hook::x86::debug_fields(Abi::of(image), code, func) {
      let Some(name) = image
         .read(field.name, field.name_len)
         .and_then(|name| std::str::from_utf8(name).ok())
      else {
         continue;
      };
      if out.contains_key(name) {
         continue;
      }
      out.insert(name.to_owned(), field.offset);
      let fmt = field
         .vtable
         .and_then(|vtable| image.read_usize(vtable + 0x18));
      if depth > 0
         && let Some(fmt) = fmt.filter(|&fmt| image.is_code(fmt))
      {
         for (inner, offset) in debug_offsets(image, fmt, depth - 1) {
            if let Some(offset) = field.offset.checked_add(offset) {
               out.entry(format!("{name}.{inner}")).or_insert(offset);
            }
         }
      }
   }
   out
}

/// `<StyleRefinement as Debug>::fmt`, the one loader of a `StyleRefinement`
/// literal that prints `text`, `padding` and `gap`.
fn style_offsets(image: &Image<'_>, bytes: usize) -> Found<StyleOffsets> {
   let leas = string_leas(image, &[b"StyleRefinement"]);
   let mut funcs: Vec<usize> = leas
      .values()
      .flatten()
      .filter_map(|&lea| Some(image.function_bounds(lea)?.0))
      .collect();
   funcs.sort_unstable();
   funcs.dedup();
   let found: Vec<HashMap<String, usize>> = funcs
      .into_iter()
      .map(|func| debug_offsets(image, func, 1))
      .filter(|fields| {
         ["text", "padding", "gap"]
            .iter()
            .all(|field| fields.contains_key(*field))
      })
      .collect();
   let [fields] = &found[..] else {
      return Err(format!(
         "StyleRefinement Debug matched {} functions",
         found.len()
      ));
   };
   style_from_fields(fields, bytes)
}

/// [`StyleOffsets`] from `StyleRefinement`'s fields by name, as either
/// architecture's `Debug` reader returns them, checked against its size.
pub(crate) fn style_from_fields(
   fields: &HashMap<String, usize>,
   bytes: usize,
) -> Found<StyleOffsets> {
   let get = |name: &str| -> Found<usize> {
      fields
         .get(name)
         .copied()
         .ok_or_else(|| format!("StyleRefinement Debug has no {name}"))
   };
   let edges = |name: &str| -> Found<[usize; 4]> {
      Ok([
         get(&format!("{name}.top"))?,
         get(&format!("{name}.right"))?,
         get(&format!("{name}.bottom"))?,
         get(&format!("{name}.left"))?,
      ])
   };
   let axes = |name: &str| -> Found<[usize; 2]> {
      Ok([
         get(&format!("{name}.width"))?,
         get(&format!("{name}.height"))?,
      ])
   };
   let style = StyleOffsets {
      bytes,
      font_size: get("text.font_size")?,
      line_height: get("text.line_height")?,
      font_family: get("text.font_family")?,
      text_color: get("text.color")?,
      white_space: get("text.white_space")?,
      flex_shrink: get("flex_shrink")?,
      background: get("background")?,
      border_color: get("border_color")?,
      overflow_x: get("overflow.x")?,
      position: get("position")?,
      flex_wrap: get("flex_wrap")?,
      display: get("display")?,
      align_items: get("align_items")?,
      justify_content: get("justify_content")?,
      border_widths: edges("border_widths")?,
      corner_radii: [
         get("corner_radii.top_left")?,
         get("corner_radii.top_right")?,
         get("corner_radii.bottom_right")?,
         get("corner_radii.bottom_left")?,
      ],
      padding: edges("padding")?,
      inset: edges("inset")?,
      gap: axes("gap")?,
      size: axes("size")?,
      min_size: axes("min_size")?,
   };
   // The widest value the runtime writes into each field: `Option<Hsla>` is
   // 20 bytes, `Option<Background>` 0x48, a `SharedString` 24, a length 8.
   let single = [
      (style.font_size, 8),
      (style.line_height, 8),
      (style.font_family, 24),
      (style.text_color, 20),
      (style.white_space, 1),
      (style.flex_shrink, 8),
      (style.background, 0x48),
      (style.border_color, 20),
      (style.overflow_x, 1),
      (style.position, 1),
      (style.flex_wrap, 1),
      (style.display, 1),
      (style.align_items, 1),
      (style.justify_content, 1),
   ];
   let lengths = [
      &style.border_widths[..],
      &style.corner_radii,
      &style.padding,
      &style.inset,
      &style.gap,
      &style.size,
      &style.min_size,
   ]
   .into_iter()
   .flatten()
   .map(|&at| (at, 8));
   if let Some(end) = single
      .into_iter()
      .chain(lengths)
      .map(|(at, len)| at.saturating_add(len))
      .max()
      && end > bytes
   {
      return Err(format!(
         "StyleRefinement is {bytes:#x} bytes, its fields reach {end:#x}"
      ));
   }
   Ok(style)
}

/// `Div::new` boxes `Interactivity` (`alloc(size, 8)`), copies it in
/// (`mov arg2, size; mov arg0, box; call memcpy`) and stores the box into the
/// `Div` (`mov [sret + d8], box`).
fn div_interactivity(image: &Image<'_>, div_new: usize) -> Found<(usize, usize)> {
   let abi = Abi::of(image);
   let code = image.read(div_new, 0x180).ok_or("Div::new is unmapped")?;
   let size = alloc_size(abi, code)
      .filter(|size| (0x100..0x2000).contains(size))
      .ok_or("Div::new Interactivity allocation not found")?;
   let size_le = u32::try_from(size)
      .map_err(|_| "Div::new Interactivity size overflows")?
      .to_le_bytes();
   let len = [mov_imm32(abi.arg(2)), size_le.to_vec()].concat();
   let copy = code
      .windows(len.len())
      .position(|w| w == len)
      .ok_or("Div::new Interactivity copy not found")?
      + len.len();
   // `mov arg0, box` right before the call. ELF builds call libc's `memcpy`
   // through a GOT slot only the loader fills.
   let (call, boxed) = (copy..copy + 0x20)
      .find_map(|at| {
         let src = mov_from(code.get(at..)?, abi.arg(0))?;
         matches!(code.get(at + 3..at + 5)?, [0xE8, _] | [0xFF, 0x15]).then_some((at + 3, src))
      })
      .ok_or("Div::new memcpy destination not found")?;
   let offset = (call..code.len())
      .find_map(|at| store_disp8(&code[at..], boxed))
      .map(usize::from)
      .ok_or("Div::new Interactivity store not found")?;
   Ok((offset, size))
}

/// The two in-struct fields Discord most often sets right after
/// `Button::new`, `variant` and then `size`, over every call site.
fn button_style_fields(image: &Image<'_>, button_new: usize, size: usize) -> Found<(usize, usize)> {
   let ranked = post_ctor_byte_stores(image, button_new, size);
   // 189, 20 and 4 stores on 2026-11 builds.
   match ranked[..] {
      [(top, variant), (second, scale), ref rest @ ..]
         if top >= 3 * second && rest.first().is_none_or(|&(third, _)| second >= 3 * third) =>
      {
         Ok((variant, scale))
      },
      _ => {
         Err(format!(
            "Button variant and size stores are ambiguous ({:x?})",
            &ranked[..ranked.len().min(4)]
         ))
      },
   }
}

/// Decodes the [`GpuiLayout`] from the builders.
///
/// Each constructor must still set the fields the runtime overwrites and fit
/// its buffer. A layout that does not decode turns the settings page and
/// accessories off.
#[expect(clippy::too_many_lines, reason = "one block per decoded struct")]
pub fn gpui_layouts(image: &Image<'_>, gpui: &Gpui) -> Found<GpuiLayout> {
   let abi = Abi::of(image);
   let check = |name: &str, func: usize, size: usize, fields: &[usize]| -> Found<()> {
      let end = image
         .function_bounds(func)
         .map_or(func + 0x800, |(_, end)| end);
      let code = image
         .read(func, end - func)
         .ok_or_else(|| format!("{name} is unmapped"))?;
      let stores = phoenix_hook::x86::sret_stores(Abi::of(image), code, func);
      let extent = stores.iter().map(|&(at, len)| at + len).max().unwrap_or(0);
      if extent == 0 || extent > size {
         return Err(format!("{name} fills {extent:#x} bytes of {size:#x}"));
      }
      for field in fields {
         if !stores.iter().any(|&(at, _)| at == *field) {
            return Err(format!("{name} no longer sets +{field:#x}"));
         }
      }
      Ok(())
   };

   // `div_child_div`: `lea rcx, [div + children]; ...; call push`, then the
   // return copy `mov r8d, size; call memcpy`.
   let child = image
      .read(gpui.div_child_div, 0x80)
      .ok_or("div_child_div is unmapped")?;
   let children = (0..child.len())
      .find_map(|at| lea_disp8(&child[at..], abi.arg(0)))
      .map(usize::from)
      .ok_or("Div children SmallVec offset not found")?;
   let div_size = fits(
      "Div",
      imm_after(child, &mov_imm32(abi.arg(2))).ok_or("Div size not found")?,
      layout::DIV_CAP,
   )?;
   let (interactivity, interactivity_size) = div_interactivity(image, gpui.div_new)?;
   if children + 0x48 > div_size || interactivity + 8 > div_size {
      return Err("Div children or Interactivity sit past the Div".to_owned());
   }
   check("Div::new", gpui.div_new, div_size, &[interactivity])?;
   let div = DivLayout {
      size: div_size,
      children,
      interactivity,
      interactivity_size,
   };

   let (on_change, switch_size) = rc_store(function_code(
      image,
      gpui.switch_on_change,
      "Switch::on_change",
   )?)
   .ok_or("Switch::on_change Rc store not found")?;
   let switch_size = fits(
      "Switch",
      switch_size.ok_or("Switch::on_change return copy not found")?,
      layout::SCRATCH_CAP,
   )?;
   // ELF builds hand the `Switch`'s leading field struct, copied from offset
   // 0, to a setter of its own, so the offset holds one call down too.
   let setter_field = |func: usize, name: &str| -> Found<usize> {
      let code = function_code(image, func, name)?;
      shared_string_setter(code)
         .or_else(|| {
            let &(_, inner) = calls_in(image, func, code.len()).first()?;
            shared_string_setter(function_code(image, inner, name).ok()?)
         })
         .ok_or_else(|| format!("{name} setter not found"))
   };
   let label = setter_field(gpui.switch_label, "Switch::label")?;
   let description = setter_field(gpui.switch_desc, "Switch::description")?;
   if label.max(description) + 24 > switch_size || on_change + 16 > switch_size {
      return Err("Switch fields sit past the Switch".to_owned());
   }
   check("Switch::new", gpui.switch_new, switch_size, &[on_change])?;
   let switch = SwitchLayout {
      size: switch_size,
      label,
      description,
      on_change,
   };

   let (on_click, button_size) = rc_store(function_code(
      image,
      gpui.button_on_click,
      "Button::on_click",
   )?)
   .ok_or("Button::on_click Rc store not found")?;
   let button_size = fits(
      "Button",
      button_size.ok_or("Button::on_click return copy not found")?,
      layout::SCRATCH_CAP,
   )?;
   if on_click + 16 > button_size {
      return Err("Button on_click sits past the Button".to_owned());
   }
   let (variant, scale) = button_style_fields(image, gpui.button_new, button_size)?;
   check("Button::new", gpui.button_new, button_size, &[
      on_click, variant, scale,
   ])?;
   let button = ButtonLayout {
      size: button_size,
      on_click,
      variant,
      scale,
   };

   // `Interactivity::default` boxes `StyleRefinement` with `mov ecx, size; mov
   // edx, 8; call`.
   let box_alloc = |func: usize, limit: usize| alloc_size(abi, image.read(func, limit)?);
   let (_, div_end) = image
      .function_bounds(gpui.div_new)
      .ok_or("Div::new has no unwind entry")?;
   let style_bytes = callees(image, gpui.div_new, div_end)
      .into_iter()
      .filter_map(|callee| box_alloc(callee, 0xC0))
      .find(|&size| (0x200..=0x400).contains(&size))
      .ok_or("Interactivity::default StyleRefinement allocation not found")?;
   let style = style_offsets(image, style_bytes)?;

   Ok(GpuiLayout {
      style,
      div,
      switch,
      button,
   })
}

/// `MessageRow::render`, and where its message keeps the id.
pub struct MessageRow {
   pub func:          usize,
   pub msg_id_offset: usize,
}

/// The one function calling every helper that formats `message-content-{id}`.
/// Builds since November 2026 keep two copies of the template, one per helper.
pub fn message_row(image: &Image<'_>) -> Found<MessageRow> {
   /// `lea rax, [rcx + disp32]`, the id the template formats.
   const MSG_ID_LEA: Pattern = Pattern::new("48 8D 81 ?? ?? ?? ??");
   let anchors = image.find_bytes(b"\x10message-content-\xc0\x00");
   if anchors.is_empty() {
      return Err("message-content template not found".to_owned());
   }
   let leas = anchors
      .into_iter()
      .flat_map(|anchor| lea_refs(image, anchor))
      .collect::<Vec<_>>();
   if leas.is_empty() {
      return Err("nothing loads the message-content template".to_owned());
   }
   let msg_id_offset = leas
      .iter()
      .find_map(|&lea| {
         let start = lea.saturating_sub(0x60);
         let window = image.read(start, lea - start)?;
         MSG_ID_LEA
            .find_in(window)
            .into_iter()
            .rev()
            .filter_map(|at| imm32(image, start + at + 3))
            .find(|disp| (0x100..=0x2000).contains(disp))
      })
      .ok_or("message id load not found near the template")?;
   let helpers = leas
      .iter()
      .filter_map(|&lea| Some(image.function_bounds(lea)?.0))
      .collect::<HashSet<_>>();
   let common = helpers
      .iter()
      .map(|&helper| callers(image, helper))
      .filter(|parents| !parents.is_empty())
      .reduce(|common, parents| common.intersection(&parents).copied().collect())
      .unwrap_or_default();
   let func = one(
      "MessageRow::render",
      &common.into_iter().collect::<Vec<_>>(),
   )?;
   if !prologues(image).row.at(image, func) {
      return Err("MessageRow::render prologue changed".to_owned());
   }
   Ok(MessageRow {
      func,
      msg_id_offset,
   })
}

/// `build_message_body`, and where its message and row colors sit.
pub struct MessageBody {
   pub func:              usize,
   pub msg_id_offset:     usize,
   pub row_colors_offset: usize,
}

/// The function `MessageRow::render` calls that loads the `message-edited-`
/// template.
pub fn message_body(image: &Image<'_>, render_row: usize) -> Found<MessageBody> {
   // `mov rcx, [arg0 + disp32]`, loading the message id.
   let mut msg_id_mov = rm_disp8(0x8B, Register::RCX, Abi::of(image).arg(0));
   msg_id_mov[2] += 0x40;
   let anchor = one(
      "message-edited template",
      &image.find_bytes(b"\x0fmessage-edited-\xc0\x00"),
   )?;
   let mut found = lea_refs(image, anchor)
      .into_iter()
      .filter_map(|lea| Some(image.function_bounds(lea)?.0))
      .filter(|&func| prologues(image).row.at(image, func))
      .filter(|&func| callers(image, func).contains(&render_row))
      .collect::<Vec<_>>();
   found.sort_unstable();
   found.dedup();
   let func = one("message body builder", &found)?;
   let head = image
      .read(func, 0x120)
      .ok_or("message body builder is unmapped")?;
   let msg_id_offset = head[..0x100]
      .windows(3)
      .enumerate()
      .filter(|(_, w)| *w == msg_id_mov)
      .filter_map(|(at, _)| imm32(image, func + at + 3))
      .find(|disp| (0x100..=0x800).contains(disp))
      .ok_or("message body id load not found")?;
   // Three `mov r32, imm32` with consecutive `RowColors` field offsets
   // `+0xc`, `+0x8` and `+0x4`. Newer builds take `lea r64, [arg1 + RowColors]`
   // and pick the fields with `push imm8; pop`.
   let arg1 = number(Abi::of(image).arg(1));
   let row_colors_offset = head
      .windows(15)
      .enumerate()
      .filter(|(_, w)| [w[0], w[5], w[10]].iter().all(|&op| op & 0xF8 == 0xB8))
      .find_map(|(at, _)| {
         let (a, c, d) = (
            imm32(image, func + at + 1)?,
            imm32(image, func + at + 6)?,
            imm32(image, func + at + 11)?,
         );
         (a == c + 4 && c == d + 4 && (0x100..=0x400).contains(&d)).then(|| d - 4)
      })
      .or_else(|| {
         mem_operands(head, 0x8D)
            .find(|&(_, base, disp)| base == arg1 && (0x100..=0x400).contains(&disp))
            .map(|(_, _, disp)| disp)
      })
      .ok_or("message body row colors not found")?;
   Ok(MessageBody {
      func,
      msg_id_offset,
      row_colors_offset,
   })
}

/// A hex displacement as `disasm` prints it, like `1F8h`.
fn hex_disp(text: &str) -> Option<usize> {
   usize::from_str_radix(text.strip_suffix('h')?, 16).ok()
}

/// What the island in front of a hooked call hands its detour after the
/// callee's own arguments, and what frame slots are read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
   /// A register of the caller: `rbp`, which Win64 frames address their slots
   /// from, or a callee-saved register holding a pointer the detour reads.
   Reg(Register),
   /// The caller's `rsp` at the call. System V frames address their slots
   /// from it, keeping no frame pointer.
   Stack,
}

impl Frame {
   /// What `abi` builds address frame slots from.
   #[must_use]
   pub const fn of(abi: Abi) -> Self {
      match abi {
         Abi::Win64 => Self::Reg(Register::RBP),
         Abi::SysV => Self::Stack,
      }
   }

   /// The island code putting this into `arg`: `mov arg, reg`, or `lea arg,
   /// [rsp+8]`, since the call into the island pushed a return address.
   #[must_use]
   pub fn island(self, arg: Register) -> Vec<u8> {
      let (to, to_ext) = reg(arg);
      match self {
         Self::Reg(from) => {
            let (from, from_ext) = reg(from);
            vec![0x48 | from_ext << 2 | to_ext, 0x89, 0xC0 | from << 3 | to]
         },
         Self::Stack => vec![0x48 | to_ext << 2, 0x8D, 0x44 | to << 3, 0x24, 0x08],
      }
   }

   /// The register as `disasm` prints it.
   fn base(self) -> String {
      match self {
         Self::Reg(register) => reg_name(register),
         Self::Stack => "rsp".to_owned(),
      }
   }
}

impl std::fmt::Display for Frame {
   fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
      f.write_str(&self.base())
   }
}

/// A register as `disasm` prints it, like `r8`.
fn reg_name(register: Register) -> String {
   format!("{register:?}").to_lowercase()
}

/// `[base]` or `[base+disp]` as `disasm` prints a frame operand, like
/// `[rbp+0DB0h]` or `[rsp+8]`.
fn slot_of(operand: &str, base: &str) -> Option<usize> {
   let rest = operand
      .strip_prefix('[')?
      .strip_suffix(']')?
      .strip_prefix(base)?;
   if rest.is_empty() {
      return Some(0);
   }
   let disp = rest.strip_prefix('+')?;
   disp.strip_suffix('h').map_or_else(
      || disp.parse().ok(),
      |hex| usize::from_str_radix(hex, 16).ok(),
   )
}

/// The callee of the call `line` decodes: `call rel32`, or through a pointer
/// slot as ELF builds call.
fn callee(image: &Image<'_>, line: &phoenix_hook::x86::Line) -> Option<usize> {
   if !line.text.starts_with("call ") {
      return None;
   }
   call_target(image, image.read(line.addr, line.len)?, line.addr).map(|(_, callee)| callee)
}

/// Whether [`Image::hook_call`] can point the call `line` decodes elsewhere.
fn hookable(image: &Image<'_>, line: &phoenix_hook::x86::Line) -> bool {
   match image.read(line.addr, 2) {
      Some([0xE8, _]) => line.len == 5,
      Some([0xFF, 0x15]) => line.len == 6,
      _ => false,
   }
}

/// The operand an `AnyElement` is copied from for the call at `at`, passed by
/// reference in `arg`: `movups xmm0, [source]` (or `movaps`), then `movaps
/// [arg], xmm0` and `movaps [arg+10h], xmm1`.
fn element_source<'a>(
   lines: &'a [phoenix_hook::x86::Line],
   at: usize,
   arg: &str,
) -> Option<&'a str> {
   let args = &lines[at.saturating_sub(12)..at];
   let copies = args
      .iter()
      .any(|line| line.text == format!("movaps [{arg}],xmm0"))
      && args
         .iter()
         .any(|line| line.text == format!("movaps [{arg}+10h],xmm1"));
   copies
      .then(|| {
         args.iter().rev().find_map(|line| {
            line
               .text
               .strip_prefix("movups xmm0,")
               .or_else(|| line.text.strip_prefix("movaps xmm0,"))
         })
      })
      .flatten()
}

/// The call before `at` that last wrote `slot`, as its hidden return pointer
/// (`lea sret, [slot]`, or `mov sret, rsp` for `[rsp]`). A store to `slot`
/// on the way means no call did.
fn returned_into(
   lines: &[phoenix_hook::x86::Line],
   at: usize,
   slot: &str,
   sret: &str,
) -> Option<usize> {
   let lea = format!("lea {sret},");
   let from_rsp = format!("mov {sret},rsp");
   for call in (0..at).rev() {
      if written(&lines[call]) == Some(slot) {
         return None;
      }
      if !lines[call].text.starts_with("call ") {
         continue;
      }
      let out = lines[call.saturating_sub(8)..call]
         .iter()
         .rev()
         .find_map(|line| {
            if let Some(operand) = line.text.strip_prefix(&lea) {
               return Some(Some(operand));
            }
            if line.text == from_rsp {
               return Some(Some("[rsp]"));
            }
            (written(line) == Some(sret)).then_some(None)
         });
      if out.flatten() == Some(slot) {
         return Some(call);
      }
   }
   None
}

/// The member list's row renderer, and where its name joins the row.
pub struct MemberRow {
   pub func: usize,
   pub name: MemberName,
}

/// The calls adding a member's name to its row, one per branch.
///
/// They are `Div::child` by value, `(sret, div, &name) -> Div`. The row's
/// closure keeps its environment pointer in a frame slot on Win64 and in a
/// callee-saved register on System V, and the environment a pointer to the
/// member's user id.
#[derive(Clone, Debug)]
pub struct MemberName {
   pub sites:        Vec<usize>,
   /// What the islands hand the detour.
   pub frame:        Frame,
   /// `[frame+closure_slot]` holds the environment pointer at the calls;
   /// without one, `frame` is the environment pointer itself.
   pub closure_slot: Option<usize>,
   /// `[environment+env]` holds the pointer to the user id.
   pub env:          usize,
}

/// The member row hands its name to a closure building the name's row: the
/// closure's environment holds the name and, at `+env`, a pointer to the id
/// slot (`lea rax, [slot]; mov [env_arg+env], rax`). The closure's function
/// saves the environment pointer (`mov [rbp+closure_slot], r8` on Win64, `mov
/// rbx, rdx` on System V) and adds the name with calls taking a freshly copied
/// `AnyElement`: the one each branch's name row was returned into.
fn member_name(
   image: &Image<'_>,
   lines: &[phoenix_hook::x86::Line],
   slot: &str,
) -> Option<MemberName> {
   let abi = Abi::of(image);
   let env_arg = reg_name(abi.arg(2));
   let env_store = format!("mov [{env_arg}+");
   let (store, env) = lines.iter().enumerate().find_map(|(at, line)| {
      let field = line.text.strip_prefix(&env_store)?.strip_suffix("],rax")?;
      (lines.get(at.checked_sub(1)?)?.text == format!("lea rax,{slot}"))
         .then_some((at, hex_disp(field)?))
   })?;
   let closure = lines[store..]
      .iter()
      .take(40)
      .find_map(|line| callee(image, line))?;
   let code = function_code(image, closure, "member name row").ok()?;
   let body = phoenix_hook::x86::disasm(code, closure, usize::MAX);
   let base = Frame::of(abi).base();
   let saved_reg = |line: &phoenix_hook::x86::Line| {
      let to = line
         .text
         .strip_prefix("mov ")?
         .strip_suffix(&format!(",{env_arg}"))?;
      [
         Register::RBX,
         Register::R12,
         Register::R13,
         Register::R14,
         Register::R15,
      ]
      .into_iter()
      .find(|&register| reg_name(register) == to)
   };
   let (saved, frame, closure_slot) = body.iter().enumerate().take(16).find_map(|(at, line)| {
      if let Some(register) = saved_reg(line) {
         return Some((at, Frame::Reg(register), None));
      }
      let operand = line
         .text
         .strip_prefix("mov ")?
         .strip_suffix(&format!(",{env_arg}"))?;
      Some((at, Frame::of(abi), Some(slot_of(operand, &base)?)))
   })?;
   let sret = reg_name(abi.arg(0));
   let calls = body
      .iter()
      .enumerate()
      .filter_map(|(at, line)| {
         let target = callee(image, line)?;
         Some((at, target, element_source(&body, at, &env_arg)?))
      })
      .collect::<Vec<_>>();
   let child = calls.first()?.1;
   // Styled names are first wrapped with the name taken from the
   // environment; the row then gets the element the name row was returned
   // into, in every branch. Only those calls add to the row itself. ELF builds
   // keep a copy of `Div::child` per caller crate.
   let sites = calls
      .iter()
      .filter(|&&(at, target, source)| {
         (target == child || same_code(image, target, child))
            && hookable(image, &body[at])
            && returned_into(&body, at, source, &sret).is_some()
      })
      .map(|&(at, ..)| at)
      .collect::<Vec<_>>();
   // A register handed over must still hold the environment at every site.
   if let Frame::Reg(register) = frame
      && closure_slot.is_none()
   {
      let name = reg_name(register);
      let last = *sites.last()?;
      let clobbered = body[saved + 1..last]
         .iter()
         .any(|line| written(line) == Some(name.as_str()) || line.text == format!("pop {name}"));
      if clobbered {
         return None;
      }
   }
   (!sites.is_empty()).then(|| {
      MemberName {
         sites: sites.into_iter().map(|at| body[at].addr).collect(),
         frame,
         closure_slot,
         env,
      }
   })
}

/// The function formatting `member-{}-{}`, a member row's element id from the
/// member's user id, and the calls in it adding the member's name to its row.
pub fn member_row(image: &Image<'_>) -> Found<MemberRow> {
   let anchors = image.find_bytes(b"member-{}-{}");
   let mut funcs = anchors
      .iter()
      .flat_map(|&anchor| lea_refs(image, anchor))
      .filter_map(|lea| Some(image.function_bounds(lea)?.0))
      .collect::<Vec<_>>();
   funcs.sort_unstable();
   funcs.dedup();
   let func = one("member row", &funcs)?;
   if !prologues(image).row.at(image, func) {
      return Err("member row prologue changed".to_owned());
   }
   let lines =
      phoenix_hook::x86::disasm(function_code(image, func, "member row")?, func, usize::MAX);
   // The id is formatted from a frame slot, the fourth argument (`mov r9,
   // [rbp+slot]` on Win64) of the call taking the template.
   let abi = Abi::of(image);
   let id_load = format!("mov {},", reg_name(abi.arg(3)));
   let base = Frame::of(abi).base();
   let template = lines
      .iter()
      .position(|line| line.target.is_some_and(|target| anchors.contains(&target)))
      .ok_or("member row template load not found")?;
   let slot = lines[template.saturating_sub(12)..template]
      .iter()
      .rev()
      .find_map(|line| line.text.strip_prefix(&id_load))
      .filter(|slot| slot_of(slot, &base).is_some())
      .ok_or("member row id slot not found")?;
   Ok(MemberRow {
      func,
      name: member_name(image, &lines, slot).ok_or("member name row not decoded")?,
   })
}

/// The DM list's row renderer, and where its recipient's name joins the row.
///
/// The row adds the box built around the name (`dm-name-bounds`) to a flex row
/// with `Div::child` by value, `(sret, div, &name) -> Div`. One-to-one DMs keep
/// their recipient as an `Option<UserId>` copied into two frame slots, the
/// one the row's presence dot (`dm-status`) is keyed by; group DMs have none.
#[derive(Clone, Debug)]
pub struct DmRow {
   pub func:  usize,
   /// The call adding the name box.
   pub site:  usize,
   /// What the island hands the detour, which the slots are read from.
   pub frame: Frame,
   /// `[frame+some]` is the recipient's `Option` tag, zero for none.
   pub some:  usize,
   /// `[frame+user]` is the recipient's user id when `some` is set.
   pub user:  usize,
}

/// The single function loading `text`, as it is laid out in the string pool.
fn loader_of(image: &Image<'_>, what: &str, text: &[u8]) -> Found<usize> {
   let mut funcs = image
      .find_bytes(text)
      .iter()
      .flat_map(|&anchor| lea_refs(image, anchor))
      .filter_map(|lea| Some(image.function_bounds(lea)?.0))
      .collect::<Vec<_>>();
   funcs.sort_unstable();
   funcs.dedup();
   one(what, &funcs)
}

/// The `Option<UserId>` slots read after `anchor`'s `lea`: the id is the first
/// `mov rax, [frame+user]` after it, and the row copies the option there from
/// its props, `mov rax, [r]; mov [frame+some], rax; mov rax, [r+8]; mov
/// [frame+user], rax`, then tests `cmp qword ptr [frame+some], 0`.
fn option_user_slots(
   lines: &[phoenix_hook::x86::Line],
   anchor: usize,
   base: &str,
) -> Option<(usize, usize)> {
   let user = lines
      .get(anchor..)?
      .iter()
      .take(8)
      .find_map(|line| slot_of(line.text.strip_prefix("mov rax,")?, base))?;
   let stored = |line: &phoenix_hook::x86::Line| {
      slot_of(line.text.strip_prefix("mov ")?.strip_suffix(",rax")?, base)
   };
   let at = lines.iter().position(|line| stored(line) == Some(user))?;
   let [load_some, store_some, load_user] = lines.get(at.checked_sub(3)?..at)? else {
      return None;
   };
   let props = load_user
      .text
      .strip_prefix("mov rax,[")?
      .strip_suffix("+8]")?;
   let some = stored(store_some)?;
   let tested = lines.iter().any(|line| {
      line
         .text
         .strip_prefix("cmp qword ptr ")
         .and_then(|rest| rest.strip_suffix(",0"))
         .and_then(|operand| slot_of(operand, base))
         == Some(some)
   });
   (load_some.text == format!("mov rax,[{props}]") && tested).then_some((some, user))
}

/// The DM row: the one caller of the function building the box around the
/// recipient's name, and the call adding that box's element to the row.
///
/// ELF builds inline the box into the row, whose element is then the one the
/// box's last call returns into.
pub fn dm_row(image: &Image<'_>) -> Found<DmRow> {
   let abi = Abi::of(image);
   let frame = Frame::of(abi);
   let base = frame.base();
   let element_arg = reg_name(abi.arg(2));
   let name_box = loader_of(image, "DM name box", b"dm-name-bounds")?;
   let status = image.find_bytes(b"dm-status");
   let loads_status = |lines: &[phoenix_hook::x86::Line]| {
      lines
         .iter()
         .position(|line| line.target.is_some_and(|target| status.contains(&target)))
   };
   let box_lines = phoenix_hook::x86::disasm(
      function_code(image, name_box, "DM name box")?,
      name_box,
      usize::MAX,
   );
   let (func, lines, call) = if loads_status(&box_lines).is_some() {
      let names = image.find_bytes(b"dm-name-bounds");
      let built = box_lines
         .iter()
         .position(|line| line.target.is_some_and(|target| names.contains(&target)))
         .ok_or("DM name box load not found")?;
      let sret = reg_name(abi.arg(0));
      let at = (built + 1..box_lines.len().min(built + 400))
         .find(|&at| {
            callee(image, &box_lines[at]).is_some()
               && element_source(&box_lines, at, &element_arg).is_some_and(|source| {
                  returned_into(&box_lines, at, source, &sret).is_some_and(|by| by > built)
               })
         })
         .ok_or("DM name child call not found")?;
      let child = callee(image, &box_lines[at]).ok_or("DM name child call is indirect")?;
      let adds_name = box_lines[built..at].iter().any(|line| {
         callee(image, line).is_some_and(|other| other == child || same_code(image, other, child))
      });
      if !adds_name {
         return Err("DM name child is not Div::child".to_owned());
      }
      (name_box, box_lines, at)
   } else {
      let funcs = callers(image, name_box).into_iter().collect::<Vec<_>>();
      let func = one("DM row", &funcs)?;
      let lines =
         phoenix_hook::x86::disasm(function_code(image, func, "DM row")?, func, usize::MAX);
      let built = lines
         .iter()
         .position(|line| callee(image, line) == Some(name_box))
         .ok_or("DM name box call not found")?;
      let box_arg = format!("lea {},", reg_name(abi.arg(0)));
      let out = lines[built.saturating_sub(8)..built]
         .iter()
         .rev()
         .find_map(|line| line.text.strip_prefix(&box_arg))
         .filter(|out| slot_of(out, &base).is_some())
         .ok_or("DM name box result not found")?;
      let at = (built + 1..lines.len().min(built + 80))
         .filter(|&at| lines[at].text.starts_with("call "))
         .find(|&at| element_source(&lines, at, &element_arg) == Some(out))
         .ok_or("DM name child call not found")?;
      let child = callee(image, &lines[at]).ok_or("DM name child call is indirect")?;
      // The name box adds the name itself with the same `Div::child`.
      if !box_lines
         .iter()
         .any(|line| callee(image, line) == Some(child))
      {
         return Err("DM name child is not Div::child".to_owned());
      }
      (func, lines, at)
   };
   if !hookable(image, &lines[call]) {
      return Err("DM name child call cannot be hooked".to_owned());
   }
   let anchor = loads_status(&lines).ok_or("DM status id not found")?;
   let (some, user) =
      option_user_slots(&lines, anchor, &base).ok_or("DM recipient slots not decoded")?;
   Ok(DmRow {
      func,
      site: lines[call].addr,
      frame,
      some,
      user,
   })
}

/// The message header's node API, and where each header row adds the author's
/// name.
///
/// Headers are not GPUI `Div`s but Discord's own `Rc<Node>` handles: every
/// builder call takes the handle and returns it, cloned first when shared.
#[derive(Clone, Debug)]
pub struct MessageHeader {
   /// The calls adding the name, one per display mode.
   pub sites:      Vec<HeaderSite>,
   /// What the islands hand the detours, which the slots are read from.
   pub frame:      Frame,
   /// `child_node(node, child) -> node`, adding a node as a child.
   pub child_node: usize,
   pub svg:        NodeSvg,
}

/// Discord's node SVG leaf, drawn from an asset path.
#[derive(Clone, Copy, Debug)]
pub struct NodeSvg {
   /// `svg_new(out, path ptr, path len, size: f32)`.
   pub new:   usize,
   /// `push_svg(node, &leaf) -> node`, moving the leaf into a boxed child.
   pub push:  usize,
   /// The leaf's color tag (`1` when set); the packed color follows it.
   pub color: usize,
   /// The leaf's size, which `push_svg` copies.
   pub size:  usize,
}

/// A `call child_node` adding the name wrapper to a header row.
#[derive(Clone, Copy, Debug)]
pub struct HeaderSite {
   pub site: usize,
   /// `[frame+slot]` holds the message id at the call.
   pub slot: usize,
}

/// The app tag's element id template, loaded by the two header row builders.
const HEADER_APP_TAG: &[u8] = b"\x0fheader-app-tag-\xc0\x00";
/// The most header row builders a build has had.
pub const MAX_HEADER_BUILDERS: usize = 3;
/// A bundled icon drawn as a node SVG leaf.
const NODE_SVG: &[u8] = b"features_ui/partner.svg";

/// `child_node`'s head: the child handle wrapped as a node child, tag 0.
const fn child_node_head(abi: Abi) -> [u8; 16] {
   match abi {
      // `sub rsp,48h; lea rax,[rsp+28h]; and dword ptr [rax],0; mov
      // [rax+8],rdx`
      Abi::Win64 => {
         [
            0x48, 0x83, 0xEC, 0x48, 0x48, 0x8D, 0x44, 0x24, 0x28, 0x83, 0x20, 0x00, 0x48, 0x89,
            0x50, 0x08,
         ]
      },
      // `sub rsp,28h; lea rax,[rsp+8]; and dword ptr [rax],0; mov [rax+8],rsi`
      Abi::SysV => {
         [
            0x48, 0x83, 0xEC, 0x28, 0x48, 0x8D, 0x44, 0x24, 0x08, 0x83, 0x20, 0x00, 0x48, 0x89,
            0x70, 0x08,
         ]
      },
   }
}

/// The register or memory operand an instruction writes, as `disasm` prints
/// it: `mov rsi,rax` writes `rsi`.
fn written(line: &phoenix_hook::x86::Line) -> Option<&str> {
   let (_, operands) = line.text.split_once(' ')?;
   Some(operands.split_once(',')?.0)
}

/// Locates [`MessageHeader`]. `body` is [`message_body`]'s function, whose
/// auto-moderation avatar fallback draws a node SVG leaf.
///
/// Every row builder adds the name with `mov child,from; call child_node`,
/// where `from`, a register or a frame slot, holds what one function called
/// only by the builders returned. The message id slot is the format
/// argument of the app tag's element id.
pub fn message_header(image: &Image<'_>, body: usize) -> Found<MessageHeader> {
   let anchor = one("header-app-tag template", &image.find_bytes(HEADER_APP_TAG))?;
   let mut builders = lea_refs(image, anchor)
      .into_iter()
      .filter_map(|lea| Some(image.function_bounds(lea)?.0))
      .collect::<Vec<_>>();
   builders.sort_unstable();
   builders.dedup();
   // Two on Win64 and on Linux 2233; older Linux builds kept a second copy of
   // one, taking another argument.
   if !(2..=MAX_HEADER_BUILDERS).contains(&builders.len()) {
      return Err(format!(
         "the app tag template is loaded by {} functions",
         builders.len()
      ));
   }
   let decoded = builders
      .iter()
      .map(|&func| header_builder(image, func, anchor))
      .collect::<Found<Vec<_>>>()?;
   let all = builders.iter().copied().collect::<HashSet<_>>();
   let mut sites = Vec::new();
   let mut child_node = None;
   for (slot, adds) in &decoded {
      let names = adds
         .iter()
         .filter(|&&(_, _, producer)| {
            let by = callers(image, producer);
            !by.is_empty() && by.is_subset(&all)
         })
         .copied()
         .collect::<Vec<_>>();
      let (site, callee, _) = one("header name add", &names)?;
      if child_node.is_some_and(|known| known != callee && !same_code(image, known, callee)) {
         return Err("the header builders add children differently".to_owned());
      }
      child_node = Some(callee);
      sites.push(HeaderSite { site, slot: *slot });
   }
   let child_node = child_node.ok_or("header child_node not found")?;
   Ok(MessageHeader {
      sites,
      frame: Frame::of(Abi::of(image)),
      child_node,
      svg: node_svg(image, body)?,
   })
}

/// `(site, callee, producer)`: a `call child_node` adding what `producer`
/// returned.
type ChildAdd = (usize, usize, usize);

/// A header builder's message id slot, and every call adding a call's result
/// with `child_node`.
fn header_builder(image: &Image<'_>, func: usize, anchor: usize) -> Found<(usize, Vec<ChildAdd>)> {
   let abi = Abi::of(image);
   let base = Frame::of(abi).base();
   let head = child_node_head(abi);
   let is_child_node = |func: usize| image.read(func, 16) == Some(&head[..]);
   let child_arg = format!("mov {},", reg_name(abi.arg(1)));
   let lines = phoenix_hook::x86::disasm(
      function_code(image, func, "header builder")?,
      func,
      usize::MAX,
   );
   let template = lines
      .iter()
      .position(|line| line.target == Some(anchor))
      .ok_or("header app tag load not decoded")?;
   let slot = lines[template.saturating_sub(8)..template]
      .iter()
      .rev()
      .find_map(|line| slot_of(line.text.strip_prefix("lea rax,")?, &base))
      .ok_or("header message id slot not found")?;
   let mut adds = Vec::new();
   for at in 1..lines.len() {
      let line = &lines[at];
      let Some(callee) = callee(image, line) else {
         continue;
      };
      let Some(from) = lines[at - 1].text.strip_prefix(&child_arg) else {
         continue;
      };
      if !hookable(image, line) || !is_child_node(callee) {
         continue;
      }
      let Some(set) = (0..at - 1)
         .rev()
         .find(|&j| written(&lines[j]) == Some(from))
      else {
         continue;
      };
      if lines[set].text != format!("mov {from},rax") {
         continue;
      }
      // Calls with stacked arguments pop them before the result is kept.
      let producer = lines[..set]
         .iter()
         .rev()
         .find(|line| line.text != "nop" && !line.text.starts_with("add rsp,"))
         .and_then(|line| self::callee(image, line));
      if let Some(producer) = producer {
         adds.push((line.addr, callee, producer));
      }
   }
   Ok((slot, adds))
}

fn node_svg(image: &Image<'_>, body: usize) -> Found<NodeSvg> {
   let abi = Abi::of(image);
   let base = Frame::of(abi).base();
   let mut svg_news = image
      .find_bytes(NODE_SVG)
      .into_iter()
      .flat_map(|path| lea_refs(image, path))
      .filter_map(|lea| {
         let code = image.read(lea, 0x40)?;
         phoenix_hook::x86::disasm(code, lea, 12)
            .iter()
            .find(|line| line.text.starts_with("call "))
            .and_then(|line| callee(image, line))
      })
      .collect::<Vec<_>>();
   // The leaf keeps the size, the first float argument (`xmm3` on Win64,
   // `xmm0` on System V), and the path at `out+10h`; the bundled icon is drawn
   // as a GPUI `Svg` elsewhere too.
   let out_path = format!("add {},10h", reg_name(abi.arg(0)));
   let keeps_size = |line: &phoenix_hook::x86::Line| {
      match abi {
         Abi::Win64 => line.text == "movaps xmm6,xmm3",
         Abi::SysV => line.text.starts_with("movss [rsp+") && line.text.ends_with(",xmm0"),
      }
   };
   svg_news.retain(|&func| {
      image.read(func, 0x30).is_some_and(|head| {
         let head = phoenix_hook::x86::disasm(head, func, 8);
         head.iter().any(keeps_size) && head.iter().any(|line| line.text == out_path)
      })
   });
   svg_news.sort_unstable();
   svg_news.dedup();
   let svg_new = one("node svg_new", &svg_news)?;

   // In the body: `lea out,[frame+leaf]; ...; call svg_new`, then
   // `mov dword ptr [frame+color],1` and the call pushing the leaf.
   let leaf_arg = format!("lea {},", reg_name(abi.arg(0)));
   let lines = phoenix_hook::x86::disasm(
      function_code(image, body, "message body")?,
      body,
      usize::MAX,
   );
   let (push_svg, svg_color) = lines
      .iter()
      .enumerate()
      .filter(|(_, line)| callee(image, line) == Some(svg_new))
      .find_map(|(at, _)| {
         let leaf = lines[at.saturating_sub(8)..at]
            .iter()
            .rev()
            .find_map(|line| slot_of(line.text.strip_prefix(&leaf_arg)?, &base))?;
         let after = &lines[at + 1..(at + 24).min(lines.len())];
         let push = after.iter().find(|line| line.text.starts_with("call "))?;
         let tag = after
            .iter()
            .take_while(|line| line.addr < push.addr)
            .find_map(|line| {
               slot_of(
                  line
                     .text
                     .strip_prefix("mov dword ptr ")?
                     .strip_suffix(",1")?,
                  &base,
               )
            })?;
         Some((callee(image, push)?, tag.checked_sub(leaf)?))
      })
      .ok_or("node SVG leaf push not found in the message body")?;
   if !(4..0x30).contains(&svg_color) {
      return Err(format!("node SVG color at {svg_color:#x}"));
   }
   // `push_svg` boxes the leaf first: `push size; pop size_arg` before the
   // allocation, which takes the alignment first.
   let size_pop = format!("pop {}", reg_name(abi.arg(1)));
   let convert = calls_in(image, push_svg, 0x40)
      .first()
      .ok_or("push_svg calls nothing")?
      .1;
   let code = image
      .read(convert, 0x80)
      .ok_or("the leaf box is unmapped")?;
   let head = phoenix_hook::x86::disasm(code, convert, 32);
   let leaf_size = head
      .windows(2)
      .find_map(|pair| {
         (pair[1].text == size_pop)
            .then(|| hex_disp(pair[0].text.strip_prefix("push ")?))
            .flatten()
      })
      .filter(|&size| svg_color + 8 <= size && size <= 0x100)
      .ok_or("node SVG leaf size not found")?;
   Ok(NodeSvg {
      new:   svg_new,
      push:  push_svg,
      color: svg_color,
      size:  leaf_size,
   })
}

/// A bundled icon every build so far draws with `svg().path(..)`, which the
/// SVG builders and the asset loader are found from.
const BUNDLED_SVG: &[u8] = b"features_ui/platform-spotify.svg";

/// GPUI's `Svg` element builders.
#[derive(Clone, Copy, Debug)]
pub struct SvgBuilders {
   /// `svg()`: `(sret, &caller_location) -> Svg`.
   pub new:       usize,
   /// `Svg::path`: `(sret, svg, &'static str ptr, len) -> Svg`.
   pub path:      usize,
   /// `Svg::into_any_element`: `(sret, svg) -> AnyElement`.
   pub into_any:  usize,
   /// `Svg`'s size, which `into_any` moves.
   pub size:      usize,
   /// Where `Svg` keeps its boxed `StyleRefinement`.
   pub style_box: usize,
}

/// The function drawing [`BUNDLED_SVG`]: `svg()` is the call before the path
/// is loaded, `path` the call after it, and `into_any` the call writing the
/// function's `sret`. A style setter on the way reads the style box.
pub fn svg_builders(image: &Image<'_>) -> Found<SvgBuilders> {
   let abi = Abi::of(image);
   let anchors = image.find_bytes(BUNDLED_SVG);
   let (func, lea) = anchors
      .iter()
      .flat_map(|&anchor| lea_refs(image, anchor))
      .find_map(|lea| Some((image.function_bounds(lea)?.0, lea)))
      .ok_or("no code draws the bundled SVG")?;
   let lines = phoenix_hook::x86::disasm(function_code(image, func, "SVG icon")?, func, usize::MAX);
   let at = lines
      .iter()
      .position(|line| line.addr == lea)
      .ok_or("SVG path load not decoded")?;
   let call_target = |line: &phoenix_hook::x86::Line| callee(image, line);
   let new = lines[..at]
      .iter()
      .rev()
      .find_map(call_target)
      .ok_or("svg() call not found")?;
   let after = lines[at..]
      .iter()
      .filter_map(call_target)
      .collect::<Vec<_>>();
   let path = *after.first().ok_or("Svg::path call not found")?;
   // `mov rax, [svg+box]` at the head of a style setter taking the `Svg`.
   let box_load = format!("mov rax,[{}+", reg_name(abi.arg(1)));
   let style_box = after
      .iter()
      .take(4)
      .find_map(|&callee| {
         let head = image.read(callee, 0x20)?;
         phoenix_hook::x86::disasm(head, callee, 6)
            .iter()
            .find_map(|line| hex_disp(line.text.strip_prefix(&box_load)?.strip_suffix(']')?))
      })
      .filter(|&offset| (0x100..0x800).contains(&offset))
      .ok_or("Svg style box not found")?;
   // The last call, whose result the function returns.
   let ret = lines
      .iter()
      .position(|line| line.text == "ret")
      .ok_or("SVG icon return not found")?;
   let into_any = lines[at..ret]
      .iter()
      .rev()
      .find_map(call_target)
      .ok_or("Svg::into_any call not found")?;
   // It moves the `Svg` with a copy of its size, the third argument.
   let size_load = format!("mov {},", reg_name(abi.arg(2).full_register32()));
   let size = image
      .read(into_any, 0x30)
      .map(|head| phoenix_hook::x86::disasm(head, into_any, 8))
      .and_then(|head| {
         head
            .iter()
            .find_map(|line| hex_disp(line.text.strip_prefix(&size_load)?))
      })
      .filter(|&size| (style_box..0x1000).contains(&size))
      .ok_or("Svg size not found")?;
   Ok(SvgBuilders {
      new,
      path,
      into_any,
      size,
      style_box,
   })
}

/// Discord's asset loader, `(sret, path ptr, path len) -> Option<Vec<u8>>`.
///
/// It ends at the bundle holding [`BUNDLED_SVG`]. The bundle sorts records
/// `{path ptr, path len, index}`; its descriptor points at their start, and the
/// loader loads the descriptor. ELF images also hold the descriptor's address
/// as relocation addends, which no code loads.
pub fn asset_load(image: &Image<'_>) -> Found<usize> {
   let record = |slot: usize| {
      let ptr = image.read_usize(slot)?;
      let len = image.read_usize(slot + 8)?;
      let index = image.read_usize(slot + 16)?;
      let path = image
         .read(ptr, len)
         .filter(|_| (3..=200).contains(&len) && index < 100_000)?;
      path
         .iter()
         .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(byte))
         .then_some(())
   };
   let mut slots = image
      .find_bytes(BUNDLED_SVG)
      .into_iter()
      .flat_map(|string| image.pointers_to(string))
      .filter(|&slot| {
         image.read_usize(slot + 8) == Some(BUNDLED_SVG.len()) && record(slot).is_some()
      })
      .collect::<Vec<_>>();
   slots.dedup();
   let mut start = one("bundled SVG record", &slots)?;
   while let Some(previous) = start.checked_sub(24)
      && record(previous).is_some()
   {
      start = previous;
   }
   let descriptors = image
      .pointers_to(start)
      .into_iter()
      .filter(|&descriptor| !lea_refs(image, descriptor).is_empty())
      .collect::<Vec<_>>();
   let descriptor = one("asset bundle", &descriptors)?;
   let mut loaders = lea_refs(image, descriptor)
      .into_iter()
      .filter_map(|lea| Some(image.function_bounds(lea)?.0))
      .filter(|&func| prologues(image).asset_load.at(image, func))
      .collect::<Vec<_>>();
   loaders.sort_unstable();
   loaders.dedup();
   one("asset loader", &loaders)
}

/// The `jne rel32` that leaves the composer's Nitro gift button out.
///
/// The composer fills an `Option` for the button: its block, after the
/// `gift-button.wrap` id, stores `Some` (`mov qword [rax - 8], 1`) and jumps
/// past the `None` store, `and qword [rsp + disp32], 0`. The first `jne` to
/// just before that store skips the button. Rewriting its two opcode bytes as
/// `nop; jmp` keeps the displacement, and the locator checks both bytes share
/// a word so the rewrite is one atomic store.
pub fn composer_gift(image: &Image<'_>) -> Found<usize> {
   const SOME_THEN_JMP: [u8; 9] = [0x48, 0xC7, 0x40, 0xF8, 0x01, 0x00, 0x00, 0x00, 0xE9];
   const NONE_STORE: [u8; 4] = [0x48, 0x83, 0xA4, 0x24];

   let ids = literal(image, b"gift-button.wrap")
      .into_iter()
      .flat_map(|text| lea_refs(image, text))
      .collect::<Vec<_>>();
   let id = one("gift button id", &ids)?;
   let (start, _) = image
      .function_bounds(id)
      .ok_or("gift button has no function bounds")?;
   let block = image.read(id, 0x200).ok_or("gift button is unmapped")?;
   let some = block
      .windows(SOME_THEN_JMP.len())
      .position(|window| window == SOME_THEN_JMP)
      .ok_or("gift button stores no Some")?;
   let after = rel32_target(image, id + some + SOME_THEN_JMP.len())
      .ok_or("gift button jump does not decode")?;
   let none = after.checked_sub(9).ok_or("gift button jumps to 0")?;
   match image.read(none, 9) {
      Some(store) if store.starts_with(&NONE_STORE) && store[8] == 0 => {},
      _ => {
         return Err(format!(
            "gift button jumps to {after:#x}, after no None store"
         ));
      },
   }
   let code = none
      .checked_sub(start)
      .and_then(|len| image.read(start, len))
      .ok_or("gift button's None store is outside its function")?;
   (0..code.len().saturating_sub(6))
      .find(|&at| {
         code[at..at + 2] == [0x0F, 0x85]
            && rel32_target(image, start + at + 2)
               .is_some_and(|to| (none - 0x20..=none).contains(&to))
      })
      .map(|at| start + at)
      .filter(|&jne| jne % 8 != 7)
      .ok_or_else(|| format!("no jne skips the gift button before {none:#x}"))
}

/// `Store::delete_message`, the client's optimistic delete. It shares the map
/// and index deletes with the `MESSAGE_DELETE` handler and takes a lock.
pub fn optimistic_delete(image: &Image<'_>, message_delete: usize) -> Found<usize> {
   let (del_start, del_end) = image
      .function_bounds(message_delete)
      .ok_or("MESSAGE_DELETE handler has no unwind entry")?;
   let del_callees = callees(image, del_start, del_end)
      .into_iter()
      .collect::<HashSet<_>>();
   let mut parents: HashMap<usize, HashSet<usize>> = HashMap::new();
   for (site, callee) in call_sites(image, |callee| del_callees.contains(&callee)) {
      if let Some((parent, _)) = image.function_bounds(site) {
         parents.entry(callee).or_default().insert(parent);
      }
   }
   let mut found = Vec::new();
   for parents in parents.values() {
      if parents.len() != 2 || !parents.contains(&del_start) {
         continue;
      }
      let Some(&other) = parents.iter().find(|&&parent| parent != del_start) else {
         continue;
      };
      if found.contains(&other) || !prologues(image).frame.at(image, other) {
         continue;
      }
      // `lock cmpxchg [..], r64` taking the store's write lock, inline or, in
      // ELF builds, in the first function the head calls.
      let cmpxchg = |at: usize| {
         image.read(at, 0x80).is_some_and(|head| {
            head
               .windows(4)
               .any(|w| w[0] == 0xF0 && w[1] & 0xF8 == 0x48 && w[2..] == [0x0F, 0xB1])
         })
      };
      let locks = cmpxchg(other)
         || calls_in(image, other, 0x40)
            .first()
            .is_some_and(|&(_, lock)| cmpxchg(lock));
      let Some((start, end)) = image.function_bounds(other) else {
         continue;
      };
      // 6 shared callees on the builds seen, at most 2 for unrelated callers.
      let shared = callees(image, start, end)
         .into_iter()
         .filter(|callee| del_callees.contains(callee))
         .count();
      if locks && shared >= 4 {
         found.push(other);
      }
   }
   one("optimistic delete", &found)
}

/// `flag_override` in `StoreState`, from the inlined
/// `(user.flags | flag_override) & 1`:
///
/// ```text
/// cmp dword ptr [base + tag], 1    ; (41) 83 B8..BF <tag> 01
/// jne +0x0e
/// mov r64, qword ptr [base + value] ; 48..4F 8B <modrm> <value>
/// cmp reg2, 1; je +9; jmp +0xe; xor r32, r32; cmp reg2, 1; jne +7
/// or r64, qword ptr [base + flags]  ; 48..4F 0B <modrm> <flags>
/// ```
///
/// The Linux build reads it off `r15`, behind REX prefixes, and lays
/// `StoreState` out differently, so the base register is decoded with them.
/// Reads off `&RwLock<StoreState>` sit 0x10 bytes higher, behind the lock, so
/// the lower pair is the one gateway handlers see.
pub fn staff(image: &Image<'_>) -> Found<StaffField> {
   const CHECK: Pattern = Pattern::new(
      "83 ?? ?? ?? ?? ?? 01 75 0E ?? 8B ?? ?? ?? ?? ?? 83 ?? 01 74 09 EB 0E 31 ?? 83 ?? 01 75 07 \
       ?? 0B ?? ?? ?? ?? ??",
   );
   const LOCK_HEADER: usize = 0x10;
   // `[base + disp32]` with no SIB byte, as `base` with REX.B folded in.
   let base = |rex: u8, modrm: u8| {
      (modrm >> 6 == 0b10 && modrm & 7 != 4).then_some(modrm & 7 | (rex & 1) << 3)
   };
   let rex_w = |byte: u8| (0x48..=0x4F).contains(&byte).then_some(byte);
   let mut found = image
      .scan(CHECK)
      .into_iter()
      .filter_map(|at| {
         let bytes = image.read(at, 37)?;
         let cmp_rex = match at.checked_sub(1).and_then(|before| image.read(before, 1)) {
            Some([0x41]) => 0x41,
            _ => 0x40,
         };
         let cmp = (bytes[1] & 0x38 == 0x38).then(|| base(cmp_rex, bytes[1]))??;
         let mov = base(rex_w(bytes[9])?, bytes[11])?;
         let or = base(rex_w(bytes[30])?, bytes[32])?;
         if cmp != mov || mov != or {
            return None;
         }
         let (tag, value) = (imm32(image, at + 2)?, imm32(image, at + 12)?);
         (value == tag + 8).then_some(StaffField { tag, value })
      })
      .collect::<Vec<_>>();
   found.sort_unstable();
   found.dedup();
   match found[..] {
      [only] => Ok(only),
      [direct, locked]
         if locked.tag == direct.tag + LOCK_HEADER
            && locked.value == direct.value + LOCK_HEADER =>
      {
         Ok(direct)
      },
      _ => {
         Err(format!(
            "staff flag reads disagree ({} variants)",
            found.len()
         ))
      },
   }
}

/// `MessageContextMenu` builder and native `MenuItem` helpers for injecting
/// `phoenix.ui.addMessageAction` entries into right-click and `...` menus.
pub struct MessageContextMenu {
   pub build_fn:          usize,
   pub msg_ptr_offset:    usize,
   pub msg_id_offset:     usize,
   pub channel_id_offset: usize,
   pub props_new:         usize,
   pub with_id:           usize,
   pub with_icon:         usize,
   pub with_color:        usize,
   pub vec_push:          usize,
   pub layout:            MenuLayout,
}

/// `MenuItem` and its `Action` variant.
#[derive(Debug, Clone, Copy)]
pub struct MenuLayout {
   pub size:         usize,
   /// `MenuItemProps` inside `MenuItem::Action`.
   pub props:        usize,
   pub props_size:   usize,
   pub action_tag:   u64,
   /// The click handler `Rc<dyn Fn>`, and the word before it Discord zeroes.
   pub on_click:     usize,
   pub on_click_tag: usize,
}

/// `(reg, base, disp)` of `op r64, [base + disp]` (`REX.W op /r` with a
/// `disp8` or `disp32`) at each offset of `bytes`, register numbers 0 to 15.
fn mem_operands(bytes: &[u8], op: u8) -> impl Iterator<Item = (u8, u8, usize)> + '_ {
   (0..bytes.len()).filter_map(move |at| {
      let [rex, code, modrm, ref rest @ ..] = *bytes.get(at..)? else {
         return None;
      };
      if rex & 0xF8 != 0x48 || code != op || modrm & 7 == 4 {
         return None;
      }
      let disp = match modrm >> 6 {
         1 => usize::from(*rest.first()?),
         2 => u32::from_le_bytes(rest.get(..4)?.try_into().ok()?) as usize,
         _ => return None,
      };
      Some((
         (rex >> 2 & 1) << 3 | modrm >> 3 & 7,
         (rex & 1) << 3 | modrm & 7,
         disp,
      ))
   })
}

/// A register's number, as [`mem_operands`] gives it.
fn number(register: Register) -> u8 {
   let (low, ext) = reg(register);
   ext << 3 | low
}

/// Locates `MessageContextMenu`'s item list builder (`build_fn`), the
/// `message_id` / `channel_id` offsets in its input, and the `MenuItem`
/// builders (`props_new`, `with_id`, `with_icon`, `with_color`, `vec_push`).
#[expect(
   clippy::too_many_lines,
   reason = "cross-checks both Copy Message ID and Delete Message menu blocks"
)]
pub fn message_context_menu(image: &Image<'_>) -> Found<MessageContextMenu> {
   let anchor = one(
      "MessageContextMenu string",
      &literal(image, b"MessageContextMenu"),
   )?;
   let anchor_lea = one("MessageContextMenu xref", &lea_refs(image, anchor))?;
   let (build_fn, build_end) = image
      .function_bounds(anchor_lea)
      .ok_or("MessageContextMenu xref has no unwind entry")?;
   if !prologues(image).row.at(image, build_fn) {
      return Err("MessageContextMenu prologue changed".to_owned());
   }

   // Decode `msg_ptr_offset` (`lea arg0, [arg1 + disp]`) and `msg_id_offset`
   // (`mov reg, [rax + disp]`) from the head.
   let abi = Abi::of(image);
   let (arg0, arg1, rax) = (
      number(abi.arg(0)),
      number(abi.arg(1)),
      number(Register::RAX),
   );
   let head = image
      .read(build_fn, 0x160)
      .ok_or("MessageContextMenu head is unmapped")?;
   let msg_ptr_offset = mem_operands(head, 0x8D)
      .find(|&(reg, base, _)| reg == arg0 && base == arg1)
      .ok_or("MessageContextMenu message ptr offset not found")?
      .2;
   let msg_id_offset = mem_operands(head, 0x8B)
      .find(|&(reg, base, _)| reg != rax && base == rax)
      .ok_or("MessageContextMenu message id offset not found")?
      .2;
   if !(0x20..=0x200).contains(&msg_ptr_offset) || !(0x20..=0x200).contains(&msg_id_offset) {
      return Err(format!(
         "MessageContextMenu offsets out of range (msg={msg_ptr_offset:#x}, id={msg_id_offset:#x})"
      ));
   }

   // Soon after `MessageContextMenu`'s anchor call, a call computes message
   // permissions and reads `&message.channel_id` into its second argument,
   // `lea arg1, [msg + disp32]`, as far as anchor + 0x7D on Linux.
   let post_anchor_calls = calls_in(image, anchor_lea, 0xC0);
   let channel_id_offset = post_anchor_calls
      .iter()
      .find_map(|&(_, callee)| {
         let body = image.read(callee, 0x80)?;
         mem_operands(body, 0x8D)
            .find(|&(reg, _, disp)| reg == arg1 && (0x100..=0x300).contains(&disp))
            .map(|(.., disp)| disp)
      })
      .ok_or("MessageContextMenu channel_id offset not found")?;

   // Find the `Copy Message ID` and `Delete Message` `lea`s inside `build_fn`
   // directly (scanning only the ~19 KiB function instead of the whole
   // `.text`).
   let build_bytes = image
      .read(build_fn, build_end - build_fn)
      .ok_or("MessageContextMenu body is unmapped")?;
   let find_in_build = |label: &[u8], name: &str| -> Found<usize> {
      let targets: HashSet<usize> = image.find_bytes(label).into_iter().collect();
      let mut hits = Vec::new();
      for idx in 0..build_bytes.len().saturating_sub(7) {
         if build_bytes[idx] & 0xF0 == 0x40
            && build_bytes[idx + 1] == 0x8D
            && build_bytes[idx + 2] & 0xC7 == 0x05
            && let Some(target) = rel32_target(image, build_fn + idx + 3)
            && targets.contains(&target)
         {
            hits.push(build_fn + idx);
         }
      }
      one(name, &hits)
   };
   let copy_id_lea = find_in_build(b"Copy Message ID", "Copy Message ID in MessageContextMenu")?;
   let delete_lea = find_in_build(b"Delete Message", "Delete Message in MessageContextMenu")?;

   let copy_calls = calls_in(image, copy_id_lea, 0x180);
   if copy_calls.len() < 5 {
      return Err(format!(
         "Copy Message ID block has only {} calls",
         copy_calls.len()
      ));
   }
   let copy_ctor = copy_calls[1].1;
   let with_id = copy_calls[2].1;
   let with_icon = copy_calls[3].1;
   let vec_push = copy_calls[4].1;

   let delete_calls = calls_in(image, delete_lea, 0x150);
   if delete_calls.len() < 6 {
      return Err(format!(
         "Delete Message block has only {} calls",
         delete_calls.len()
      ));
   }
   if delete_calls[2].1 != with_id
      || delete_calls[3].1 != with_icon
      || delete_calls[5].1 != vec_push
   {
      return Err("Copy Message ID and Delete Message helpers disagree".to_owned());
   }
   let with_color = delete_calls[4].1;

   // Walk `copy_ctor -> wrapper -> MenuItem::new_core -> MenuItemProps::new`
   // and verify the `MenuItemProps` size (`0x1C8`) and `Action` tag (`2`).
   let wrapper = straight_calls(image, copy_ctor)
      .last()
      .ok_or("copy-id ctor has no inner call")?
      .1;
   let core_ctor = straight_calls(image, wrapper)
      .last()
      .ok_or("MenuItem ctor wrapper has no core call")?
      .1;
   let core_bytes = function_code(image, core_ctor, "MenuItem core ctor")?;
   let props_new = calls_in(image, core_ctor, core_bytes.len())
      .first()
      .ok_or("MenuItem core ctor has no props_new call")?
      .1;
   let layout = menu_layout(image, core_bytes, vec_push)?;

   Ok(MessageContextMenu {
      build_fn,
      msg_ptr_offset,
      msg_id_offset,
      channel_id_offset,
      props_new,
      with_id,
      with_icon,
      with_color,
      vec_push,
      layout,
   })
}

/// Reads [`MenuLayout`] from the `MenuItem::Action` core constructor and
/// `Vec<MenuItem>::push`.
///
/// The constructor copies `MenuItemProps` in (`lea rdi, [sret + props];
/// lea rsi, ..; mov ecx, size; rep movsb`), clears the handler's tag (`and
/// qword [sret + d8], 0`), stores the `Rc<dyn Fn>` handler, and writes the
/// variant (`mov qword [sret], tag`). The push strides by the item size
/// (`imul reg, reg, size`).
fn menu_layout(image: &Image<'_>, core: &[u8], vec_push: usize) -> Found<MenuLayout> {
   let copy = core
      .windows(7)
      .position(|w| w[0] == 0xB9 && w[5..] == [0xF3, 0xA4])
      .ok_or("MenuItemProps copy not found")?;
   let props_size = imm_after(&core[copy..], &[0xB9]).ok_or("MenuItemProps size not found")?;
   let props = core[copy.saturating_sub(12)..copy]
      .windows(4)
      .find(|w| w[..2] == [0x48, 0x8D] && w[2] & 0xF8 == 0x78 && w[2] != 0x7C)
      .map(|w| usize::from(w[3]))
      .ok_or("MenuItemProps offset not found")?;
   let tail = &core[copy..];
   let (on_click, _) = rc_store(tail).ok_or("MenuItem handler store not found")?;
   let on_click_tag = tail
      .windows(5)
      .find(|w| w[..2] == [0x48, 0x83] && w[2] & 0xF8 == 0x60 && w[2] != 0x64 && w[4] == 0)
      .map(|w| usize::from(w[3]))
      .ok_or("MenuItem handler tag not found")?;
   let action_tag = tail
      .windows(7)
      .find(|w| w[..2] == [0x48, 0xC7] && w[2] < 8 && w[2] != 4 && w[2] != 5)
      .map(|w| u64::from(u32::from_le_bytes([w[3], w[4], w[5], w[6]])))
      .filter(|&tag| tag < 0x10)
      .ok_or("MenuItem::Action tag not found")?;
   let push = function_code(image, vec_push, "Vec<MenuItem>::push")?;
   let size = push
      .windows(7)
      .find(|w| w[0] & 0xF0 == 0x40 && w[1] == 0x69 && w[2] >= 0xC0)
      .map(|w| u32::from_le_bytes([w[3], w[4], w[5], w[6]]) as usize)
      .ok_or("Vec<MenuItem>::push stride not found")?;
   let size = fits("MenuItem", size, layout::SCRATCH_CAP)?;
   if props + props_size > size || on_click + 16 > size || on_click_tag + 8 > size {
      return Err("MenuItem fields sit past the MenuItem".to_owned());
   }
   Ok(MenuLayout {
      size,
      props,
      props_size,
      action_tag,
      on_click,
      on_click_tag,
   })
}

/// A `<T as IntoElement>::into_any_element` wrapper, the one small loader of
/// the type name `T` that copies `T` once (`mov arg2, size; call memcpy`), and
/// that size.
fn find_into_any(
   image: &Image<'_>,
   leas: &HashMap<usize, Vec<usize>>,
   type_name: &[u8],
) -> Found<(usize, usize)> {
   let len = mov_imm32(Abi::of(image).arg(2));
   let mut found = Vec::new();
   for addr in image.find_bytes(type_name) {
      for &lea in leas.get(&addr).into_iter().flatten() {
         let Some((start, end)) = image.function_bounds(lea) else {
            continue;
         };
         if end.saturating_sub(start) >= 0x100 || found.iter().any(|&(f, _)| f == start) {
            continue;
         }
         let Some(code) = image.read(start, end - start) else {
            continue;
         };
         // Whole instructions only: a `mov edx, imm32` is one `BA` byte, which
         // a displacement can hold too.
         let copies: Vec<usize> = phoenix_hook::x86::disasm(code, start, usize::MAX)
            .into_iter()
            .filter(|line| line.len == len.len() + 4)
            .filter_map(|line| imm_after(code.get(line.addr - start..)?.get(..line.len)?, &len))
            .collect();
         if let [size] = copies[..] {
            found.push((start, size));
         }
      }
   }
   let label = std::str::from_utf8(type_name).unwrap_or("into_any");
   let (func, size) = one(label, &found)?;
   Ok((func, fits(label, size, layout::SCRATCH_CAP)?))
}

/// The loads of every copy of each of `needles`, from one pass over `.text`.
fn string_leas(image: &Image<'_>, needles: &[&[u8]]) -> HashMap<usize, Vec<usize>> {
   let targets: Vec<usize> = needles
      .iter()
      .flat_map(|needle| image.find_bytes(needle))
      .collect();
   lea_refs_many(image, &targets)
}

/// What the native modal overlay builds and hooks.
///
/// `Workspace::render_overlays`, the `mana_html::hosted::modal::Modal`
/// builders, `Vec<AnyElement>::push`, `Div::id` (required for mouse-wheel
/// scrolling), GPUI's `WM_USER+5` force-redraw validation-number offset, and
/// the backdrop's click-outside wiring.
pub struct Modal {
   pub overlay_render:           usize,
   pub vec_push_any:             usize,
   pub modal_new:                usize,
   pub modal_into_any:           usize,
   pub modal_child_div:          usize,
   pub modal_footer_button:      usize,
   pub div_id:                   usize,
   pub window_validation_offset: usize,
   pub dismiss:                  Dismiss,
   pub layout:                   ModalLayout,
}

/// `mana_html::hosted::modal::Modal`.
#[derive(Debug, Clone, Copy)]
pub struct ModalLayout {
   pub size:     usize,
   /// `Vec<AnyElement>` of body children and of footer buttons.
   pub children: usize,
   pub footer:   usize,
   /// `Option<SharedString>`.
   pub subtitle: usize,
   /// `Option<Rc<dyn Fn(&mut Window, &mut App)>>`.
   pub on_close: usize,
   /// The `ModalSize` field.
   pub scale:    usize,
}

/// What Discord's overlay `backdrop` uses to close on a press outside the
/// dialog.
///
/// `Interactivity::on_mouse_down` pushes a `Box<dyn Fn(&MouseDownEvent,
/// DispatchPhase, &Hitbox, &mut Window, &mut App)>`, whose wrapper checks
/// `Hitbox::is_hovered` and calls `cx.stop_propagation()`.
#[derive(Debug, Clone, Copy)]
pub struct Dismiss {
   /// `Interactivity` offset of the mouse-down listener `Vec`.
   pub mouse_down_listeners: usize,
   /// `Vec<Box<dyn ..>>::push(vec, data, vtable)`.
   pub listener_push:        usize,
   /// `Hitbox::is_hovered(&Hitbox, &Window) -> bool`.
   pub hitbox_is_hovered:    usize,
   /// `App` offset of `propagate_event`.
   pub propagate_event:      usize,
}

/// Locates `Workspace::render_overlays` (`nav-overlay-popout-layer`), `Modal`
/// builders, `Vec<AnyElement>::push`, `Div::id`, and GPUI's
/// `WM_GPUI_FORCE_UPDATE_WINDOW` (`0x405`) validation offset.
#[expect(
   clippy::too_many_lines,
   reason = "one locator per modal builder, cross-checked"
)]
pub fn modal(image: &Image<'_>, div_into_any: usize, div: &DivLayout) -> Found<Modal> {
   // `Modal::child::<Div>` and `Modal::footer_button` push an `AnyElement`
   // into one of the `Modal`'s `Vec`s (`lea arg0, [modal + vec]; lea arg1,
   // [element]; call push`) and return it by copy (`mov ecx, size`), Win64
   // with its SEH `nop` between. The child pusher is the `Div` one whose
   // `Vec` is not the footer's.
   const MODAL_TYPE: &[u8] = b"mana_html::hosted::modal::Modal";
   const BUTTON_TYPE: &[u8] = b"mana_gpui::components::button::Button";
   let leas = string_leas(image, &[
      b"nav-overlay-popout-layer",
      b"mana-modal",
      MODAL_TYPE,
      BUTTON_TYPE,
      b"game-server-shop-carousel",
   ]);
   let leas_of = |addr: usize| leas.get(&addr).cloned().unwrap_or_default();

   let overlay_str = one(
      "nav-overlay-popout-layer",
      &image.find_bytes(b"nav-overlay-popout-layer"),
   )?;
   let overlay_lea = one("nav-overlay-popout-layer load", &leas_of(overlay_str))?;
   let (overlay_render, overlay_end) = image
      .function_bounds(overlay_lea)
      .ok_or("Workspace::render_overlays has no unwind entry")?;
   if !prologues(image).frame.at(image, overlay_render) {
      return Err("Workspace::render_overlays prologue changed".to_owned());
   }

   // `Modal::new` (about 1 KiB) shares a `mana-modal` literal with the large
   // `Modal::render` (> 0x4000, frame prologue). ELF builds keep a copy of it
   // per codegen unit, so the most called one is taken.
   let calls = call_index(image);
   let mut ranked = Vec::new();
   for addr in literal(image, b"mana-modal") {
      let mut funcs: Vec<(usize, usize)> = leas_of(addr)
         .into_iter()
         .filter_map(|lea| image.function_bounds(lea))
         .collect();
      funcs.sort_unstable();
      funcs.dedup();
      let rendered = funcs
         .iter()
         .any(|&(start, end)| end - start > 0x4000 && prologues(image).frame.at(image, start));
      if !rendered {
         continue;
      }
      ranked.extend(
         funcs
            .into_iter()
            .filter(|&(start, end)| end - start <= 0x800)
            .map(|(start, _)| (calls.sites(start).count(), start)),
      );
   }
   ranked.sort_unstable_by(|a, b| b.cmp(a));
   ranked.dedup();
   let modal_new = clear_winner("Modal::new", &ranked, 4)?;

   let (modal_into_any, modal_size) = find_into_any(image, &leas, MODAL_TYPE)?;
   let (button_into_any, _) = find_into_any(image, &leas, BUTTON_TYPE)?;

   let arg0 = Abi::of(image).arg(0);
   let copy = [
      &[0xB9][..],
      &u32::try_from(modal_size).unwrap_or(0).to_le_bytes(),
   ]
   .concat();
   let mut pushers: Vec<(usize, usize, usize, usize)> = Vec::new();
   for at in image.find_bytes(&copy) {
      let Some(call) = at.checked_sub(5 + usize::from(image.read(at - 1, 1) == Some(&[0x90])))
      else {
         continue;
      };
      let Some((start, end)) = image.function_bounds(call) else {
         continue;
      };
      if image.read(call, 1) != Some(&[0xE8]) || pushers.iter().any(|p| p.0 == start) {
         continue;
      }
      let vec = image
         .read(call - 12, 12)
         .and_then(|before| (0..before.len()).find_map(|at| lea_disp8(&before[at..], arg0)));
      let (Some(first), Some(push_fn), Some(vec)) = (
         calls_in(image, start, end - start)
            .first()
            .map(|&(_, callee)| callee),
         rel32_target(image, call + 1),
         vec.map(usize::from),
      ) else {
         continue;
      };
      pushers.push((start, first, push_fn, vec));
   }
   let footers: Vec<(usize, usize, usize)> = pushers
      .iter()
      .filter(|p| p.1 == button_into_any)
      .map(|p| (p.0, p.2, p.3))
      .collect();
   let (modal_footer_button, push_b, footer) = one("Modal::footer_button", &footers)?;
   let childs: Vec<(usize, usize, usize)> = pushers
      .iter()
      .filter(|p| p.1 == div_into_any && p.3 != footer)
      .map(|p| (p.0, p.2, p.3))
      .collect();
   let (modal_child_div, vec_push_any, children) = one("Modal::child<Div>", &childs)?;
   if vec_push_any != push_b {
      return Err("Modal child/footer Vec<AnyElement>::push disagree".to_owned());
   }
   if !callees(image, overlay_render, overlay_end)
      .into_iter()
      .any(|callee| callee == vec_push_any || same_code(image, callee, vec_push_any))
   {
      return Err(
         "Workspace::render_overlays no longer pushes via Vec<AnyElement>::push".to_owned(),
      );
   }

   // `Div::id` is the first call after `lea` of `"game-server-shop-carousel"`,
   // checking `ElementId::None` in `Interactivity` (`cmp byte ptr [rcx +
   // disp32], 0xff`).
   let carousel = one(
      "game-server-shop-carousel",
      &image.find_bytes(b"game-server-shop-carousel"),
   )?;
   // `Div::id(div, id)` takes the id string as its third argument.
   let abi = Abi::of(image);
   let carousel_leas: Vec<usize> = leas_of(carousel)
      .into_iter()
      .filter(|&lea| image.read(lea, 3) == Some(&lea_rip(abi.arg(2))[..]))
      .collect();
   let carousel_lea = one("game-server-shop-carousel load", &carousel_leas)?;
   let div_id = calls_in(image, carousel_lea, 0x30)
      .first()
      .ok_or("Div::id call after carousel anchor not found")?
      .1;
   // `cmp byte [interactivity + disp32], 0xff`, `ElementId::None`.
   if !image.read(div_id, 0x200).is_some_and(|code| {
      code
         .windows(7)
         .any(|w| w[0] == 0x80 && w[1] & 0xF8 == 0xB8 && !matches!(w[1] & 7, 4 | 5) && w[6] == 0xFF)
   }) {
      return Err("Div::id element_id offset check failed".to_owned());
   }

   let window_validation_offset = match abi {
      Abi::Win64 => window_validation(image)?,
      Abi::SysV => 0,
   };

   Ok(Modal {
      overlay_render,
      vec_push_any,
      modal_new,
      modal_into_any,
      modal_child_div,
      modal_footer_button,
      div_id,
      window_validation_offset,
      dismiss: backdrop_dismiss(image, overlay_render, overlay_end, div)?,
      layout: ModalLayout {
         size: modal_size,
         children,
         footer,
         subtitle: modal_subtitle(image, modal_size)?,
         on_close: modal_on_close(image, modal_size)?,
         scale: modal_scale(image, modal_new, modal_size)?,
      },
   })
}

/// GPUI's Wayland frame scheduling, which [`wayland_frame`] locates.
///
/// Each `WaylandWindow` is `{state, callbacks, status, ping, requested}`:
/// `status` an `Rc<Cell<u8>>` (7 idle, 5 requested, 1 in flight, 2 again),
/// `ping` the calloop `Ping` whose callback calls [`Self::frame`] on every
/// requested window, and `requested` an `Rc<Cell<Option<Instant>>>`.
#[derive(Debug, Clone, Copy)]
pub struct WaylandFrame {
   /// `WaylandWindowStatePtr::frame(&window, requested, force)`, which hands
   /// the window's `request_frame` callback `state + force_render` as its
   /// `force_render`.
   pub frame:        usize,
   pub force_render: usize,
   /// calloop's `Ping::ping(&ping)`, safe from any thread.
   pub ping:         usize,
}

/// Locates [`WaylandFrame`] from `frame`'s start and the `request_frame` that
/// moves `status` from idle to requested and pings, on System V builds.
///
/// `frame` loads `force_render` with the first `movzx r8d, byte [state +
/// disp32]`, whose state register changed from r13 to r12 after build 2233.
pub fn wayland_frame(image: &Image<'_>) -> Found<WaylandFrame> {
   const FRAME: Pattern = Pattern::new(
      "55 41 57 41 56 41 55 41 54 53 48 81 EC ?? ?? ?? ?? 48 8B 6F 10 C6 45 10 01 4C 8B",
   );
   /// `status` at +0x10, idle 7 to requested 5, `requested` at +0x20 set to
   /// `Instant::now()`, then a tail call to `Ping::ping(&self.ping)` at +0x18.
   const REQUEST: Pattern = Pattern::new(
      "48 8B 47 10 0F B6 48 10 83 F9 07 74 ?? 83 F9 01 75 ?? C6 40 10 02 48 83 C4 08 5B 41 5E C3 \
       C6 40 10 05 4C 8B 77 20 48 89 FB FF 15 ?? ?? ?? ?? 49 89 46 10 41 89 56 18 48 83 C3 18 48 \
       89 DF 48 83 C4 08 5B 41 5E FF 25",
   );
   const REQUEST_LEN: usize = 71;
   let frame = one("Wayland frame", &image.scan(FRAME))?;
   let force_render = image
      .read(frame, 0x80)
      .and_then(|code| {
         phoenix_hook::x86::disasm(code, frame, 24)
            .into_iter()
            .find(|line| line.text.starts_with("movzx r8d,byte ptr ["))
      })
      .and_then(|line| imm32(image, line.addr + line.len - 4))
      .ok_or("Wayland frame force_render did not decode")?;
   let request = one("Wayland request_frame", &image.scan(REQUEST))?;
   let ping = rel32_target(image, request + REQUEST_LEN)
      .and_then(|slot| image.read_usize(slot))
      .filter(|&ping| image.is_code(ping))
      .ok_or("Wayland request_frame ping did not decode")?;
   Ok(WaylandFrame {
      frame,
      force_render,
      ping,
   })
}

/// GPUI's `Zed::Window` `WndProc` checks `wParam == *(rc + disp32)` for
/// `WM_USER+5` (`0x405`, `WM_GPUI_FORCE_UPDATE_WINDOW`), a Win32 message.
fn window_validation(image: &Image<'_>) -> Found<usize> {
   const VALIDATION_PAT: Pattern = Pattern::new(
      "8D 83 FF FB FF FF 83 F8 13 0F 87 ?? ?? ?? ?? B9 ?? ?? ?? ?? 0F A3 C1 0F 83 ?? ?? ?? ?? 4C \
       3B B2",
   );
   let val_hit = one("GPUI window validation check", &image.scan(VALIDATION_PAT))?;
   let offset = imm32(image, val_hit + 32).ok_or("GPUI window validation offset did not decode")?;
   if !(0x400..=0x4000).contains(&offset) {
      return Err(format!(
         "GPUI window validation offset out of range ({offset:#x})"
      ));
   }
   Ok(offset)
}

/// The best of `ranked` (`(count, value)`, sorted down) if it beats the
/// runner-up `margin` times over.
fn clear_winner(what: &str, ranked: &[(usize, usize)], margin: usize) -> Found<usize> {
   match ranked {
      [(top, value), rest @ ..] if rest.first().is_none_or(|&(next, _)| *top >= margin * next) => {
         Ok(*value)
      },
      _ => {
         Err(format!(
            "{what} is ambiguous ({:x?})",
            &ranked[..ranked.len().min(4)]
         ))
      },
   }
}

/// Small functions that return a `size`-byte struct by copy, `mov arg2,
/// size; call memcpy` or `mov ecx, size; rep movsb`.
fn copy_returners(image: &Image<'_>, size: usize, max_len: usize) -> Vec<(usize, usize)> {
   let Ok(size) = u32::try_from(size) else {
      return Vec::new();
   };
   let le = size.to_le_bytes();
   let mut funcs: Vec<(usize, usize)> = [mov_imm32(Abi::of(image).arg(2)), vec![0xB9]]
      .into_iter()
      .flat_map(|op| image.find_bytes(&[&op[..], &le].concat()))
      .filter(|&at| image.is_code(at))
      .filter_map(|at| image.function_bounds(at))
      .filter(|&(start, end)| end - start <= max_len)
      .collect();
   funcs.sort_unstable();
   funcs.dedup();
   funcs
}

/// `Modal::subtitle`, the most called small `SharedString` setter on a
/// `Modal` (160 callers to 2 for the next on 2026-11 builds).
fn modal_subtitle(image: &Image<'_>, size: usize) -> Found<usize> {
   let calls = call_index(image);
   let mut setters: Vec<(usize, usize)> = copy_returners(image, size, 0x180)
      .into_iter()
      .filter_map(|(start, end)| {
         let field = shared_string_setter(image.read(start, end - start)?)?;
         Some((calls.sites(start).count(), field))
      })
      .filter(|&(_, field)| field + 24 <= size)
      .collect();
   setters.sort_unstable_by(|a, b| b.cmp(a));
   clear_winner("Modal subtitle setter", &setters, 4)
}

/// `Modal::on_close`, the `Rc<dyn Fn>` most often stored into a `Modal` right
/// before it is returned by copy (344 stores, the only field on 2026-11
/// builds).
fn modal_on_close(image: &Image<'_>, size: usize) -> Found<usize> {
   let mut votes: HashMap<usize, usize> = HashMap::new();
   let le = u32::try_from(size)
      .map_err(|_| "Modal size overflows")?
      .to_le_bytes();
   for at in image.find_bytes(&[0xB9, le[0], le[1], le[2], le[3]]) {
      if !image.is_code(at) {
         continue;
      }
      let Some(window) = image.read(at.saturating_sub(0x24), 0x24 + 0x10) else {
         continue;
      };
      if let Some((field, Some(copy))) = rc_store(window)
         && copy == size
         && field + 16 <= size
      {
         *votes.entry(field).or_default() += 1;
      }
   }
   let mut ranked: Vec<(usize, usize)> = votes.into_iter().map(|(at, n)| (n, at)).collect();
   ranked.sort_unstable_by(|a, b| b.cmp(a));
   clear_winner("Modal on_close store", &ranked, 4)
}

/// How often each byte of a `size`-byte struct is set right after `ctor`
/// returns it, over every call site, most set first.
fn post_ctor_byte_stores(image: &Image<'_>, ctor: usize, size: usize) -> Vec<(usize, usize)> {
   let mut counts: HashMap<usize, usize> = HashMap::new();
   let abi = Abi::of(image);
   for site in call_index(image).sites(ctor) {
      let Some((len, _)) = image
         .read(site, 6)
         .and_then(|call| call_target(image, call, site))
      else {
         continue;
      };
      let (Some(before), Some(after)) =
         (image.read(site - 0x30, 0x30), image.read(site + len, 0x60))
      else {
         continue;
      };
      let mut seen = HashSet::new();
      for (at, _) in
         phoenix_hook::x86::sret_byte_stores(abi, before, site - 0x30, after, site + len)
      {
         if at < size && seen.insert(at) {
            *counts.entry(at).or_default() += 1;
         }
      }
   }
   let mut ranked: Vec<(usize, usize)> = counts.into_iter().map(|(at, n)| (n, at)).collect();
   ranked.sort_unstable_by(|a, b| b.cmp(a));
   ranked
}

/// `Modal::size`, the byte Discord most often sets right after `Modal::new`
/// (28 call sites to 3 on 2026-11 builds).
fn modal_scale(image: &Image<'_>, modal_new: usize, size: usize) -> Found<usize> {
   clear_winner(
      "Modal size store",
      &post_ctor_byte_stores(image, modal_new, size),
      4,
   )
}

/// Follows the `"backdrop"` element in `Workspace::render_overlays` down to its
/// `on_mouse_down` call, a few small helpers deep.
fn backdrop_dismiss(
   image: &Image<'_>,
   overlay: usize,
   overlay_end: usize,
   div: &DivLayout,
) -> Found<Dismiss> {
   let body = image
      .read(overlay, overlay_end - overlay)
      .ok_or("Workspace::render_overlays unreadable")?;
   let backdrop: Vec<usize> = body
      .windows(7)
      .enumerate()
      .filter(|(_, w)| w[1] == 0x8D && w[0] & 0xF0 == 0x40 && w[2] & 0xC7 == 0x05)
      .filter_map(|(offset, _)| {
         let lea = overlay + offset;
         let target = rel32_target(image, lea + 3)?;
         (image.read(target, 8)? == b"backdrop").then_some(lea)
      })
      .collect();
   let backdrop = one("backdrop load in render_overlays", &backdrop)?;

   // Breadth-first over small callees: `when(..)` -> `|div| ..` ->
   // `on_mouse_down`.
   let mut frontier: Vec<(usize, Option<usize>)> = calls_in(image, backdrop, 0x400)
      .into_iter()
      .map(|(_, callee)| (callee, None))
      .collect();
   let mut seen = HashSet::new();
   for _ in 0..3 {
      let mut next = Vec::new();
      for (func, parent) in frontier {
         let Some((start, end)) = image.function_bounds(func) else {
            continue;
         };
         if start != func || end - start > 0x400 || !seen.insert(func) {
            continue;
         }
         if let Some(found) = on_mouse_down_shape(image, start, end, div.interactivity_size) {
            let accessor_ok = parent.is_some_and(|parent| {
               image
                  .function_bounds(parent)
                  .is_some_and(|(p_start, p_end)| {
                     calls_in(image, p_start, p_end - p_start)
                        .iter()
                        .any(|&(_, callee)| interactivity_accessor(image, callee, div))
                  })
            });
            if !accessor_ok {
               return Err("on_mouse_down caller no longer reads Div's Interactivity".to_owned());
            }
            return Ok(found);
         }
         next.extend(
            calls_in(image, start, end - start)
               .into_iter()
               .map(|(_, callee)| (callee, Some(start))),
         );
      }
      frontier = next;
   }
   Err("backdrop on_mouse_down not found".to_owned())
}

/// `mov rax, [rcx + interactivity]` early in `Div`'s `interactivity()`.
fn interactivity_accessor(image: &Image<'_>, func: usize, div: &DivLayout) -> bool {
   let Ok(disp) = u8::try_from(div.interactivity) else {
      return false;
   };
   let [rex, op, modrm] = rm_disp8(0x8B, Register::RAX, Abi::of(image).arg(0));
   let want = [rex, op, modrm, disp];
   image
      .read(func, 0x20)
      .is_some_and(|head| head.windows(4).any(|w| w == want))
}

/// `Interactivity::on_mouse_down`: `add reg, <vec offset>`, then
/// `lea arg2, [vtable]; ...; jmp <push>`, where the vtable's `call` starts
/// with the `DispatchPhase::Bubble` test and calls `Hitbox::is_hovered`.
///
/// The listener is `call(self, event, phase, hitbox, ..)`, so the event,
/// phase and hitbox sit in the second to fourth argument registers.
fn on_mouse_down_shape(
   image: &Image<'_>,
   start: usize,
   end: usize,
   interactivity_size: usize,
) -> Option<Dismiss> {
   let abi = Abi::of(image);
   let body = image.read(start, end - start)?;
   let lea = body.windows(3).position(|w| w == lea_rip(abi.arg(2)))?;
   let vtable = rel32_target(image, start + lea + 3)?;
   let listener_push = (lea + 7..body.len().min(lea + 23))
      .find_map(|at| jump_target(image, &body[at..], start + at))?;
   let add = body[..lea]
      .windows(3)
      .rposition(|w| w[0] & 0xFE == 0x48 && w[1] == 0x81 && w[2] & 0xF8 == 0xC0)?;
   let mouse_down_listeners = imm32(image, start + add + 3)?;
   if mouse_down_listeners + 0x18 > interactivity_size || !image.is_code(listener_push) {
      return None;
   }

   let call = image.read_usize(vtable + 0x28)?;
   let wrapper = image.read(call, 0x100)?;
   let phase = test_self(abi.arg(2));
   if !phase
      .iter()
      .any(|test| wrapper[..0x10].windows(test.len()).any(|w| w == test))
   {
      return None;
   }
   // `mov arg0, hitbox; (mov arg1, window;) call is_hovered; test al, al; je
   // ..; mov byte [cx + disp32], 0`
   let hitbox = mov_rr(abi.arg(0), abi.arg(3));
   let (at, hitbox_is_hovered, propagate_event) = (0..wrapper.len()).find_map(|at| {
      let w = wrapper.get(at..)?;
      if !hitbox.iter().any(|mov| w.starts_with(mov)) {
         return None;
      }
      let skip = if w.get(3)? & 0xF8 == 0x48 && matches!(w.get(4)?, 0x89 | 0x8B) {
         6
      } else {
         3
      };
      let (len, callee) = call_target(image, &w[skip..], call + at + skip)?;
      let w = w.get(skip + len..skip + len + 11)?;
      (w[..3] == [0x84, 0xC0, 0x74]
         && w[4] == 0xC6
         && w[5] & 0xC0 == 0x80
         && w[5] & 0x07 != 0x04
         && w[10] == 0)
         .then(|| {
            (
               at,
               callee,
               u32::from_le_bytes([w[6], w[7], w[8], w[9]]) as usize,
            )
         })
   })?;
   // A button listener compares `event.button` first (`mov al, [event +
   // disp8]`), which rules out the scroll and move listener `Vec`s of the same
   // shape.
   let event = 0x40 | reg(abi.arg(1)).0;
   if !wrapper[..at]
      .windows(2)
      .any(|w| w == [0x8A, event] || w == [0xB6, event])
   {
      return None;
   }
   (image.is_code(hitbox_is_hovered) && propagate_event < 0x10000).then_some(Dismiss {
      mouse_down_listeners,
      listener_push,
      hitbox_is_hovered,
      propagate_event,
   })
}

/// Native Mana/GPUI `FieldState`, `TextInput`, and `TextArea` bindings for
/// in-app URL entry and plugin code editing.
pub struct TextFields {
   pub cx_new_single:       usize,
   pub cx_new_multiline:    usize,
   pub entity_map_get:      usize,
   pub entity_downgrade:    usize,
   pub weak_drop:           usize,
   pub text_get:            usize,
   pub set_value:           usize,
   pub update_entity_dyn:   usize,
   pub arc_drop_slow:       usize,
   pub text_input_new:      usize,
   pub text_input_into_any: usize,
   pub text_area_new:       usize,
   pub text_area_into_any:  usize,
   pub layout:              FieldLayout,
}

/// Mana `FieldState`, the hosted `TextInput` and `TextArea` built on it, and
/// the GPUI handles they go through.
#[derive(Debug, Clone, Copy)]
pub struct FieldLayout {
   pub text_input_size:   usize,
   pub text_input_entity: usize,
   pub text_area_size:    usize,
   pub text_area_entity:  usize,
   /// `TextArea`'s `min_rows: Option<u8>`, with `max_rows` 2 bytes on, or
   /// `None` when `TextArea::new` no longer shows them.
   pub text_area_rows:    Option<usize>,
   /// `FieldState`'s `Option<SharedString>` slots: the two placeholders and
   /// the value `set_value` clears.
   pub placeholders:      [usize; 2],
   pub pending_value:     usize,
   /// `App`'s `EntityMap`.
   pub app_entities:      usize,
   /// The `Weak` inside a `WeakEntity`, what `weak_drop` takes a pointer to.
   pub weak_ptr:          usize,
}

/// `FieldState::new(out, a, b)` and `new_multiline`, thunks that pass their
/// arguments on to `inner(out, multiline, a, b)`, and that inner function:
/// the pair whose inner function ends by clearing three
/// `Option<SharedString>`s. Win64 calls it; System V calls or tail-jumps to
/// it.
fn field_state_new(image: &Image<'_>, abi: Abi) -> Found<(usize, usize, usize)> {
   /// Thunk patterns and where the call or jump to the inner function sits.
   type Thunks<'a> = &'a [(Pattern, usize)];
   let (singles, multis): (Thunks<'_>, Thunks<'_>) = match abi {
      Abi::Win64 => {
         (
            &[(
               Pattern::new("56 48 83 EC 20 4D 89 C1 49 89 D0 48 89 CE 31 D2 E8"),
               16,
            )],
            &[(
               Pattern::new("56 48 83 EC 20 4D 89 C1 49 89 D0 48 89 CE B2 01 E8"),
               16,
            )],
         )
      },
      Abi::SysV => {
         (
            &[
               (Pattern::new("48 89 D1 48 89 F2 31 F6"), 8),
               (Pattern::new("53 48 89 D1 48 89 F2 48 89 FB 31 F6"), 12),
            ],
            &[
               (Pattern::new("48 89 D1 48 89 F2 6A 01 5E"), 9),
               (
                  Pattern::new("53 48 89 D1 48 89 F2 48 89 FB BE 01 00 00 00"),
                  15,
               ),
            ],
         )
      },
   };
   let inner = |at: usize| {
      let code = image.read(at, 6)?;
      call_target(image, code, at)
         .map(|(_, target)| target)
         .or_else(|| jump_target(image, code, at))
   };
   let thunks = |shapes: &[(Pattern, usize)]| -> Vec<(usize, usize)> {
      shapes
         .iter()
         .flat_map(|&(pattern, at)| {
            image
               .scan(pattern)
               .into_iter()
               .filter_map(move |thunk| Some((thunk, inner(thunk + at)?)))
         })
         .collect()
   };
   let multis: HashMap<usize, usize> = thunks(multis)
      .into_iter()
      .map(|(thunk, target)| (target, thunk))
      .collect();
   let pairs: Vec<(usize, usize, usize)> = thunks(singles)
      .into_iter()
      .filter(|&(_, target)| field_slots(image, target).is_some())
      .filter_map(|(thunk, target)| Some((thunk, *multis.get(&target)?, target)))
      .collect();
   one("FieldState::new thunks", &pairs)
}

/// The last three `None` stores `FieldState::new` ends with, in its own body
/// or, where System V outlines its tail, in the one callee that holds them.
fn field_slots(image: &Image<'_>, inner: usize) -> Option<Vec<usize>> {
   let last3 = |func: usize| {
      let slots = none_stores(function_code(image, func, "").ok()?);
      slots
         .get(slots.len().checked_sub(3)?..)
         .map(<[usize]>::to_vec)
   };
   last3(inner).or_else(|| {
      let (start, end) = image.function_bounds(inner)?;
      calls_in(image, start, end - start)
         .into_iter()
         .find_map(|(_, callee)| last3(callee))
   })
}

/// `(site, callee)` of the direct and `[rip + slot]` calls among the first
/// `max` instructions from `start` to `end`.
fn decoded_calls(image: &Image<'_>, start: usize, end: usize, max: usize) -> Vec<(usize, usize)> {
   let Some(body) = image.read(start, end.saturating_sub(start)) else {
      return Vec::new();
   };
   phoenix_hook::x86::disasm(body, start, max)
      .into_iter()
      .filter(|line| line.text.starts_with("call "))
      .filter_map(|line| {
         let code = image.read(line.addr, line.len)?;
         Some((line.addr, call_target(image, code, line.addr)?.1))
      })
      .collect()
}

/// The end of the first `ret` between `start` and `end`. ELF builds place
/// their landing pads after it.
fn first_ret(image: &Image<'_>, start: usize, end: usize) -> usize {
   image.read(start, end - start).map_or(end, |body| {
      phoenix_hook::x86::disasm(body, start, usize::MAX)
         .into_iter()
         .find(|line| line.text == "ret")
         .map_or(end, |line| line.addr + line.len)
   })
}

/// `inc qword ptr [base + disp32]`, the `App` pending-update bump.
fn inc_disp32(base: Register) -> [u8; 3] {
   let (number, ext) = reg(base);
   [0x48 | ext, 0xFF, 0x80 | number]
}

/// Locates Mana `FieldState` creation, text read/write, and hosted
/// `TextInput`/`TextArea` element constructors.
#[expect(
   clippy::too_many_lines,
   reason = "resolves and cross-checks FieldState, TextInput, and TextArea together"
)]
pub fn text_fields(image: &Image<'_>) -> Found<TextFields> {
   const READ_PANIC: &[u8] = b"readmana_gpui::components::rich_text::state::RichTextState";
   const TEXT_INPUT_TYPE: &[u8] = b"mana_html::hosted::text_input::TextInput";
   const TEXT_AREA_TYPE: &[u8] = b"mana_html::hosted::text_area::TextArea";
   const LOCK_DEC: [u8; 4] = [0xF0, 0x48, 0xFF, 0x08];

   let abi = Abi::of(image);
   let (ctor_single, ctor_multi, inner_a) = field_state_new(image, abi)?;

   // Single pass over direct calls to find callers of both `FieldState` thunks.
   let mut single_callers = HashSet::new();
   let mut multi_callers = HashSet::new();
   for (site, callee) in call_sites(image, |t| t == ctor_single || t == ctor_multi) {
      if let Some((func, _)) = image.function_bounds(site) {
         if callee == ctor_single {
            single_callers.insert(func);
         } else {
            multi_callers.insert(func);
         }
      }
   }

   let pick_cx_new = |candidates: &HashSet<usize>,
                      ctor: usize,
                      name: &str|
    -> Found<(usize, usize, usize, usize)> {
      let mut matches: Vec<(usize, usize, usize, usize)> = candidates
         .iter()
         .filter_map(|&func| {
            let (start, end) = image.function_bounds(func)?;
            let end = first_ret(image, start, end);
            let head = image.read(start, 0x30)?;
            // `cx.new(app, window)` bumps `app`'s pending updates and keeps
            // `window` for the constructor.
            let (window, window_ext) = reg(abi.arg(2));
            let keeps_window = head.windows(3).any(|w| {
               w[0] & 0xFC == 0x48 | window_ext << 2
                  && w[1] == 0x89
                  && w[2] & 0xF8 == 0xC0 | window << 3
            });
            if !head.windows(3).any(|w| w == inc_disp32(abi.arg(1))) || !keeps_window {
               return None;
            }
            let calls: Vec<usize> = decoded_calls(image, start, end, 300)
               .into_iter()
               .map(|(_, callee)| callee)
               .collect();
            if end - start > 0x260 || calls.len() != 10 || calls[4] != ctor {
               return None;
            }
            Some((start, calls[1], calls[3], calls[5]))
         })
         .collect();
      matches.sort_by_key(|(addr, ..)| *addr);
      matches
         .into_iter()
         .next()
         .ok_or_else(|| format!("{name} not found"))
   };

   let (cx_new_single, entity_clone, entity_downgrade, weak_drop) =
      pick_cx_new(&single_callers, ctor_single, "cx.new(FieldState::new)")?;
   let (cx_new_multiline, m_clone, m_down, m_weak) = pick_cx_new(
      &multi_callers,
      ctor_multi,
      "cx.new(FieldState::new_multiline)",
   )?;
   if entity_clone != m_clone || entity_downgrade != m_down || weak_drop != m_weak {
      return Err("cx.new single/multiline helpers disagree".to_owned());
   }

   // Set C: every function referencing any copy of the `RichTextState` "read"
   // panic string (may appear in multiple CGUs).
   // COFF merges the panic's verb into the type name's literal, ELF loads the
   // bare name.
   let read_panic = match abi {
      Abi::Win64 => READ_PANIC,
      Abi::SysV => &READ_PANIC[b"read".len()..],
   };
   let leas = string_leas(image, &[read_panic, TEXT_INPUT_TYPE, TEXT_AREA_TYPE]);
   let read_panics = image.find_bytes(read_panic);
   if read_panics.is_empty() {
      return Err("read RichTextState panic not found".to_owned());
   }
   let mut set_c: Vec<(usize, usize)> = read_panics
      .into_iter()
      .flat_map(|addr| leas.get(&addr).into_iter().flatten().copied())
      .filter_map(|lea| image.function_bounds(lea))
      .collect();
   set_c.sort_unstable();
   set_c.dedup();

   let c_calls: Vec<(usize, Vec<(usize, usize)>)> = set_c
      .iter()
      .map(|&(start, end)| (start, decoded_calls(image, start, end, 600)))
      .collect();
   let c_set: HashSet<usize> = set_c.iter().map(|&(start, _)| start).collect();

   // `text_get(out, field, app)` opens with `lea arg0, [app + entities]` for
   // `EntityMap::get`, which tells it from `Entity::read(entity, app)` that
   // ELF builds keep out of line.
   let entities_lea = rm_disp8(0x8D, abi.arg(0), abi.arg(2));
   let app_entities_of = |func: usize| {
      image.read(func, 0x40).and_then(|head| {
         head
            .windows(4)
            .find(|w| w[..3] == entities_lea)
            .map(|w| usize::from(w[3]))
      })
   };
   // `text_get` is the member of C called by the most other members of C (>=
   // 3).
   let mut caller_counts: HashMap<usize, usize> = HashMap::new();
   for (func, calls) in &c_calls {
      let mut seen = HashSet::new();
      for &(_, callee) in calls {
         if callee != *func
            && c_set.contains(&callee)
            && seen.insert(callee)
            && app_entities_of(callee).is_some()
         {
            *caller_counts.entry(callee).or_default() += 1;
         }
      }
   }
   let mut ranked: Vec<(usize, usize)> = caller_counts
      .into_iter()
      .filter(|&(_, count)| count >= 3)
      .map(|(func, count)| (count, func))
      .collect();
   ranked.sort_unstable_by(|a, b| b.cmp(a));
   let text_get = clear_winner("FieldState text_get", &ranked, 2)?;

   let text_get_calls = &c_calls
      .iter()
      .find(|(f, _)| *f == text_get)
      .ok_or("text_get missing from set C")?
      .1;
   let entity_map_get = text_get_calls
      .first()
      .ok_or("text_get has no EntityMap::get call")?
      .1;

   // `set_value` is the member of C whose calls start `[SharedString::from,
   // text_get, ...]`, clears `pending_value` (`mov byte [reg + d8], 0xff`) and
   // updates the `RichTextState` entity by type name (its length in arg 3).
   let rts_len =
      u8::try_from(READ_PANIC.len() - b"read".len()).map_err(|_| "RichTextState name too long")?;
   let len_loads = imm_loads(abi.arg(3), rts_len);
   let loads_len = |code: &[u8]| {
      len_loads
         .iter()
         .any(|load| code.windows(load.len()).any(|w| w == load.as_slice()))
   };
   // ELF repeats the store on the unwind path.
   let cleared_slots = |body: &[u8]| {
      let mut stores = none_stores(body);
      if stores.is_empty() {
         stores = none_stores_through(body);
      }
      stores.dedup();
      stores
   };
   let set_value_hits: Vec<(usize, &Vec<(usize, usize)>)> = c_calls
      .iter()
      .filter(|(func, calls)| {
         // `SharedString::from(&str)`, which `text_get` ends with too. ELF
         // keeps `set_value::<String>` beside it, which converts first.
         if calls.len() < 4
            || calls[0].1 == entity_map_get
            || calls[1].1 != text_get
            || !text_get_calls
               .iter()
               .any(|&(_, callee)| callee == calls[0].1)
         {
            return false;
         }
         let Some((start, end)) = image.function_bounds(*func) else {
            return false;
         };
         let Some(body) = image.read(start, end - start) else {
            return false;
         };
         let stores = cleared_slots(body);
         stores.len() == 1 && loads_len(body)
      })
      .map(|(f, calls)| (*f, calls))
      .collect();
   // `arc_drop_slow` is called right after `lock dec qword ptr [rax]` (`F0 48
   // FF 08`), or, where the drop is outlined, tail-jumped to after it at the
   // end of the `SharedString` drop's own tail jump.
   let outlined_drop_slow = |drop: usize| {
      let tail = |func: usize| {
         let (start, end) = image
            .function_bounds(func)
            .filter(|&(start, end)| end - start <= 0x20)?;
         let body = image.read(start, end - start)?;
         let target = phoenix_hook::x86::disasm(body, start, usize::MAX)
            .into_iter()
            .filter_map(|line| jump_target(image, image.read(line.addr, line.len)?, line.addr))
            .find(|&target| !(start..end).contains(&target))?;
         Some((body, target))
      };
      let (_, inner) = tail(drop)?;
      let (body, target) = tail(inner)?;
      body.windows(4).any(|w| w == LOCK_DEC).then_some(target)
   };
   let set_value_callees = |sv_calls: &[(usize, usize)]| -> Found<(usize, bool, usize)> {
      let inline = sv_calls.iter().find_map(|&(site, callee)| {
         let before = image.read(site.saturating_sub(24), 24)?;
         before.windows(4).any(|w| w == LOCK_DEC).then_some(callee)
      });
      let arc_drop_slow = inline
         .or_else(|| {
            sv_calls
               .iter()
               .find_map(|&(_, callee)| outlined_drop_slow(callee))
         })
         .ok_or("Arc<str>::drop_slow not found in FieldState::set_value")?;

      // `update_entity_dyn` is called in `set_value` right after the name's
      // length and begins with `inc qword ptr [arg0 + disp32]`.
      let update_entity_dyn = sv_calls
         .iter()
         .find_map(|&(site, callee)| {
            if !loads_len(image.read(site.saturating_sub(32), 32)?) {
               return None;
            }
            let head = image.read(callee, 0x40)?;
            head
               .windows(3)
               .any(|w| w == inc_disp32(abi.arg(0)))
               .then_some(callee)
         })
         .ok_or("App::update_entity_dyn not found in FieldState::set_value")?;
      Ok((arc_drop_slow, inline.is_some(), update_entity_dyn))
   };
   // ELF can keep two copies of `set_value` from different codegen units,
   // one with its drops outlined, each calling its own `Arc<str>::drop_slow`.
   let resolved = set_value_hits
      .iter()
      .map(|&(func, calls)| Ok((func, set_value_callees(calls)?)))
      .collect::<Found<Vec<(usize, (usize, bool, usize))>>>()?;
   let Some(&(set_value, (arc_drop_slow, _, update_entity_dyn))) = resolved
      .iter()
      .find(|(_, (_, inline, _))| *inline)
      .or_else(|| resolved.first())
   else {
      return Err("FieldState::set_value matched 0 functions".to_owned());
   };
   if resolved
      .iter()
      .any(|&(_, (_, _, found))| found != update_entity_dyn)
   {
      return Err(format!(
         "FieldState::set_value matched {} functions that disagree",
         resolved.len()
      ));
   }

   // Several hosted elements share `HOSTED_NEW`, so each is the one that fills
   // its own size.
   // Hosted `TextInput::new` and `TextArea::new` clone the entity into the
   // struct first: `add arg0, entity; call entity_clone`, or into a temporary
   // that the lower of two `movups [out + entity], xmm` then moves in.
   let add_arg0 = {
      let (number, ext) = reg(abi.arg(0));
      [0x48 | ext, 0x81, 0xC0 | number]
   };
   let hosted: Vec<(usize, usize, usize)> = call_sites(image, |callee| callee == entity_clone)
      .into_iter()
      .filter_map(|(site, _)| {
         let (at, end) = image
            .function_bounds(site)
            .filter(|&(at, _)| site - at <= 0x10)?;
         let added = image
            .read(site.checked_sub(7)?, 7)
            .filter(|add| add[..3] == add_arg0)
            .and_then(|_| imm32(image, site - 4));
         let entity = added.or_else(|| {
            image
               .read(site, 0x30)?
               .windows(7)
               .take(0x20)
               .filter(|w| w[..2] == [0x0F, 0x11] && w[2] & 0xC0 == 0x80 && w[2] & 7 != 4)
               .map(|w| u32::from_le_bytes([w[3], w[4], w[5], w[6]]) as usize)
               .min()
         })?;
         let extent = phoenix_hook::x86::sret_stores(abi, image.read(at, end - at)?, at)
            .into_iter()
            .map(|(field, len)| field + len)
            .max()?;
         Some((at, entity, extent))
      })
      .collect();
   let hosted_new = |size: usize, name: &str| -> Found<(usize, usize)> {
      let hits: Vec<(usize, usize)> = hosted
         .iter()
         .filter(|&&(_, entity, extent)| {
            extent <= size && size - extent < 8 && entity + 0x20 <= size
         })
         .map(|&(at, entity, _)| (at, entity))
         .collect();
      one(name, &hits)
   };
   let (text_input_into_any, text_input_size) = find_into_any(image, &leas, TEXT_INPUT_TYPE)?;
   let (text_area_into_any, text_area_size) = find_into_any(image, &leas, TEXT_AREA_TYPE)?;
   let (text_input_new, text_input_entity) = hosted_new(text_input_size, "hosted TextInput::new")?;
   let (text_area_new, text_area_entity) = hosted_new(text_area_size, "hosted TextArea::new")?;
   let text_area_rows = function_code(image, text_area_new, "hosted TextArea::new")
      .ok()
      .and_then(|code| text_area_rows(code, text_area_size));

   // `FieldState::new` ends by writing `None` into its three
   // `Option<SharedString>`s; the one `set_value` clears is the pending value.
   let pending_value = cleared_slots(function_code(image, set_value, "FieldState::set_value")?)[0];
   let slots = field_slots(image, inner_a)
      .filter(|slots| slots.contains(&pending_value))
      .ok_or("FieldState::new no longer clears three SharedString slots")?;
   let placeholders: Vec<usize> = slots
      .iter()
      .copied()
      .filter(|&slot| slot != pending_value)
      .collect();
   let [first, second] = placeholders[..] else {
      return Err("FieldState placeholder slots did not decode".to_owned());
   };

   let app_entities =
      app_entities_of(text_get).ok_or("App entity map offset not found in text_get")?;
   // `entity_downgrade` bumps the weak count: `mov rax, [arg1 + weak]; cmp
   // rax, -1`.
   let weak_load = rm_disp8(0x8B, Register::RAX, abi.arg(1));
   let weak_ptr = image
      .read(entity_downgrade, 0x20)
      .and_then(|head| {
         head
            .windows(8)
            .find(|w| w[..3] == weak_load && w[4..] == [0x48, 0x83, 0xF8, 0xFF])
      })
      .map(|w| usize::from(w[3]))
      .filter(|&weak| weak + 8 <= layout::ENTITY)
      .ok_or("WeakEntity Weak offset not found in entity_downgrade")?;

   Ok(TextFields {
      cx_new_single,
      cx_new_multiline,
      entity_map_get,
      entity_downgrade,
      weak_drop,
      text_get,
      set_value,
      update_entity_dyn,
      arc_drop_slow,
      text_input_new,
      text_input_into_any,
      text_area_new,
      text_area_into_any,
      layout: FieldLayout {
         text_input_size,
         text_input_entity,
         text_area_size,
         text_area_entity,
         text_area_rows,
         placeholders: [first, second],
         pending_value,
         app_entities,
         weak_ptr,
      },
   })
}

/// `TextArea::new` sets `min_rows` and `max_rows`, two `Option<u8>`s side by
/// side, to `None`: the one pair of `mov byte [reg + disp32], 0` stores 2 bytes
/// apart. Discord's callers then store `Some(3)` to `Some(20)` words there.
fn text_area_rows(code: &[u8], size: usize) -> Option<usize> {
   let zeroed: HashSet<usize> = code
      .windows(7)
      .filter(|w| w[0] == 0xC6 && w[1] & 0xF8 == 0x80 && w[1] != 0x84 && w[6] == 0)
      .map(|w| u32::from_le_bytes([w[2], w[3], w[4], w[5]]) as usize)
      .collect();
   let pairs: Vec<usize> = zeroed
      .iter()
      .copied()
      .filter(|&rows| zeroed.contains(&(rows + 2)) && rows + 4 <= size)
      .collect();
   match pairs[..] {
      [rows] => Some(rows),
      _ => None,
   }
}

/// The Settings layer's render, `fn(sret: *mut AnyElement, layer, window, cx)`.
///
/// The one function that loads both its `layer-trap-` focus-trap id and
/// `settings-layer`. It draws above `Workspace::render_overlays`, so plugin
/// modals opened over Settings go inside it.
pub fn settings_layer(image: &Image<'_>) -> Found<usize> {
   /// How far before a match a merged string literal can start.
   const LITERAL_REACH: usize = 512;
   let trap = one("layer-trap- literal", &image.find_bytes(b"layer-trap-"))?;
   // Rust merges literals, so code loads the closest literal start before it.
   let loads = lea_index(image).range(trap.saturating_sub(LITERAL_REACH)..=trap);
   let start = loads.iter().map(|&(_, target)| target).max();
   let mut funcs: Vec<usize> = loads
      .into_iter()
      .filter(|&(_, target)| Some(target) == start)
      .filter_map(|(site, _)| Some(image.function_bounds(site)?.0))
      .filter(|&func| {
         image
            .function_bounds(func)
            .and_then(|(start, end)| image.read(start, end - start))
            .is_some_and(|code| {
               rip_leas(code, func)
                  .any(|(_, target)| image.read(target, 14) == Some(b"settings-layer".as_slice()))
            })
      })
      .collect();
   funcs.sort_unstable();
   funcs.dedup();
   let func = one("settings layer render", &funcs)?;
   if !prologues(image).frame.at(image, func) {
      return Err("settings layer render prologue changed".to_owned());
   }
   Ok(func)
}

/// The `tracing::Callsite` static naming `"media_fetch"` /
/// `"libdiscord_media::fetch"`, the same data on every platform.
pub(crate) fn media_fetch_callsite(image: &Image<'_>) -> Found<usize> {
   let name = one(
      "media_fetch tracing name",
      &image.find_bytes(b"media_fetchlibdiscord_media::fetch"),
   )?;
   let target = name + 11;
   let name_bytes = name.to_le_bytes();

   let mut callsites = Vec::new();
   for seg in image.segments.iter().filter(|s| !s.executable) {
      let mut pos = 0;
      while pos + 8 <= seg.bytes.len() {
         let Some(rel) = seg.bytes[pos..]
            .as_chunks::<8>()
            .0
            .iter()
            .position(|chunk| *chunk == name_bytes)
         else {
            break;
         };
         let off = pos + rel * 8;
         let slot = seg.addr + off;
         if image.read_usize(slot + 8) == Some(11)
            && image.read_usize(slot + 0x10) == Some(target)
            && image.read_usize(slot + 0x18) == Some(23)
            && let Some(callsite) = image.read_usize(slot + 0x30)
         {
            callsites.push(callsite);
         }
         pos = off + 8;
      }
   }
   one("media_fetch callsite", &callsites)
}

/// Locates `libdiscord_media::fetch` via its `tracing::Metadata` callsite.
///
/// The callsite static names `"media_fetch"` / `"libdiscord_media::fetch"`,
/// and the desktop runtime hooks the function to unwrap custom badge icon
/// URLs (`/badge-icons/phx_u_<hex>`).
pub fn media_fetch(image: &Image<'_>) -> Found<usize> {
   let callsite = media_fetch_callsite(image)?;
   let mut funcs: Vec<usize> = lea_refs(image, callsite)
      .into_iter()
      .filter_map(|site| Some(image.function_bounds(site)?.0))
      .collect();
   funcs.sort_unstable();
   funcs.dedup();
   let func = one("libdiscord_media::fetch", &funcs)?;
   if !prologues(image).media_fetch.at(image, func) {
      return Err("libdiscord_media::fetch prologue changed".to_owned());
   }
   Ok(func)
}

/// `serde_json::to_value::<Message>`, which Phoenix runs to store a message,
/// and `drop_in_place::<serde_json::Value>`, which frees what it returns.
#[derive(Clone, Copy, Debug)]
pub struct MessageJson {
   /// `fn(out: *mut Result<Value, Error>, message: *const Message)`.
   pub to_value:   usize,
   /// `fn(value: *mut Value)`.
   pub drop_value: usize,
}

/// Locates [`MessageJson`].
///
/// `to_value` is the one function that loads `"mention_everyone"` whose first
/// string is the struct name `Message` and whose next are the fields `id`,
/// `channel_id` and `guild_id`, with a `type` key and no `kind` (another
/// `Message` model's). The drop glue reads `Value`'s tag from the niche in its
/// first word (`5` for an object), and drops an object's index table of
/// 8-byte slots aligned to 16; every copy of it is the same function.
pub fn message_json(image: &Image<'_>) -> Found<MessageJson> {
   const DECODE: Pattern = Pattern::new(
      "48 8B ?? 48 B9 00 00 00 00 00 00 00 80 48 31 C1 48 85 C0 B8 05 00 00 00 48 0F 48 C1 48 83 \
       F8 03",
   );
   let leas = lea_index(image);
   let mut funcs: Vec<(usize, usize)> = image
      .find_bytes(b"mention_everyone")
      .into_iter()
      .flat_map(|name| leas.sites(name))
      .filter_map(|site| image.function_bounds(site))
      .collect();
   funcs.sort_unstable();
   funcs.dedup();
   let starts = |s: &[u8], prefix: &[u8]| s.starts_with(prefix);
   let found: Vec<usize> = funcs
      .into_iter()
      .filter(|&(start, end)| {
         let Some(code) = image.read(start, end - start) else {
            return false;
         };
         let strings: Vec<&[u8]> = rip_leas(code, start)
            .filter_map(|(_, target)| image.read(target, 16))
            .collect();
         let [first, id, channel, guild, ..] = strings.as_slice() else {
            return false;
         };
         starts(first, b"Message")
            && starts(id, b"id")
            && starts(channel, b"channel_id")
            && starts(guild, b"guild_id")
            && strings.iter().any(|s| starts(s, b"type"))
            && !strings.iter().any(|s| starts(s, b"kind"))
      })
      .map(|(start, _)| start)
      .collect();
   let to_value = one("Message to_value", &found)?;

   let table_drop: &[u8] = match Abi::of(image) {
      Abi::Win64 => &[0x41, 0xB8, 8, 0, 0, 0, 0x41, 0xB9, 0x10, 0, 0, 0],
      Abi::SysV => &[0xBA, 8, 0, 0, 0, 0xB9, 0x10, 0, 0, 0],
   };
   let drop_value = image
      .scan(DECODE)
      .into_iter()
      .find_map(|hit| {
         let start = image.function_containing(hit)?;
         let tail = image.read(hit, 0xA0)?;
         (hit - start < 0x30 && tail.windows(table_drop.len()).any(|w| w == table_drop))
            .then_some(start)
      })
      .ok_or("serde_json::Value drop glue not found")?;
   Ok(MessageJson {
      to_value,
      drop_value,
   })
}

/// Each `lea r64, [rip + disp32]` in `code` mapped at `addr`, as (instruction,
/// target).
fn rip_leas(code: &[u8], addr: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
   memchr::memchr_iter(0x8D, code).filter_map(move |pos| {
      let rex = *code.get(pos.checked_sub(1)?)?;
      let modrm = *code.get(pos + 1)?;
      if rex & 0xFB != 0x48 || modrm & 0xC7 != 0x05 {
         return None;
      }
      let disp = i32::from_le_bytes(code.get(pos + 2..pos + 6)?.try_into().ok()?);
      Some((
         addr + pos - 1,
         (addr + pos + 6).checked_add_signed(disp as isize)?,
      ))
   })
}

/// Mana's icon names in id order: a `MenuItem`'s icon is the index of its
/// name in this list.
///
/// The icon table pairs each name with `icons/<Name>.svg`; the paths are read
/// in a run from `icons/A.svg`, and the ids are their names sorted, which
/// Discord's own menu icons (`Copy` 0x99, `Id` 0x15b, `Link` 0x177, `Trash`
/// 0x280 on 2026-11 builds) confirm.
pub fn icon_names(image: &Image<'_>) -> Found<Vec<String>> {
   /// How far apart two paths in the table may sit, a name between them.
   const GAP: usize = 128;
   let start = image
      .find_bytes_iter(b"icons/A.svg")
      .next()
      .ok_or("icon table not found")?;
   let mut names = Vec::new();
   let mut at = start;
   while let Some(window) = image.read(at, GAP) {
      let Some(path) = window.windows(6).position(|w| w == b"icons/") else {
         break;
      };
      let Some(rest) = image.read(at + path + 6, GAP) else {
         break;
      };
      let Some(end) = rest.windows(4).position(|w| w == b".svg") else {
         break;
      };
      let Ok(name) = std::str::from_utf8(&rest[..end]) else {
         break;
      };
      at += path + 6 + end + 4;
      // `Name.secondary.svg` is a second layer of `Name`, with no id.
      if name.contains('.') {
         continue;
      }
      if !name.bytes().all(|byte| byte.is_ascii_alphanumeric())
         || (name == "A" && !names.is_empty())
      {
         break;
      }
      names.push(name.to_owned());
   }
   names.sort_unstable();
   names.dedup();
   let has = |name: &str| {
      names
         .binary_search_by(|probe| probe.as_str().cmp(name))
         .is_ok()
   };
   if names.len() < 100 || !["Copy", "Id", "Link", "Trash"].into_iter().all(has) {
      return Err(format!("icon table holds {} names", names.len()));
   }
   Ok(names)
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn islands_hand_over_the_frame() {
      let text = |frame: Frame, arg: Register| {
         let code = frame.island(arg);
         let lines = phoenix_hook::x86::disasm(&code, 0, usize::MAX);
         assert_eq!(lines.iter().map(|line| line.len).sum::<usize>(), code.len());
         lines
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>()
            .join("; ")
      };
      assert_eq!(text(Frame::Reg(Register::RBP), Register::R9), "mov r9,rbp");
      assert_eq!(text(Frame::Reg(Register::RBP), Register::R8), "mov r8,rbp");
      assert_eq!(
         text(Frame::Reg(Register::RBX), Register::RCX),
         "mov rcx,rbx"
      );
      assert_eq!(
         text(Frame::Reg(Register::R14), Register::RCX),
         "mov rcx,r14"
      );
      assert_eq!(text(Frame::Stack, Register::RCX), "lea rcx,[rsp+8]");
      assert_eq!(text(Frame::Stack, Register::RDX), "lea rdx,[rsp+8]");
      assert_eq!(text(Frame::Stack, Register::R9), "lea r9,[rsp+8]");
   }

   #[test]
   fn frame_slots_parse() {
      assert_eq!(slot_of("[rbp+0DB0h]", "rbp"), Some(0xDB0));
      assert_eq!(slot_of("[rsp+8]", "rsp"), Some(8));
      assert_eq!(slot_of("[rsp]", "rsp"), Some(0));
      assert_eq!(slot_of("[rsp+8]", "rbp"), None);
      assert_eq!(slot_of("[rsp+rax]", "rsp"), None);
   }
}
