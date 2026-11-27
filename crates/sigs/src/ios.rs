//! Locators for iOS Phoenix, `PhoenixFatClient.framework`, where the Swift host
//! and the Rust guest share one image.

use std::borrow::Cow;

use phoenix_hook::{
   image::Image,
   pattern::Pattern,
};

use crate::{
   Found,
   one,
};

/// The host's call into the guest, which takes `{data, vtable}` and calls
/// `vtable[dispatch](data, bytes, len, out)`: `mov x19, x3; ldp x8, x9, [x0];
/// cmp x2, #0; csinc x1, x1, xzr, ne; ldr x9, [x9, #0x20]; mov x0, x8; blr x9`.
const BRIDGE: Pattern = Pattern::new(
   "F3 03 03 AA 08 24 40 A9 5F 00 00 F1 21 14 9F 9A 29 ?? ?? F9 E0 03 08 AA 20 01 3F D6",
);

/// The guest boxing itself for the host, once per constructor: `mov w0, #0x18;
/// bl malloc; cbz x0; ldur q0; str q0, [x0]; ldr x8; str x8, [x0, #0x10];
/// adrp x8, VTABLE; add x8, x8, VTABLE; stp x0, x8, [sp, #8]`.
const GUEST_BOX: Pattern = Pattern::new(
   "00 03 80 52 ?? ?? ?? 94 E0 01 00 B4 E0 83 C0 3C 00 00 80 3D E8 0F 40 F9 08 08 00 F9 ?? ?? ?? \
    ?? ?? ?? ?? 91 E0 A3 00 A9 E0 23 00 91 E1 03 13 AA",
);

/// The guest wrapping the host's callbacks before it boxes itself: `adrp x1,
/// VTABLE; add x1, x1, VTABLE; add x8, sp, #8; bl new`, then [`GUEST_BOX`].
const HOST_BOX: Pattern = Pattern::new(
   "?? ?? ?? ?? 21 ?? ?? 91 E8 23 00 91 ?? ?? ?? 97 00 03 80 52 ?? ?? ?? 94 E0 01 00 B4",
);

/// `call(data, bytes, len)` in the host callbacks' vtable, its first method
/// after drop, size and align. The answer comes back through `x8`.
pub const HOST_CALL: usize = 0x18;

/// The two vtables between the Swift host and the Rust guest.
pub struct Bridge {
   pub call:        usize,
   /// The guest's, which the host calls every view and host event through.
   pub vtable:      usize,
   /// Offset of `dispatch` in `vtable`, 0x20 on 357.0, after drop, size,
   /// align and one method.
   pub dispatch:    usize,
   /// The host callbacks', which the guest makes every host call through.
   pub host_vtable: usize,
}

pub fn bridge(image: &Image<'_>) -> Found<Bridge> {
   let call = one("guest bridge", &image.scan(BRIDGE))?;
   let load = image
      .read(call + 16, 4)
      .ok_or("guest bridge: the vtable load is unmapped")?;
   let load = u32::from_le_bytes([load[0], load[1], load[2], load[3]]);
   // `ldr x9, [x9, #imm]`, whose 12-bit immediate counts words.
   if load & 0xFFC0_03FF != 0xF940_0129 {
      return Err(format!("guest bridge at {call:#x}: no vtable load into x9"));
   }
   let dispatch = ((load >> 10) & 0xFFF) as usize * 8;

   let boxes = image.scan(GUEST_BOX);
   let vtables = boxes
      .iter()
      .map(|&at| adrp_add(image, at + 28, 8))
      .collect::<Found<Vec<usize>>>()?;
   let vtable = match vtables[..] {
      [first, ref rest @ ..] if rest.iter().all(|&other| other == first) => first,
      _ => {
         return Err(format!(
            "guest vtable: {} boxing sites name {} vtables",
            boxes.len(),
            vtables.len()
         ));
      },
   };
   let entry = image
      .read_usize(vtable + dispatch)
      .ok_or("guest vtable: the dispatch slot is unmapped")?;
   if !image.is_code(entry) {
      return Err(format!(
         "guest vtable at {vtable:#x}: dispatch {entry:#x} is not code"
      ));
   }

   let host_box = one("host callbacks", &image.scan(HOST_BOX))?;
   let host_vtable = adrp_add(image, host_box, 1)?;
   let host_call = image
      .read_usize(host_vtable + HOST_CALL)
      .ok_or("host callbacks: the call slot is unmapped")?;
   if !image.is_code(host_call) {
      return Err(format!(
         "host vtable at {host_vtable:#x}: call {host_call:#x} is not code"
      ));
   }
   Ok(Bridge {
      call,
      vtable,
      dispatch,
      host_vtable,
   })
}

/// The address an `adrp xN; add xN, xN, #lo` pair at `at` builds.
fn adrp_add(image: &Image<'_>, at: usize, register: u32) -> Found<usize> {
   let words = image
      .read(at, 8)
      .ok_or_else(|| format!("{at:#x}: the address load is unmapped"))?;
   let adrp = u32::from_le_bytes([words[0], words[1], words[2], words[3]]);
   let add = u32::from_le_bytes([words[4], words[5], words[6], words[7]]);
   // `adrp` and an unshifted 64-bit `add` with the same source and target.
   if adrp & 0x9F00_001F != 0x9000_0000 | register
      || add & 0xFFC0_03FF != 0x9100_0000 | register << 5 | register
   {
      return Err(format!("{at:#x}: no adrp and add into x{register}"));
   }
   let pages = ((((adrp >> 5) & 0x7_FFFF) << 2) | ((adrp >> 29) & 3)).cast_signed() << 11 >> 11;
   let offset = isize::try_from(i64::from(pages) << 12).map_err(|err| err.to_string())?;
   let low = ((add >> 10) & 0xFFF) as usize;
   Ok((at & !0xFFF).wrapping_add_signed(offset) + low)
}

/// Phoenix's numbers for host modules, methods, events and fields, by name.
///
/// The host keeps them in Swift `[String: Int]` literals in its writable
/// data, runs of 24-byte entries of a `String` then an `Int`. A name the
/// entries give two numbers comes back `None`, like one they lack.
#[must_use]
pub fn host_names(image: &Image<'_>, names: &[&str]) -> Vec<Option<u16>> {
   #[derive(Clone, Copy)]
   enum Seen {
      Not,
      Once(u16),
      Conflict,
   }

   let mut seen = vec![Seen::Not; names.len()];
   for segment in image.segments.iter().filter(|segment| segment.writable) {
      let words = segment.bytes.as_chunks::<8>().0;
      let entry = |at: usize| {
         let [low, high, number] = words.get(at..at + 3)? else {
            return None;
         };
         let number = u16::try_from(i64::from_le_bytes(*number)).ok()?;
         Some((swift_string(image, *low, *high)?, number))
      };
      for at in 0..words.len() {
         let Some((text, number)) = entry(at) else {
            continue;
         };
         // A lone `{String, Int}` that Swift wrote at runtime is no table.
         if entry(at + 3).is_none() && at.checked_sub(3).and_then(entry).is_none() {
            continue;
         }
         for (name, slot) in names.iter().zip(&mut seen) {
            if name.as_bytes() != &*text {
               continue;
            }
            *slot = match *slot {
               Seen::Not => Seen::Once(number),
               Seen::Once(first) if first == number => Seen::Once(first),
               _ => Seen::Conflict,
            };
         }
      }
   }
   seen
      .into_iter()
      .map(|seen| {
         match seen {
            Seen::Once(number) => Some(number),
            Seen::Not | Seen::Conflict => None,
         }
      })
      .collect()
}

/// The bytes of a Swift `String` held as two words: up to 15 ASCII bytes
/// inline, or an immortal literal's count and the address 32 bytes before its
/// bytes.
fn swift_string<'a>(image: &'a Image<'_>, low: [u8; 8], high: [u8; 8]) -> Option<Cow<'a, [u8]>> {
   if high[7] & 0xF0 == 0xE0 {
      let count = usize::from(high[7] & 0x0F);
      let mut inline = [0; 15];
      inline[..8].copy_from_slice(&low);
      inline[8..].copy_from_slice(&high[..7]);
      let (text, rest) = inline.split_at(count);
      return (rest.iter().all(|&byte| byte == 0) && text.iter().all(u8::is_ascii_graphic))
         .then(|| Cow::Owned(text.to_vec()));
   }
   let flags = u64::from_le_bytes(low);
   let object = u64::from_le_bytes(high);
   if flags >> 56 != 0xD0 || object >> 63 == 0 {
      return None;
   }
   let count = usize::try_from(flags & 0xFFFF_FFFF_FFFF).ok()?;
   let address = usize::try_from(object & 0x00FF_FFFF_FFFF_FFFF).ok()? + 32;
   (count <= 64)
      .then(|| image.read(address, count))
      .flatten()
      .map(Cow::Borrowed)
}

/// The two `ClientAccessGate` evaluators the launch gate reads before running
/// Phoenix. Each starts 8 bytes before this body, which loads the gate and
/// walks the experiment table. Both must become `mov w0, #1; ret` to open the
/// gate: patching only one crashes the client, since they share a lock.
///
/// Read on 357.0 (build 124398). The experiment-table reference locates one of
/// them, so the body is the anchor that also catches its twin. The two loads'
/// field offsets are wildcards: the `ldrb` moved from 0x42d to 0x83d on the
/// 2026-11-24 build while the code around it stayed the same.
const ACCESS_GATE: Pattern = Pattern::new(
   "09 01 40 F9 3F 09 00 F1 C0 00 00 54 08 ?? ?? F9 1F 05 00 B1 60 00 00 54 08 ?? ?? 39 68 00 00 \
    36",
);

/// The first bytes of a Hermes bytecode bundle like `main.jsbundle`.
pub const HERMES_MAGIC: [u8; 8] = 0x1F19_03C1_03BC_1FC6_u64.to_le_bytes();

/// The `2026-11-phoenix-switch-entrypoint` experiment's definition in the
/// Hermes bytecode, three `NewObjectWithBufferLong`s: its name, its
/// `{enabled: false}` default config and its `{enabled: true}` variation.
/// Read on Discord 357.0 (1.12.2), where the name's buffer ids anchor it.
const SWITCH_ENTRYPOINT: Pattern = Pattern::new(
   "02 01 0D 02 00 00 D0 AD 0F 00 02 07 ?? ?? ?? ?? ?? ?? ?? ?? 52 01 07 02 01 07 ?? ?? ?? ?? 02 \
    08 ?? ?? ?? ?? ?? ?? ?? ?? 58 07 08 01",
);

/// The key and value buffer ids of a `NewObjectWithBufferLong`, after its
/// opcode and register.
pub const OBJECT_BUFFERS: usize = 8;

/// File offsets of the buffer ids of the Phoenix switch experiment's two
/// configs in `main.jsbundle`. Discord offers the switch to staff, developers
/// and users whose config of this experiment is enabled.
pub struct SwitchEntrypoint {
   pub default_config: usize,
   pub enabled_config: usize,
}

pub fn switch_entrypoint(bundle: &[u8]) -> Found<SwitchEntrypoint> {
   let at = one(
      "phoenix switch entrypoint",
      &SWITCH_ENTRYPOINT.find_in(bundle),
   )?;
   Ok(SwitchEntrypoint {
      default_config: at + 12,
      enabled_config: at + 32,
   })
}

/// The two access-gate evaluators' entry points, which open the gate when both
/// return `Some(true)`.
pub fn access_gate(image: &Image<'_>) -> Found<[usize; 2]> {
   match image.scan(ACCESS_GATE)[..] {
      [a, b] => Ok([a - 8, b - 8]),
      ref hits => {
         Err(format!(
            "access gate: expected 2 evaluators, found {}",
            hits.len()
         ))
      },
   }
}
