// finds libdiscord_gateway's SocketSink vtable and start_send slot (slot 4)

use memchr::memchr_iter;
use phoenix_hook::{
   image::Image,
   x86,
};

use crate::Found;

const ADOPT_ANCHOR: &[u8] = b"gateway connect adopted the early socket";
const START_SEND_PANIC: &[u8] = b"assertion failed: this.state.is_none()";
const START_SEND_SLOT_OFFSET: usize = 32;

// WsMessage::Text(String) tag and payload offset in libdiscord_gateway::transport
pub const WS_MESSAGE_TEXT_TAG: u64 = 2;

#[derive(Debug, Clone)]
pub struct DesktopGatewaySend {
   pub start_send_slots: Vec<usize>,
   pub start_send_fn:    usize,
   pub ok_tag:           u64,
}

impl DesktopGatewaySend {
   pub fn locate(image: &Image<'_>) -> Found<Self> {
      if image.pointer_width != 8 {
         return Err("not a 64-bit image".to_owned());
      }

      let anchor = crate::one("gateway adopt anchor", &image.find_bytes(ADOPT_ANCHOR))?;
      let lea = *crate::desktop::lea_refs(image, anchor)
         .first()
         .ok_or("gateway adopt anchor is never loaded")?;
      let (open_fn, open_end) = image
         .function_bounds(lea)
         .ok_or("gateway connect_adopting has no unwind entry")?;

      let panic_addrs = image.find_bytes(START_SEND_PANIC);
      if panic_addrs.is_empty() {
         return Err("start_send panic string not found".to_owned());
      }

      let mut matches = Vec::new();
      for vtable in rip_leas(image, open_fn, open_end) {
         let Some(start_send) = sink_vtable_start_send(image, vtable) else {
            continue;
         };
         if !loads_any(image, start_send, &panic_addrs) {
            continue;
         }
         let Some(ok_tag) = read_ok_tag(image, start_send) else {
            continue;
         };
         matches.push((vtable, start_send, ok_tag));
      }
      matches.sort_unstable();
      matches.dedup_by_key(|&mut (_, start_send, _)| start_send);
      let (vtable, start_send_fn, ok_tag) = crate::one("SocketSink vtable", &matches)?;

      let mut start_send_slots = find_sink_slots(image, start_send_fn);
      let direct = vtable + START_SEND_SLOT_OFFSET;
      if !start_send_slots.contains(&direct) {
         start_send_slots.push(direct);
      }
      start_send_slots.sort_unstable();

      Ok(Self {
         start_send_slots,
         start_send_fn,
         ok_tag,
      })
   }
}

fn rip_leas(image: &Image<'_>, start: usize, end: usize) -> Vec<usize> {
   let Some(code) = image.read(start, end - start) else {
      return Vec::new();
   };
   let mut out = Vec::new();
   for pos in memchr_iter(0x8D, code) {
      if pos == 0 || pos + 6 > code.len() {
         continue;
      }
      let rex = code[pos - 1];
      let modrm = code[pos + 1];
      if (rex != 0x48 && rex != 0x4C) || (modrm & 0xC7) != 0x05 {
         continue;
      }
      let Ok(bytes) = code[pos + 2..pos + 6].try_into() else {
         continue;
      };
      let disp = i32::from_le_bytes(bytes);
      if let Some(target) = (start + pos + 6).checked_add_signed(disp as isize) {
         out.push(target);
      }
   }
   out
}

fn sink_vtable_start_send(image: &Image<'_>, vtable: usize) -> Option<usize> {
   if !vtable.is_multiple_of(8) {
      return None;
   }
   let drop_fn = image.read_usize(vtable)?;
   let size = image.read_usize(vtable + 8)?;
   let align = image.read_usize(vtable + 16)?;
   let poll_ready = image.read_usize(vtable + 24)?;
   let start_send = image.read_usize(vtable + START_SEND_SLOT_OFFSET)?;
   let poll_flush = image.read_usize(vtable + 40)?;
   let poll_close = image.read_usize(vtable + 48)?;
   let valid = (0x20..=0x2000).contains(&size)
      && align.is_power_of_two()
      && align <= 64
      && [drop_fn, poll_ready, start_send, poll_flush, poll_close]
         .into_iter()
         .all(|func| image.is_function(func));
   valid.then_some(start_send)
}

fn loads_any(image: &Image<'_>, func: usize, needles: &[usize]) -> bool {
   let Some((start, end)) = image.function_bounds(func) else {
      return false;
   };
   rip_leas(image, start, end)
      .into_iter()
      .any(|target| needles.contains(&target))
}

// reads the Ok(()) niche discriminant start_send writes to *sret before returning
fn read_ok_tag(image: &Image<'_>, func: usize) -> Option<u64> {
   let (start, end) = image.function_bounds(func)?;
   let code = image.read(start, end - start)?;
   let mut reg_fallback = None;
   for line in x86::disasm(code, start, 128) {
      let text = line.text.as_str();
      if !text.starts_with("mov ") {
         continue;
      }
      let Some((lhs, rhs)) = text.split_once(',') else {
         continue;
      };
      let Some(tag) = parse_ok_tag(rhs.trim()) else {
         continue;
      };
      if lhs.contains('[') {
         return Some(tag);
      }
      reg_fallback.get_or_insert(tag);
   }
   reg_fallback
}

fn parse_ok_tag(rhs: &str) -> Option<u64> {
   if rhs == "-2" {
      return Some(u64::MAX - 1);
   }
   if rhs == "-1" {
      return Some(u64::MAX);
   }
   let hex = rhs
      .strip_prefix("0x")
      .or_else(|| rhs.strip_prefix("0X"))
      .or_else(|| rhs.strip_suffix('h'))
      .or_else(|| rhs.strip_suffix('H'))?;
   let value = u64::from_str_radix(hex, 16).ok()?;
   match value {
      0xFFFF_FFFF_FFFF_FFFE | 0xFFFF_FFFF_FFFF_FFFF => Some(value),
      _ => None,
   }
}

fn find_sink_slots(image: &Image<'_>, start_send: usize) -> Vec<usize> {
   let mut slots = Vec::new();
   for slot in image.pointers_to(start_send) {
      let Some(vtable) = slot.checked_sub(START_SEND_SLOT_OFFSET) else {
         continue;
      };
      if sink_vtable_start_send(image, vtable) == Some(start_send) {
         slots.push(slot);
      }
   }
   slots.sort_unstable();
   slots.dedup();
   slots
}

#[cfg(test)]
mod tests {
   use std::borrow::Cow;

   use phoenix_hook::image::{
      Image,
      Segment,
   };

   use super::*;

   fn rip_lea_rax(pc: usize, target: usize) -> [u8; 7] {
      let disp = i32::try_from(target.cast_signed() - (pc + 7).cast_signed()).unwrap();
      let mut out = [0x48, 0x8D, 0x05, 0, 0, 0, 0];
      out[3..7].copy_from_slice(&disp.to_le_bytes());
      out
   }

   #[test]
   fn locates_socket_sink_vtable_and_ok_tag() {
      let text_base: usize = 0x1000_0000;
      let rdata_base: usize = 0x1001_0000;
      let mut text = vec![0x90u8; 0x400];
      let mut rdata = vec![0u8; 0x400];

      let adopt_va = rdata_base + 0x10;
      rdata[0x10..0x10 + ADOPT_ANCHOR.len()].copy_from_slice(ADOPT_ANCHOR);
      let panic_va = rdata_base + 0x80;
      rdata[0x80..0x80 + START_SEND_PANIC.len()].copy_from_slice(START_SEND_PANIC);

      let open_fn = text_base;
      let drop_fn = text_base + 0x40;
      let poll_ready = text_base + 0x50;
      let start_send = text_base + 0x60;
      let poll_flush = text_base + 0xA0;
      let poll_close = text_base + 0xB0;

      let vtable_va = rdata_base + 0x100;
      let entries: [u64; 7] = [
         drop_fn as u64,
         0x78,
         8,
         poll_ready as u64,
         start_send as u64,
         poll_flush as u64,
         poll_close as u64,
      ];
      for (i, word) in entries.into_iter().enumerate() {
         rdata[0x100 + i * 8..0x108 + i * 8].copy_from_slice(&word.to_le_bytes());
      }

      // open_fn loads adopt anchor and vtable
      let lea1 = rip_lea_rax(open_fn, adopt_va);
      text[0x00..0x07].copy_from_slice(&lea1);
      let lea2 = rip_lea_rax(open_fn + 0x07, vtable_va);
      text[0x07..0x0E].copy_from_slice(&lea2);
      text[0x0E] = 0xC3;

      // start_send loads panic anchor and writes -2 to [rdi]
      let lea_p = rip_lea_rax(start_send, panic_va);
      text[0x60..0x67].copy_from_slice(&lea_p);
      // mov qword ptr [rdi], -2 ; ret
      text[0x67..0x6F].copy_from_slice(&[0x48, 0xC7, 0x07, 0xFE, 0xFF, 0xFF, 0xFF, 0xC3]);

      for off in [0x40, 0x50, 0xA0, 0xB0] {
         text[off] = 0xC3;
      }

      let pdata_off = 0x200;
      let pdata_entries: [(u32, u32, u32); 6] = [
         (0x00, 0x20, 0),
         (0x40, 0x48, 0),
         (0x50, 0x58, 0),
         (0x60, 0x90, 0),
         (0xA0, 0xA8, 0),
         (0xB0, 0xB8, 0),
      ];
      let mut pdata_len = 0;
      for (s, e, u) in pdata_entries {
         rdata[pdata_off + pdata_len..pdata_off + pdata_len + 4].copy_from_slice(&s.to_le_bytes());
         rdata[pdata_off + pdata_len + 4..pdata_off + pdata_len + 8]
            .copy_from_slice(&e.to_le_bytes());
         rdata[pdata_off + pdata_len + 8..pdata_off + pdata_len + 12]
            .copy_from_slice(&u.to_le_bytes());
         pdata_len += 12;
      }

      let image = Image {
         path:            "Discord.exe".to_owned(),
         build_id:        None,
         segments:        vec![
            Segment {
               addr:       text_base,
               bytes:      Cow::Borrowed(&text),
               executable: true,
               writable:   false,
            },
            Segment {
               addr:       rdata_base,
               bytes:      Cow::Borrowed(&rdata),
               executable: false,
               writable:   false,
            },
         ],
         eh_frame_hdr:    None,
         arm_exidx:       None,
         pdata:           Some((text_base, rdata_base + pdata_off, pdata_len)),
         function_starts: None,
         pointer_width:   8,
      };

      let found = DesktopGatewaySend::locate(&image).expect("should locate SocketSink");
      assert_eq!(found.start_send_fn, start_send);
      assert_eq!(found.start_send_slots, vec![vtable_va + START_SEND_SLOT_OFFSET]);
      assert_eq!(found.ok_tag, u64::MAX - 1);
   }
}
