//! Where Android Phoenix (`liblibdiscord_wasm_host.so`) keeps what the runtime
//! patches, shared by the runtime and `sigscan check`.

use phoenix_hook::{
   image::Image,
   pattern::Pattern,
};

use crate::{
   Found,
   StaffField,
   one,
};

/// `flag_override` in the arm64 store state.
///
/// Read from the tail of the inlined flag read: `add x8, x8, #page`, the
/// three `flag_override` loads and the `csel` that ORs in the low bit. Both the
/// page immediate (0x8000 on 356207/357200, 0x9000 on 357201) and the load
/// offsets are read back from each match, and every match must agree. 6 copies
/// on 356207, 357200, and 357201.
pub fn staff_aarch64(image: &Image<'_>) -> Found<StaffField> {
   const CHECK: Pattern = Pattern::new(
      "08 ?? ?? 91 09 ?? ?? F9 3F 09 00 F1 61 00 00 54 F3 03 1F 2A ?? 00 00 14 0A ?? ?? B9 0B ?? \
       ?? B9 0C ?? ?? B9 5F 01 00 71 6A 11 9F 1A",
   );
   // `add x8, x8, #imm{, lsl #12}`
   let add = |word: u32| {
      ((word & 0xFF80_03FF) == 0x9100_0108)
         .then(|| ((word >> 10) & 0xFFF) as usize * if (word >> 22) & 1 == 1 { 0x1000 } else { 1 })
   };
   // 32-bit `ldr wt, [xn, #imm]`, scaled by 4
   let ldr = |word: u32| (((word >> 10) & 0xFFF) as usize) << 2;
   let mut found = image
      .scan(CHECK)
      .into_iter()
      .filter_map(|at| {
         let base = add(image.read_u32(at)?)?;
         Some(StaffField {
            tag:   base + ldr(image.read_u32(at + 24)?),
            value: base + ldr(image.read_u32(at + 28)?),
         })
      })
      .collect::<Vec<_>>();
   found.sort_unstable();
   found.dedup();
   one("staff flag_override", &found)
}

/// Start of Phoenix's armeabi-v7a staff check, `bool f(&Store, int force)`.
///
/// Outlined there, with 19 callers on 356207. Thumb, so the hook adds bit 0.
/// The whole `movw` of the store offset is wildcarded: its immediate moved on
/// 357200, which changes the encoding's second byte.
pub fn staff_arm(image: &Image<'_>) -> Found<usize> {
   const IS_STAFF: Pattern =
      Pattern::new("10 B5 84 B0 39 B9 03 46 00 68 ?? ?? ?? ?? 40 58 90 F8 ?? ?? 08 B1 01 24");
   one("staff check", &image.scan(IS_STAFF))
}

/// Start of `getEmojiUnavailableReason`, `u8 f(&Emoji, &EmojiContext)`, which
/// reads the context's intention and answers 3 for profiles.
pub fn emoji_aarch64(image: &Image<'_>) -> Found<usize> {
   const CHECK: Pattern = Pattern::new(
      "28 90 40 39 09 01 1F 32 3F 39 00 71 ?? ?? ?? 54 68 00 80 52 E0 03 08 2A C0 03 5F D6",
   );
   one("emoji check", &image.scan(CHECK))
}

/// Thumb-2 start of the same check: `ldrb.w r12, [r1, #intention]`, then
/// `orr.w r2, r12, #2; cmp r2, #14` and the `3` for profiles.
pub fn emoji_arm(image: &Image<'_>) -> Found<usize> {
   const CHECK: Pattern = Pattern::new("91 F8 ?? C0 4C F0 02 02 0E 2A ?? D1 03 22 10 46 70 47");
   one("emoji check", &image.scan(CHECK))
}
