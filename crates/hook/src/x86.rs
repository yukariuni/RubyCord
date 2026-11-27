//! x86-64 disassembly, and on Windows and Linux live entry hooks that report a
//! call's argument registers, for the debug tools.

#[cfg(any(windows, target_os = "linux"))]
use std::ffi::c_void;
use std::ops::Range;

pub use iced_x86::Register;
#[cfg(any(windows, target_os = "linux"))]
use iced_x86::{
   BlockEncoder,
   BlockEncoderOptions,
   FlowControl,
   InstructionBlock,
};
use iced_x86::{
   Decoder,
   DecoderOptions,
   Formatter as _,
   Instruction,
   IntelFormatter,
};

use crate::image::Image;
#[cfg(windows)] use crate::image::win32;
#[cfg(any(windows, target_os = "linux"))]
use crate::{
   Error,
   image::{
      ABSOLUTE_JUMP_LEN,
      absolute_jump,
      alloc_exec,
   },
};

/// One decoded instruction.
pub struct Line {
   pub addr:   usize,
   pub len:    usize,
   pub text:   String,
   /// The address a RIP-relative operand or a near branch points at.
   pub target: Option<usize>,
}

/// A decoded address. Only built for 64-bit hosts, where it always fits.
#[expect(clippy::cast_possible_truncation, reason = "x86_64 hosts only")]
const fn addr(value: u64) -> usize {
   value as usize
}

/// Whether `code` at `ip` is whole instructions that run the same from
/// anywhere: no RIP-relative operand and no branch.
#[must_use]
pub fn relocatable(code: &[u8], ip: usize) -> bool {
   let mut decoder = Decoder::with_ip(64, code, ip as u64, DecoderOptions::NONE);
   let mut instruction = Instruction::default();
   while decoder.can_decode() {
      decoder.decode_out(&mut instruction);
      if instruction.is_invalid()
         || instruction.is_ip_rel_memory_operand()
         || instruction.flow_control() != iced_x86::FlowControl::Next
      {
         return false;
      }
   }
   true
}

/// Up to `count` instructions decoded from `code`, which starts at `ip`. Stops
/// early at bytes that do not decode.
#[must_use]
pub fn disasm(code: &[u8], ip: usize, count: usize) -> Vec<Line> {
   let mut decoder = Decoder::with_ip(64, code, ip as u64, DecoderOptions::NONE);
   let mut formatter = IntelFormatter::new();
   let mut instruction = Instruction::default();
   let mut lines = Vec::new();
   let mut offset = 0;
   while lines.len() < count && decoder.can_decode() {
      decoder.decode_out(&mut instruction);
      if instruction.is_invalid() {
         break;
      }
      let mut text = String::new();
      formatter.format(&instruction, &mut text);
      let target = if instruction.is_ip_rel_memory_operand() {
         Some(instruction.ip_rel_memory_address())
      } else if instruction.near_branch_target() != 0 {
         Some(instruction.near_branch_target())
      } else {
         None
      };
      lines.push(Line {
         addr: ip + offset,
         len: instruction.len(),
         text,
         target: target.and_then(|target| usize::try_from(target).ok()),
      });
      offset += instruction.len();
   }
   lines
}

/// The calling convention code in an image follows: Win64 in PE images,
/// System V in ELF ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Abi {
   Win64,
   SysV,
}

/// Where an integer argument sits at the `call`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
   Reg(Register),
   /// `[rsp + offset]`.
   Stack(u64),
}

impl Abi {
   #[must_use]
   pub const fn of(image: &Image<'_>) -> Self {
      if image.pdata.is_some() {
         Self::Win64
      } else {
         Self::SysV
      }
   }

   /// The integer argument registers, in order. The hidden return pointer is
   /// the first.
   #[must_use]
   pub const fn arg_regs(self) -> &'static [Register] {
      match self {
         Self::Win64 => &[Register::RCX, Register::RDX, Register::R8, Register::R9],
         Self::SysV => {
            &[
               Register::RDI,
               Register::RSI,
               Register::RDX,
               Register::RCX,
               Register::R8,
               Register::R9,
            ]
         },
      }
   }

   /// The `index`th integer argument register.
   ///
   /// # Panics
   ///
   /// When `index` is 4 or more, past what Win64 passes in registers.
   #[must_use]
   pub const fn arg(self, index: usize) -> Register {
      assert!(
         index < 4,
         "Win64 passes four integer arguments in registers"
      );
      self.arg_regs()[index]
   }

   /// Where the `index`th integer argument sits at the `call`. Win64 keeps a
   /// 32-byte shadow space below the stacked ones.
   #[must_use]
   pub const fn arg_slot(self, index: usize) -> Slot {
      let regs = self.arg_regs();
      if index < regs.len() {
         return Slot::Reg(regs[index]);
      }
      let shadow = match self {
         Self::Win64 => 0x20,
         Self::SysV => 0,
      };
      Slot::Stack(shadow + 8 * (index - regs.len()) as u64)
   }

   /// The registers a call may clobber.
   #[must_use]
   pub const fn volatile(self) -> &'static [Register] {
      match self {
         Self::Win64 => {
            &[
               Register::RAX,
               Register::RCX,
               Register::RDX,
               Register::R8,
               Register::R9,
               Register::R10,
               Register::R11,
            ]
         },
         Self::SysV => {
            &[
               Register::RAX,
               Register::RCX,
               Register::RDX,
               Register::RSI,
               Register::RDI,
               Register::R8,
               Register::R9,
               Register::R10,
               Register::R11,
            ]
         },
      }
   }
}

/// Tracks the constant an instruction loads: `mov reg, imm`, or `push imm;
/// pop reg`, the form size-optimized builds pick for small constants.
#[derive(Default)]
struct Constants {
   pushed: Option<u64>,
}

impl Constants {
   /// The constant `instruction` writes to its first operand.
   fn loaded(&mut self, instruction: &Instruction) -> Option<u64> {
      use iced_x86::{
         Mnemonic,
         OpKind,
      };
      let immediate = |operand: u32| {
         matches!(
            instruction.op_kind(operand),
            OpKind::Immediate8
               | OpKind::Immediate8to32
               | OpKind::Immediate8to64
               | OpKind::Immediate32
               | OpKind::Immediate32to64
               | OpKind::Immediate64
         )
         .then(|| instruction.immediate(operand))
      };
      match instruction.mnemonic() {
         Mnemonic::Push => {
            self.pushed = immediate(0);
            None
         },
         Mnemonic::Pop => self.pushed.take(),
         Mnemonic::Mov => {
            self.pushed = None;
            immediate(1)
         },
         _ => {
            self.pushed = None;
            None
         },
      }
   }
}

/// What a function does with the struct its `index`th argument points at.
///
/// Like [`sret_stores`], control flow is followed linearly, so a use after the
/// pointer's register is reused is missed and never misread. Unlike it,
/// decoding runs past each `ret` to the end of `code`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ArgUse {
   /// The end of the furthest read or write through the pointer.
   pub extent:     usize,
   /// Each call, with the struct offset each of the first four argument
   /// registers stands for: `lea reg, [ptr+offset]`, or a slot `reg` points
   /// at that a 16-byte move filled from `[ptr+offset]`.
   pub calls:      Vec<[Option<usize>; 4]>,
   /// `rep movsb` copies out of the struct.
   pub copies:     Vec<StructCopy>,
   /// Offsets whose value indexes a jump table.
   pub dispatches: Vec<usize>,
}

/// A `rep movsb` out of the struct [`arg_use`] follows.
#[derive(Debug, PartialEq, Eq)]
pub struct StructCopy {
   pub offset:      usize,
   pub len:         usize,
   /// How many calls come before it.
   pub after_calls: usize,
}

/// What a register holds, as the struct offset it relates to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Holds {
   /// A pointer to the field at this offset.
   Pointer(usize),
   /// The value of the field at this offset.
   Value(usize),
   /// A pointer to a slot copied from the field at this offset.
   Slot(usize),
}

/// The registers [`arg_use`] follows, and what it found so far.
struct ArgTracker {
   held:     Vec<(Register, Holds)>,
   constant: Vec<(Register, u64)>,
   /// Frame slots `[rsp|rbp+displacement]` a held register was saved to.
   spilled:  Vec<((Register, u64), Holds)>,
   found:    ArgUse,
}

impl ArgTracker {
   /// Larger displacements are globals or stack slots, not struct fields.
   const FIELD_LIMIT: u64 = 0x1000;

   fn holds(&self, register: Register) -> Option<Holds> {
      let register = register.full_register();
      self
         .held
         .iter()
         .find(|(held, _)| *held == register)
         .map(|&(_, holds)| holds)
   }

   fn pointer(&self, register: Register) -> Option<usize> {
      match self.holds(register)? {
         Holds::Pointer(offset) => Some(offset),
         _ => None,
      }
   }

   fn value(&self, register: Register) -> Option<usize> {
      match self.holds(register)? {
         Holds::Value(offset) => Some(offset),
         _ => None,
      }
   }

   /// The struct offset `[base+displacement]` reaches, when `base` points
   /// into it.
   fn field(&self, base: Register, displacement: u64) -> Option<usize> {
      (displacement < Self::FIELD_LIMIT)
         .then(|| Some(self.pointer(base)? + addr(displacement)))
         .flatten()
   }

   /// The struct offset of `instruction`'s memory operand, and which operand
   /// it is.
   fn memory_field(&self, instruction: &Instruction) -> Option<(u32, usize)> {
      use iced_x86::OpKind;
      let operand =
         (0..instruction.op_count()).find(|&op| instruction.op_kind(op) == OpKind::Memory)?;
      (instruction.memory_index() == Register::None)
         .then(|| {
            self.field(
               instruction.memory_base(),
               instruction.memory_displacement64(),
            )
         })
         .flatten()
         .map(|offset| (operand, offset))
   }

   fn call(&mut self, abi: Abi) {
      let stands_for = |register| {
         match self.holds(register)? {
            Holds::Pointer(offset) | Holds::Slot(offset) => Some(offset),
            Holds::Value(_) => None,
         }
      };
      let args = [0, 1, 2, 3].map(|arg| stands_for(abi.arg(arg)));
      self.found.calls.push(args);
      self
         .held
         .retain(|(register, _)| !abi.volatile().contains(register));
      self
         .constant
         .retain(|(register, _)| !abi.volatile().contains(register));
   }

   /// Follows a held register through a frame slot: Windows builds keep the
   /// pointer on the stack across calls and reload it before each use.
   fn spill(&mut self, instruction: &Instruction) -> Option<(Register, Holds)> {
      use iced_x86::{
         Code,
         OpKind,
      };
      let base = instruction.memory_base();
      if instruction.memory_index() != Register::None
         || !matches!(base, Register::RSP | Register::RBP)
      {
         return None;
      }
      let slot = (base, instruction.memory_displacement64());
      if instruction.op0_kind() == OpKind::Memory {
         self.spilled.retain(|(held, _)| *held != slot);
         if instruction.code() == Code::Mov_rm64_r64
            && let Some(holds) = self.holds(instruction.op1_register())
         {
            self.spilled.push((slot, holds));
         }
         return None;
      }
      (instruction.code() == Code::Mov_r64_rm64)
         .then(|| self.spilled.iter().find(|(held, _)| *held == slot))
         .flatten()
         .map(|&(_, holds)| (instruction.op0_register().full_register(), holds))
   }

   /// What `instruction` makes its first operand hold, and the copies and
   /// dispatches it shows.
   fn step(&mut self, instruction: &Instruction) -> Option<(Register, Holds)> {
      use iced_x86::{
         Code,
         Mnemonic,
         OpKind,
      };
      let target = (instruction.op0_kind() == OpKind::Register)
         .then(|| instruction.op0_register().full_register());
      let sixteen = matches!(
         instruction.mnemonic(),
         Mnemonic::Movups | Mnemonic::Movaps | Mnemonic::Movdqu | Mnemonic::Movdqa
      );
      match (
         instruction.mnemonic(),
         self.memory_field(instruction),
         target,
      ) {
         (Mnemonic::Lea, Some((1, offset)), Some(target)) => Some((target, Holds::Pointer(offset))),
         (Mnemonic::Mov, Some((1, offset)), Some(target))
            if instruction.code() == Code::Mov_r64_rm64 =>
         {
            Some((target, Holds::Value(offset)))
         },
         (Mnemonic::Mov, None, Some(target))
            if matches!(instruction.code(), Code::Mov_r64_rm64 | Code::Mov_rm64_r64)
               && instruction.op1_kind() == OpKind::Register =>
         {
            Some((target, self.holds(instruction.op1_register())?))
         },
         (_, Some((1, offset)), Some(target)) if sixteen => Some((target, Holds::Value(offset))),
         (..)
            if sixteen
               && instruction.op0_kind() == OpKind::Memory
               && instruction.memory_displacement64() == 0
               && instruction.memory_index() == Register::None =>
         {
            let offset = self.value(instruction.op1_register())?;
            Some((
               instruction.memory_base().full_register(),
               Holds::Slot(offset),
            ))
         },
         (Mnemonic::Movsxd, ..) => {
            let offset = self.value(instruction.memory_index())?;
            self.found.dispatches.push(offset);
            None
         },
         (Mnemonic::Movsb, ..) if instruction.has_rep_prefix() => {
            let offset = self.pointer(Register::RSI)?;
            let &(_, len) = self
               .constant
               .iter()
               .find(|(register, _)| *register == Register::RCX)?;
            self.found.copies.push(StructCopy {
               offset,
               len: addr(len),
               after_calls: self.found.calls.len(),
            });
            None
         },
         _ => None,
      }
   }
}

/// Reads [`ArgUse`] for the struct the `index`th argument points at in `code`
/// at `ip`.
#[must_use]
pub fn arg_use(abi: Abi, index: usize, code: &[u8], ip: usize) -> ArgUse {
   use iced_x86::{
      InstructionInfoFactory,
      Mnemonic,
      OpAccess,
      OpKind,
   };

   let mut decoder = Decoder::with_ip(64, code, ip as u64, DecoderOptions::NONE);
   let mut factory = InstructionInfoFactory::new();
   let mut constants = Constants::default();
   let mut tracker = ArgTracker {
      held:     vec![(abi.arg(index), Holds::Pointer(0))],
      constant: Vec::new(),
      spilled:  Vec::new(),
      found:    ArgUse::default(),
   };
   while decoder.can_decode() {
      let instruction = decoder.decode();
      if instruction.is_invalid() {
         break;
      }
      if instruction.mnemonic() == Mnemonic::Call {
         tracker.call(abi);
         continue;
      }
      let info = factory.info(&instruction);
      for memory in info.used_memory() {
         if memory.index() == Register::None
            && let Some(offset) = tracker.field(memory.base(), memory.displacement())
         {
            tracker.found.extent = tracker
               .found
               .extent
               .max(offset + memory.memory_size().size());
         }
      }
      let new = tracker
         .spill(&instruction)
         .or_else(|| tracker.step(&instruction));
      let loaded = constants.loaded(&instruction);
      let written = info
         .used_registers()
         .iter()
         .filter(|used| {
            matches!(
               used.access(),
               OpAccess::Write
                  | OpAccess::CondWrite
                  | OpAccess::ReadWrite
                  | OpAccess::ReadCondWrite
            )
         })
         .map(|used| used.register().full_register())
         .collect::<Vec<_>>();
      tracker
         .held
         .retain(|(register, _)| !written.contains(register));
      tracker
         .constant
         .retain(|(register, _)| !written.contains(register));
      // A moved frame base, a push among them, leaves its slots elsewhere.
      tracker
         .spilled
         .retain(|((base, _), _)| !written.contains(base));
      if let Some(new) = new {
         tracker.held.retain(|(register, _)| *register != new.0);
         tracker.held.push(new);
      }
      if let Some(value) = loaded
         && instruction.op0_kind() == OpKind::Register
      {
         tracker
            .constant
            .push((instruction.op0_register().full_register(), value));
      }
   }
   tracker.found
}

/// The `(offset, size)` stores a function makes through its hidden return
/// pointer, the first argument register on entry.
///
/// Decoding stops at the first `ret`, and a `memcpy` into the pointer counts
/// as one store of the copied length. It shows how large the returned
/// struct is and which fields the constructor sets, so a layout baked into a
/// caller can be checked against a new build. Control flow is followed
/// linearly, which suits the straight-line constructors it is for.
#[must_use]
pub fn sret_stores(abi: Abi, code: &[u8], ip: usize) -> Vec<(usize, usize)> {
   use iced_x86::{
      Code,
      InstructionInfoFactory,
      Mnemonic,
      OpAccess,
      OpKind,
   };

   let writes = |access: OpAccess| {
      matches!(
         access,
         OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite | OpAccess::ReadCondWrite
      )
   };
   let (dest, len) = (abi.arg(0), abi.arg(2));

   let mut decoder = Decoder::with_ip(64, code, ip as u64, DecoderOptions::NONE);
   let mut factory = InstructionInfoFactory::new();
   let mut constants = Constants::default();
   let mut holders = vec![dest];
   let mut len_imm = None;
   let mut stores = Vec::new();
   while decoder.can_decode() {
      let instruction = decoder.decode();
      if instruction.is_invalid() || instruction.mnemonic() == Mnemonic::Ret {
         break;
      }
      if instruction.mnemonic() == Mnemonic::Call {
         if holders.contains(&dest)
            && let Some(len) = len_imm
         {
            stores.push((0, len));
         }
         holders.retain(|register| !abi.volatile().contains(register));
         len_imm = None;
         continue;
      }
      let constant = constants.loaded(&instruction);
      let info = factory.info(&instruction);
      for memory in info.used_memory() {
         if writes(memory.access())
            && holders.contains(&memory.base())
            && memory.index() == Register::None
         {
            stores.push((addr(memory.displacement()), memory.memory_size().size()));
         }
      }
      let copied = matches!(instruction.code(), Code::Mov_r64_rm64 | Code::Mov_rm64_r64)
         && instruction.op0_kind() == OpKind::Register
         && instruction.op1_kind() == OpKind::Register
         && holders.contains(&instruction.op1_register());
      let written = info
         .used_registers()
         .iter()
         .filter(|used| writes(used.access()))
         .map(|used| used.register().full_register())
         .collect::<Vec<_>>();
      for register in written {
         holders.retain(|&held| held != register);
         if copied && register == instruction.op0_register() {
            holders.push(register);
         }
         if register == len {
            len_imm = constant.and_then(|value| usize::try_from(value).ok());
         }
      }
   }
   stores
}

/// One `DebugStruct::field(name, &self.field)` call in a derived `Debug::fmt`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebugField {
   /// Address and length of the field name literal.
   pub name:     usize,
   pub name_len: usize,
   /// Offset of the field in `self`.
   pub offset:   usize,
   /// The `&dyn Debug` vtable passed for the field, whose slot 3 is its `fmt`.
   pub vtable:   Option<usize>,
}

/// The fields a derived `Debug::fmt` prints, in code order.
///
/// Each field is a call `field(builder, name, len, &self.field, vtable)` with
/// its arguments in `abi`'s slots: a `lea` of the name, a constant length,
/// `lea [self + off]` (or `self` after `add self, off`) and a `lea` of the
/// vtable. `self` is the register the first argument is first copied into.
/// Cold blocks are decoded linearly too, so fields printed out of line count.
#[must_use]
#[expect(clippy::too_many_lines, reason = "one match arm per instruction shape")]
pub fn debug_fields(abi: Abi, code: &[u8], ip: usize) -> Vec<DebugField> {
   use std::collections::HashMap;

   use iced_x86::{
      Code,
      Mnemonic,
      OpKind,
   };

   #[derive(Clone, Copy)]
   enum Value {
      Rip(usize),
      Field(usize),
      Imm(u64),
   }

   let mut decoder = Decoder::with_ip(64, code, ip as u64, DecoderOptions::NONE);
   let mut constants = Constants::default();
   let mut this = None;
   let mut added = 0usize;
   let mut regs: HashMap<Register, Value> = HashMap::new();
   let mut stack: HashMap<u64, Value> = HashMap::new();
   let mut fields = Vec::new();
   while decoder.can_decode() {
      let ins = decoder.decode();
      if ins.is_invalid() {
         continue;
      }
      let constant = constants.loaded(&ins);
      let dst = (ins.op_count() > 0 && ins.op0_kind() == OpKind::Register)
         .then(|| ins.op0_register().full_register());
      let to_this = dst.is_some() && dst == this;
      let self_lea = ins.mnemonic() == Mnemonic::Lea
         && Some(ins.memory_base()) == this
         && ins.memory_index() == Register::None;
      // Any other write to `self`'s register means it no longer holds `self`.
      // The epilogue's `pop` is the exception: cold blocks decoded after the
      // `ret` are reached from the body and still have `self` there.
      if to_this
         && !matches!(
            ins.mnemonic(),
            Mnemonic::Add | Mnemonic::Inc | Mnemonic::Pop
         )
         && !self_lea
      {
         this = None;
         added = 0;
      }
      match (ins.mnemonic(), dst) {
         (Mnemonic::Mov, Some(_))
            if this.is_none()
               && matches!(ins.code(), Code::Mov_r64_rm64 | Code::Mov_rm64_r64)
               && ins.op1_kind() == OpKind::Register
               && ins.op1_register() == abi.arg(0) =>
         {
            this = dst;
         },
         (Mnemonic::Lea, Some(dst)) => {
            let value = if ins.is_ip_rel_memory_operand() {
               Some(Value::Rip(addr(ins.ip_rel_memory_address())))
            } else if self_lea {
               added
                  .checked_add(addr(ins.memory_displacement64()))
                  .map(Value::Field)
            } else {
               None
            };
            match value {
               Some(Value::Field(offset)) if to_this => added = offset,
               Some(value) => {
                  regs.insert(dst, value);
               },
               None => {
                  regs.remove(&dst);
               },
            }
         },
         (Mnemonic::Add, Some(_)) if to_this => {
            // A register, a negative or an overflowing step loses track of
            // `self`.
            if let Some(sum) = ins
               .try_immediate(1)
               .ok()
               .and_then(|step| i64::try_from(step).ok())
               .and_then(|step| usize::try_from(step).ok())
               .and_then(|step| added.checked_add(step))
            {
               added = sum;
            } else {
               this = None;
               added = 0;
            }
         },
         (Mnemonic::Inc, Some(_)) if to_this => added = added.saturating_add(1),
         (Mnemonic::Mov | Mnemonic::Pop, Some(dst)) => {
            let value = match constant {
               Some(value) => Some(Value::Imm(value)),
               None if ins.op_count() < 2 || ins.op1_kind() != OpKind::Register => None,
               None if Some(ins.op1_register().full_register()) == this => {
                  Some(Value::Field(added))
               },
               None => regs.get(&ins.op1_register().full_register()).copied(),
            };
            match value {
               Some(value) => regs.insert(dst, value),
               None => regs.remove(&dst),
            };
         },
         (Mnemonic::Mov, None)
            if ins.op0_kind() == OpKind::Memory
               && ins.memory_base() == Register::RSP
               && ins.memory_index() == Register::None
               && ins.op1_kind() == OpKind::Register =>
         {
            match regs.get(&ins.op1_register().full_register()) {
               Some(&value) => stack.insert(ins.memory_displacement64(), value),
               None => stack.remove(&ins.memory_displacement64()),
            };
         },
         (Mnemonic::Call, _) => {
            let arg = |index: usize| {
               match abi.arg_slot(index) {
                  Slot::Reg(register) => regs.get(&register),
                  Slot::Stack(offset) => stack.get(&offset),
               }
            };
            if let (Some(Value::Rip(name)), Some(Value::Imm(len)), Some(Value::Field(offset))) =
               (arg(1), arg(2), arg(3))
               && (1..64).contains(len)
            {
               fields.push(DebugField {
                  name:     *name,
                  name_len: addr(*len),
                  offset:   *offset,
                  vtable:   match arg(4) {
                     Some(Value::Rip(at)) => Some(*at),
                     _ => None,
                  },
               });
            }
            // `self` lives in a callee-saved register, so it survives the call.
            regs.clear();
            stack.clear();
         },
         (Mnemonic::Jmp | Mnemonic::Ret, _) => {
            regs.clear();
            stack.clear();
            added = 0;
         },
         _ => {},
      }
   }
   fields
}

/// `(offset, value)` of each `mov byte [..], imm8` into the struct a call
/// just returned through its hidden return pointer.
///
/// `before` ends at the call and holds that pointer's `lea arg0, [base +
/// disp]`; `after` starts right behind it and is read up to the next branch
/// or call, through registers loaded with `lea reg, [base + disp]`.
#[must_use]
pub fn sret_byte_stores(
   abi: Abi,
   before: &[u8],
   before_ip: usize,
   after: &[u8],
   after_ip: usize,
) -> Vec<(usize, u8)> {
   use std::collections::HashMap;

   use iced_x86::{
      FlowControl,
      Mnemonic,
      OpKind,
   };

   let mut out = None;
   let mut decoder = Decoder::with_ip(64, before, before_ip as u64, DecoderOptions::NONE);
   while decoder.can_decode() {
      let ins = decoder.decode();
      if ins.is_invalid() {
         continue;
      }
      let writes_sret = ins.op_count() > 0
         && ins.op0_kind() == OpKind::Register
         && ins.op0_register().full_register() == abi.arg(0);
      if writes_sret {
         out = (ins.mnemonic() == Mnemonic::Lea
            && !ins.is_ip_rel_memory_operand()
            && ins.memory_index() == Register::None)
            .then(|| (ins.memory_base(), ins.memory_displacement64().cast_signed()));
      }
   }
   let Some((base, disp)) = out else {
      return Vec::new();
   };

   let mut aliases: HashMap<Register, i64> = HashMap::from([(base, 0)]);
   let mut stores = Vec::new();
   let mut decoder = Decoder::with_ip(64, after, after_ip as u64, DecoderOptions::NONE);
   while decoder.can_decode() {
      let ins = decoder.decode();
      if ins.is_invalid() || ins.flow_control() != FlowControl::Next {
         break;
      }
      if ins.op_count() > 0 && ins.op0_kind() == OpKind::Register {
         let dst = ins.op0_register().full_register();
         if dst != base {
            aliases.remove(&dst);
            if ins.mnemonic() == Mnemonic::Lea
               && ins.memory_base() == base
               && ins.memory_index() == Register::None
            {
               aliases.insert(dst, ins.memory_displacement64().cast_signed() - disp);
            }
         }
         continue;
      }
      if ins.mnemonic() == Mnemonic::Mov
         && ins.op0_kind() == OpKind::Memory
         && ins.memory_size().size() == 1
         && ins.op1_kind() == OpKind::Immediate8
         && ins.memory_index() == Register::None
         && let Some(&delta) = aliases.get(&ins.memory_base())
      {
         let at = ins.memory_displacement64().cast_signed() + delta
            - if ins.memory_base() == base { disp } else { 0 };
         if let Ok(at) = usize::try_from(at) {
            stores.push((at, ins.immediate8()));
         }
      }
   }
   stores
}

/// The length and callee of the call starting `code`, which sits at `ip`:
/// `call rel32`, or `call [rip + slot]` through a pointer to code, the form
/// ELF builds without a PLT take for most calls.
#[must_use]
pub fn call_target(image: &Image<'_>, code: &[u8], ip: usize) -> Option<(usize, usize)> {
   let rel = |at: usize| {
      let bytes = code.get(at..at + 4)?;
      Some(i32::from_le_bytes(bytes.try_into().ok()?) as isize)
   };
   match code {
      [0xE8, ..] => Some((5, (ip + 5).checked_add_signed(rel(1)?)?)),
      [0xFF, 0x15, ..] => {
         let slot = (ip + 6).checked_add_signed(rel(2)?)?;
         let callee = image
            .read_usize(slot)
            .filter(|&callee| image.is_code(callee))?;
         Some((6, callee))
      },
      _ => None,
   }
}

/// The target of the `jmp rel32` or `jmp [rip + slot]` starting `code`,
/// which sits at `ip`.
#[must_use]
pub fn jump_target(image: &Image<'_>, code: &[u8], ip: usize) -> Option<usize> {
   let rel = |at: usize| {
      let bytes = code.get(at..at + 4)?;
      Some(i32::from_le_bytes(bytes.try_into().ok()?) as isize)
   };
   match code {
      [0xE9, ..] => (ip + 5).checked_add_signed(rel(1)?),
      [0xFF, 0x25, ..] => image.read_usize((ip + 6).checked_add_signed(rel(2)?)?),
      _ => None,
   }
}

/// An instruction that points at an address, and the function holding it.
pub struct CodeRef {
   pub function: usize,
   pub line:     Line,
}

/// The instructions in `image` whose RIP-relative operand or near branch
/// points into `targets`, confirmed by decoding their function.
#[must_use]
pub fn code_refs(image: &Image<'_>, targets: Range<usize>) -> Vec<CodeRef> {
   image
      .rip_refs(targets)
      .into_iter()
      .filter_map(|(field, target)| {
         let (start, end) = image.function_bounds(field)?;
         let line = disasm(image.read(start, end - start)?, start, usize::MAX)
            .into_iter()
            .find(|line| (line.addr..line.addr + line.len).contains(&field))?;
         (line.target == Some(target)).then_some(CodeRef {
            function: start,
            line,
         })
      })
      .collect()
}

/// How far before a match a string literal can start. Rust merges literals
/// into one blob, so code loads the start of the literal, not the match.
const LITERAL_REACH: usize = 512;

/// The code that loads the string literal holding the bytes at `at`: the
/// loads of the closest literal start at or before it.
#[must_use]
pub fn literal_loads(image: &Image<'_>, at: usize) -> Vec<CodeRef> {
   let mut loads = code_refs(image, at.saturating_sub(LITERAL_REACH)..at + 1);
   let start = loads.iter().filter_map(|load| load.line.target).max();
   loads.retain(|load| load.line.target == start);
   loads
}

/// Copies `len` bytes of this process's memory at `addr`, or the readable
/// start of them. Unmapped memory gives `None` instead of a fault.
#[cfg(windows)]
#[must_use]
pub fn read_memory(addr: usize, len: usize) -> Option<Vec<u8>> {
   let mut buffer = vec![0u8; len];
   let mut read = 0usize;
   // SAFETY: `buffer` holds `len` writable bytes, and ReadProcessMemory
   // checks the source range instead of faulting.
   let ok = unsafe {
      win32::ReadProcessMemory(
         win32::GetCurrentProcess(),
         addr as *const c_void,
         buffer.as_mut_ptr().cast(),
         len,
         &raw mut read,
      )
   };
   (ok != 0 || read > 0).then(|| {
      buffer.truncate(read);
      buffer
   })
}

/// Copies `len` bytes of this process's memory at `addr`, or the readable
/// start of them. Unmapped memory gives `None` instead of a fault.
#[cfg(target_os = "linux")]
#[must_use]
pub fn read_memory(addr: usize, len: usize) -> Option<Vec<u8>> {
   let mut buffer = vec![0u8; len];
   let local = libc::iovec {
      iov_base: buffer.as_mut_ptr().cast(),
      iov_len:  len,
   };
   let remote = libc::iovec {
      iov_base: addr as *mut c_void,
      iov_len:  len,
   };
   // SAFETY: `buffer` holds `len` writable bytes, and process_vm_readv checks
   // the source range instead of faulting.
   let read = unsafe {
      libc::process_vm_readv(libc::getpid(), &raw const local, 1, &raw const remote, 1, 0)
   };
   let read = usize::try_from(read).ok().filter(|&read| read > 0)?;
   buffer.truncate(read);
   Some(buffer)
}

/// Copies `len` bytes of this process's memory at `addr`, or the readable
/// start of them. Unmapped memory gives `None` instead of a fault.
#[cfg(target_os = "macos")]
#[must_use]
pub fn read_memory(addr: usize, len: usize) -> Option<Vec<u8>> {
   unsafe extern "C" {
      static mach_task_self_: u32;
      fn mach_vm_read_overwrite(
         task: u32,
         address: u64,
         size: u64,
         data: u64,
         out_size: *mut u64,
      ) -> i32;
   }

   let mut buffer = vec![0u8; len];
   let mut read = 0u64;
   // SAFETY: `buffer` holds `len` writable bytes, and the kernel checks the
   // source range instead of faulting.
   let status = unsafe {
      mach_vm_read_overwrite(
         mach_task_self_,
         addr as u64,
         len as u64,
         buffer.as_mut_ptr() as u64,
         &raw mut read,
      )
   };
   let read = usize::try_from(read)
      .ok()
      .filter(|&read| status == 0 && read > 0)?;
   buffer.truncate(read);
   Some(buffer)
}

/// The registers at a hooked function's entry, as the stub saved them.
#[cfg(windows)]
#[repr(C)]
pub struct Regs {
   pub r9:  u64,
   pub r8:  u64,
   pub rdx: u64,
   pub rcx: u64,
   /// The caller's return address.
   pub ret: u64,
}

#[cfg(windows)]
impl Regs {
   /// The first four integer arguments, `rcx`, `rdx`, `r8` and `r9`.
   #[must_use]
   pub const fn args(&self) -> [u64; 4] {
      [self.rcx, self.rdx, self.r8, self.r9]
   }
}

/// The registers at a hooked function's entry, as the stub saved them. `rax`
/// carries the vector register count into variadic functions.
#[cfg(target_os = "linux")]
#[repr(C)]
pub struct Regs {
   pub r9:  u64,
   pub r8:  u64,
   pub rcx: u64,
   pub rdx: u64,
   pub rsi: u64,
   pub rdi: u64,
   pub rax: u64,
   /// The caller's return address.
   pub ret: u64,
}

#[cfg(target_os = "linux")]
impl Regs {
   /// The first four integer arguments, `rdi`, `rsi`, `rdx` and `rcx`.
   #[must_use]
   pub const fn args(&self) -> [u64; 4] {
      [self.rdi, self.rsi, self.rdx, self.rcx]
   }
}

/// Called on every entry of an observed function, on the caller's thread,
/// with the context passed to [`observe`]. It must not unwind.
#[cfg(any(windows, target_os = "linux"))]
pub type Observer = extern "C" fn(ctx: usize, regs: &Regs);

/// A function [`observe`] redirects through its stub.
#[cfg(any(windows, target_os = "linux"))]
pub struct Observed {
   addr:     usize,
   original: [u8; ABSOLUTE_JUMP_LEN],
}

#[cfg(any(windows, target_os = "linux"))]
const STUB_PAGE: usize = 0x1000;
#[cfg(any(windows, target_os = "linux"))]
const TRAMPOLINE_AT: usize = 0x100;

#[cfg(any(windows, target_os = "linux"))]
impl Observed {
   #[must_use]
   pub const fn addr(&self) -> usize {
      self.addr
   }

   /// Puts the function's first bytes back. The stub and trampoline stay
   /// allocated, since a thread may still run them.
   ///
   /// # Safety
   ///
   /// As for [`observe`].
   pub unsafe fn remove(&self, image: &Image<'static>) -> Result<(), Error> {
      // SAFETY: forwarded from the caller.
      unsafe { hot_patch(image, self.addr, &self.original) }
   }
}

/// Makes every call to the function at `addr` run `observer(ctx, regs)` first,
/// then the function unchanged.
///
/// The displaced prologue is decoded and relocated, so unlike
/// [`Image::hook_entry`] it needs no known pattern. The entry is patched while
/// other threads run: one that reaches it meanwhile spins on a `jmp $` until
/// the jump is whole.
///
/// # Safety
///
/// `addr` starts a function of `image` that nothing branches back into within
/// its first 14 bytes, and no thread is paused inside those bytes.
#[cfg(any(windows, target_os = "linux"))]
pub unsafe fn observe(
   image: &Image<'static>,
   addr: usize,
   observer: Observer,
   ctx: usize,
) -> Result<Observed, Error> {
   let refuse = |message: &str| {
      Error::Attach {
         backend: "observe",
         target:  addr,
         message: message.to_owned(),
      }
   };
   if addr % 8 == 7 {
      return Err(refuse(
         "the entry ends an 8-byte word, so it cannot be parked atomically",
      ));
   }
   if let Some((start, end)) = image.function_bounds(addr) {
      if start != addr {
         return Err(refuse("not a function start"));
      }
      if end - addr < ABSOLUTE_JUMP_LEN {
         return Err(refuse("the function is shorter than the 14-byte jump"));
      }
   }
   let code = image.read(addr, 32).ok_or(Error::Unmapped { addr })?;
   let original: [u8; ABSOLUTE_JUMP_LEN] = code[..ABSOLUTE_JUMP_LEN]
      .try_into()
      .map_err(|_| Error::Unmapped { addr })?;
   let mut decoder = Decoder::with_ip(64, code, addr as u64, DecoderOptions::NONE);
   let mut displaced = Vec::new();
   let mut covered = 0;
   while covered < ABSOLUTE_JUMP_LEN {
      let instruction = decoder.decode();
      if instruction.is_invalid() {
         return Err(refuse("the prologue does not decode"));
      }
      covered += instruction.len();
      let ends = matches!(
         instruction.flow_control(),
         FlowControl::Return
            | FlowControl::UnconditionalBranch
            | FlowControl::IndirectBranch
            | FlowControl::Interrupt
      );
      if ends && covered < ABSOLUTE_JUMP_LEN {
         return Err(refuse("the function ends within the 14-byte jump"));
      }
      displaced.push(instruction);
   }

   let page = alloc_near(addr).ok_or_else(|| refuse("no free memory within 1.75 GiB"))?;
   let trampoline = page + TRAMPOLINE_AT;
   let mut relocated = BlockEncoder::encode(
      64,
      InstructionBlock::new(&displaced, trampoline as u64),
      BlockEncoderOptions::NONE,
   )
   .map_err(|err| refuse(&format!("relocating the prologue: {err}")))?
   .code_buffer;
   relocated.extend(absolute_jump(addr + covered));
   let stub = stub(observer, ctx, trampoline);
   debug_assert!(stub.len() <= TRAMPOLINE_AT && relocated.len() <= STUB_PAGE - TRAMPOLINE_AT);
   // SAFETY: a fresh page of `STUB_PAGE` bytes, filled before the entry
   // jumps to it.
   unsafe {
      std::ptr::copy_nonoverlapping(stub.as_ptr(), page as *mut u8, stub.len());
      std::ptr::copy_nonoverlapping(relocated.as_ptr(), trampoline as *mut u8, relocated.len());
   }
   // SAFETY: forwarded from the caller.
   unsafe { hot_patch(image, addr, &absolute_jump(page)) }?;
   Ok(Observed { addr, original })
}

/// Writes 14 bytes at a function entry other threads may be calling: first a
/// `jmp $` that parks them, then the tail, then the head.
#[cfg(any(windows, target_os = "linux"))]
unsafe fn hot_patch(
   image: &Image<'static>,
   addr: usize,
   bytes: &[u8; ABSOLUTE_JUMP_LEN],
) -> Result<(), Error> {
   // SAFETY: `addr % 8 != 7`, so each two-byte write is one atomic word swap,
   // and the tail is only reached through the head.
   unsafe {
      image.write_code(addr, &[0xEB, 0xFE])?;
      image.write_code(addr + 2, &bytes[2..])?;
      image.write_code(addr, &bytes[..2])
   }
}

/// A fresh executable page within reach of RIP-relative operands at `addr`.
/// The kernel takes the address as a hint, so a page it put elsewhere goes
/// back.
#[cfg(target_os = "linux")]
fn alloc_near(addr: usize) -> Option<usize> {
   const STEP: usize = 0x1_0000;
   const REACH: usize = 0x7000_0000;
   let home = addr & !(STEP - 1);
   (1..REACH / STEP)
      .flat_map(|step| [home.checked_sub(step * STEP), home.checked_add(step * STEP)])
      .flatten()
      .find_map(|candidate| {
         let got = alloc_exec(candidate, STUB_PAGE)?;
         if got.abs_diff(addr) < REACH {
            return Some(got);
         }
         // SAFETY: the fresh mapping nothing else uses.
         unsafe { libc::munmap(got as *mut c_void, STUB_PAGE) };
         None
      })
}

/// A fresh executable page within reach of RIP-relative operands at `addr`.
#[cfg(windows)]
fn alloc_near(addr: usize) -> Option<usize> {
   const GRANULARITY: usize = 0x1_0000;
   const REACH: usize = 0x7000_0000;
   let home = addr & !(GRANULARITY - 1);
   (1..REACH / GRANULARITY)
      .flat_map(|step| {
         [
            home.checked_sub(step * GRANULARITY),
            home.checked_add(step * GRANULARITY),
         ]
      })
      .flatten()
      .find_map(|candidate| alloc_exec(candidate, STUB_PAGE))
}

/// Saves the argument registers and `rax`, calls `observer(ctx, &regs)` on a
/// 16-byte aligned stack, restores them and jumps to `trampoline`.
#[cfg(target_os = "linux")]
fn stub(observer: Observer, ctx: usize, trampoline: usize) -> Vec<u8> {
   // push rax, rdi, rsi, rdx, rcx, r8, r9; sub rsp, 0x80
   let mut code = vec![
      0x50, 0x57, 0x56, 0x52, 0x51, 0x41, 0x50, 0x41, 0x51, 0x48, 0x81, 0xEC, 0x80, 0x00, 0x00, 0x00,
   ];
   for xmm in 0..8u8 {
      // movdqu [rsp + xmm * 16], xmmN
      code.extend([0xF3, 0x0F, 0x7F, 0x44 | xmm << 3, 0x24, xmm * 16]);
   }
   code.extend([0x48, 0x8D, 0xB4, 0x24, 0x80, 0x00, 0x00, 0x00]); // lea rsi, [rsp+0x80]
   code.extend([0x48, 0xBF]); // mov rdi, imm64
   code.extend(ctx.to_le_bytes());
   code.extend([0x48, 0xB8]); // mov rax, imm64
   code.extend((observer as usize).to_le_bytes());
   code.extend([0xFF, 0xD0]); // call rax
   for xmm in 0..8u8 {
      // movdqu xmmN, [rsp + xmm * 16]
      code.extend([0xF3, 0x0F, 0x6F, 0x44 | xmm << 3, 0x24, xmm * 16]);
   }
   // add rsp, 0x80; pop r9, r8, rcx, rdx, rsi, rdi, rax
   code.extend([
      0x48, 0x81, 0xC4, 0x80, 0x00, 0x00, 0x00, 0x41, 0x59, 0x41, 0x58, 0x59, 0x5A, 0x5E, 0x5F, 0x58,
   ]);
   code.extend(absolute_jump(trampoline));
   code
}

/// Saves the argument registers, calls `observer(ctx, &regs)` on a 16-byte
/// aligned stack with its shadow space, restores them and jumps to
/// `trampoline`.
#[cfg(windows)]
fn stub(observer: Observer, ctx: usize, trampoline: usize) -> Vec<u8> {
   let mut code = vec![
      0x51, 0x52, 0x41, 0x50, 0x41, 0x51, // push rcx, rdx, r8, r9
      0x48, 0x83, 0xEC, 0x68, // sub rsp, 0x68
      0xF3, 0x0F, 0x7F, 0x44, 0x24, 0x20, // movdqu [rsp+0x20], xmm0
      0xF3, 0x0F, 0x7F, 0x4C, 0x24, 0x30, // movdqu [rsp+0x30], xmm1
      0xF3, 0x0F, 0x7F, 0x54, 0x24, 0x40, // movdqu [rsp+0x40], xmm2
      0xF3, 0x0F, 0x7F, 0x5C, 0x24, 0x50, // movdqu [rsp+0x50], xmm3
      0x48, 0x8D, 0x54, 0x24, 0x68, // lea rdx, [rsp+0x68]
      0x48, 0xB9, // mov rcx, imm64
   ];
   code.extend(ctx.to_le_bytes());
   code.extend([0x48, 0xB8]); // mov rax, imm64
   code.extend((observer as usize).to_le_bytes());
   code.extend([
      0xFF, 0xD0, // call rax
      0xF3, 0x0F, 0x6F, 0x44, 0x24, 0x20, // movdqu xmm0, [rsp+0x20]
      0xF3, 0x0F, 0x6F, 0x4C, 0x24, 0x30, // movdqu xmm1, [rsp+0x30]
      0xF3, 0x0F, 0x6F, 0x54, 0x24, 0x40, // movdqu xmm2, [rsp+0x40]
      0xF3, 0x0F, 0x6F, 0x5C, 0x24, 0x50, // movdqu xmm3, [rsp+0x50]
      0x48, 0x83, 0xC4, 0x68, // add rsp, 0x68
      0x41, 0x59, 0x41, 0x58, 0x5A, 0x59, // pop r9, r8, rdx, rcx
   ]);
   code.extend(absolute_jump(trampoline));
   code
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn sret_stores_follow_the_return_pointer() {
      let code = [
         0x48, 0x89, 0xC8, // mov rax, rcx
         0x88, 0x90, 0x23, 0x03, 0x00, 0x00, // mov [rax+0x323], dl
         0x48, 0x83, 0xA0, 0x80, 0x00, 0x00, 0x00, 0x00, // and qword ptr [rax+0x80], 0
         0x48, 0x89, 0x51, 0x08, // mov [rcx+8], rdx
         0x48, 0x89, 0xC1, // mov rcx, rax
         0x41, 0xB8, 0x40, 0x00, 0x00, 0x00, // mov r8d, 0x40
         0xE8, 0x00, 0x00, 0x00, 0x00, // call memcpy
         0x48, 0x89, 0x48, 0x10, // mov [rax+0x10], rcx: rax no longer holds it
         0xC3, // ret
         0x48, 0x89, 0x01, // mov [rcx], rax: past the ret
      ];
      assert_eq!(sret_stores(Abi::Win64, &code, 0x1000), [
         (0x323, 1),
         (0x80, 8),
         (8, 8),
         (0, 0x40)
      ]);
   }

   #[test]
   fn arg_use_reads_send_in_the_linux_build_from_2026_11_23() {
      // Pieces of `ReqwestTransport::send` from the build whose request grew
      // past 0xF0 bytes.
      let code = [
         0x48, 0x89, 0xF3, // mov rbx, rsi
         0xE8, 0x00, 0x00, 0x00, 0x00, // call
         0x48, 0x8B, 0x83, 0x08, 0x01, 0x00, 0x00, // mov rax, [rbx+0x108]
         0x48, 0x8D, 0x94, 0x24, 0x80, 0x09, 0x00, 0x00, // lea rdx, [rsp+0x980]
         0x48, 0x89, 0x42, 0x10, // mov [rdx+0x10], rax
         0x0F, 0x10, 0x83, 0xF8, 0x00, 0x00, 0x00, // movups xmm0, [rbx+0xF8]
         0x0F, 0x29, 0x02, // movaps [rdx], xmm0
         0x48, 0x8D, 0x8B, 0xB0, 0x00, 0x00, 0x00, // lea rcx, [rbx+0xB0]
         0xE8, 0x00, 0x00, 0x00, 0x00, // call: the request from method and URL
         0x6A, 0x60, // push 0x60
         0x59, // pop rcx
         0x48, 0x89, 0xD7, // mov rdi, rdx
         0x48, 0x89, 0xDE, // mov rsi, rbx
         0xF3, 0xA4, // rep movsb: the headers
         0x48, 0x8B, 0x43, 0x78, // mov rax, [rbx+0x78]
         0x49, 0x63, 0x04, 0x87, // movsxd rax, [r15+rax*4]: the body's tag
         0xC3, // ret
         0x40, 0x8A, 0xAB, 0x62, 0x01, 0x00, 0x00, // mov bpl, [rbx+0x162]: past the ret
      ];
      let used = arg_use(Abi::SysV, 1, &code, 0x1000);
      assert_eq!(used.extent, 0x163);
      assert_eq!(used.calls, [[None, Some(0), None, None], [
         None,
         None,
         Some(0xF8),
         Some(0xB0)
      ]]);
      assert_eq!(used.copies, [StructCopy {
         offset:      0,
         len:         0x60,
         after_calls: 2,
      }]);
      assert_eq!(used.dispatches, [0x78]);
   }

   #[test]
   fn arg_use_follows_the_request_through_a_frame_slot() {
      // Pieces of `ReqwestTransport::send` from the Windows build, which keeps
      // the request on the stack and reloads it before each use.
      let code = [
         0x48, 0x89, 0x95, 0xB0, 0x0B, 0x00, 0x00, // mov [rbp+0xBB0], rdx
         0xE8, 0x00, 0x00, 0x00, 0x00, // call
         0x48, 0x8B, 0x95, 0xB0, 0x0B, 0x00, 0x00, // mov rdx, [rbp+0xBB0]
         0x48, 0x8D, 0x5A, 0x60, // lea rbx, [rdx+0x60]
         0x48, 0x8B, 0x03, // mov rax, [rbx]
         0x48, 0x63, 0x04, 0x81, // movsxd rax, [rcx+rax*4]: the body's tag
         0x48, 0x8B, 0x8D, 0xB0, 0x0B, 0x00, 0x00, // mov rcx, [rbp+0xBB0]
         0x48, 0x8B, 0x81, 0xD0, 0x00, 0x00, 0x00, // mov rax, [rcx+0xD0]
         0x4C, 0x8D, 0x85, 0xB0, 0x07, 0x00, 0x00, // lea r8, [rbp+0x7B0]
         0x49, 0x89, 0x40, 0x10, // mov [r8+0x10], rax
         0x0F, 0x10, 0x81, 0xC0, 0x00, 0x00, 0x00, // movups xmm0, [rcx+0xC0]
         0x41, 0x0F, 0x29, 0x00, // movaps [r8], xmm0
         0x48, 0x8B, 0x81, 0xA8, 0x00, 0x00, 0x00, // mov rax, [rcx+0xA8]
         0x4C, 0x8D, 0x8D, 0xD0, 0x09, 0x00, 0x00, // lea r9, [rbp+0x9D0]
         0x49, 0x89, 0x41, 0x10, // mov [r9+0x10], rax
         0x0F, 0x10, 0x81, 0x98, 0x00, 0x00, 0x00, // movups xmm0, [rcx+0x98]
         0x41, 0x0F, 0x29, 0x01, // movaps [r9], xmm0
         0x48, 0x8D, 0x4D, 0xA0, // lea rcx, [rbp-0x60]
         0xE8, 0x00, 0x00, 0x00, 0x00, // call: the request from method and URL
         0x6A, 0x60, // push 0x60
         0x59, // pop rcx
         0x4C, 0x8D, 0x85, 0xD0, 0x09, 0x00, 0x00, // lea r8, [rbp+0x9D0]
         0x4C, 0x89, 0xC7, // mov rdi, r8
         0x48, 0x8B, 0xB5, 0xB0, 0x0B, 0x00, 0x00, // mov rsi, [rbp+0xBB0]
         0xF3, 0xA4, // rep movsb: the headers
      ];
      let used = arg_use(Abi::Win64, 1, &code, 0x1000);
      assert_eq!(used.calls, [[None, Some(0), None, None], [
         None,
         Some(0),
         Some(0xC0),
         Some(0x98)
      ]]);
      assert_eq!(used.copies, [StructCopy {
         offset:      0,
         len:         0x60,
         after_calls: 2,
      }]);
      assert_eq!(used.dispatches, [0x60]);
   }

   #[test]
   fn debug_fields_follow_sysv_argument_registers() {
      // The head of `<StyleRefinement as Debug>::fmt` in the Linux build: the
      // name in `rsi`, its length in `edx`, the field in `rcx` and the vtable
      // in `r8`.
      let code = [
         0x55, 0x53, 0x48, 0x83, 0xEC, 0x18, 0x48, 0x89, 0xFB, 0x48, 0x8D, 0x15, 0x84, 0x1C, 0x56,
         0xF0, 0x48, 0x8D, 0x7C, 0x24, 0x08, 0xB9, 0x0F, 0x00, 0x00, 0x00, 0xFF, 0x15, 0xA8, 0x02,
         0x95, 0x04, 0x80, 0xBB, 0x77, 0x02, 0x00, 0x00, 0xFF, 0x40, 0x0F, 0x95, 0xC5, 0x74, 0x25,
         0x48, 0x8D, 0x8B, 0x77, 0x02, 0x00, 0x00, 0x48, 0x8D, 0x35, 0x8D, 0x0C, 0x3F, 0xEF, 0x4C,
         0x8D, 0x05, 0x06, 0x51, 0x5C, 0x04, 0x48, 0x8D, 0x7C, 0x24, 0x08, 0xBA, 0x07, 0x00, 0x00,
         0x00, 0xFF, 0x15, 0x6E, 0xAD, 0x9E, 0x04,
      ];
      assert_eq!(debug_fields(Abi::SysV, &code, 0x1546_E250), [DebugField {
         name:     0x485_EF18,
         name_len: 7,
         offset:   0x277,
         vtable:   Some(0x19A3_3398),
      }]);
      assert_eq!(debug_fields(Abi::Win64, &code, 0x1546_E250), []);
   }

   #[cfg(windows)]
   extern "C" fn ignore(_: usize, _: &Regs) {}

   #[cfg(windows)]
   #[test]
   fn stub_decodes_as_written() {
      let stub = stub(ignore, 7, 0x1000);
      // The jump's 8-byte target is data.
      let code = &stub[..stub.len() - 8];
      let text = disasm(code, 0, 64)
         .into_iter()
         .map(|line| line.text)
         .collect::<Vec<_>>();
      assert_eq!(text[..6], [
         "push rcx",
         "push rdx",
         "push r8",
         "push r9",
         "sub rsp,68h",
         "movdqu [rsp+20h],xmm0"
      ]);
      assert!(text.contains(&"lea rdx,[rsp+68h]".to_owned()));
      assert!(text.contains(&"mov rcx,7".to_owned()));
      assert!(text.contains(&"call rax".to_owned()));
      assert_eq!(text[text.len() - 6..text.len() - 1], [
         "add rsp,68h",
         "pop r9",
         "pop r8",
         "pop rdx",
         "pop rcx"
      ]);
      assert!(
         text
            .last()
            .is_some_and(|last| last.starts_with("jmp qword ptr"))
      );
      assert_eq!(
         code.len(),
         disasm(code, 0, 64)
            .iter()
            .map(|line| line.len)
            .sum::<usize>()
      );
   }
}
