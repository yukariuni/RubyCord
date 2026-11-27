//! Forces Phoenix to launch. The launcher method that falls back to the React
//! Native client is found by its log string, since names are obfuscated per
//! build, and made to return true. Phoenix's host name table is found the
//! same way.

use pokedex::{
   checksum::Checksums,
   dex::{
      Dex,
      Method,
   },
};

use crate::Error;

const ANCHOR: &str = "Phoenix didn't stay up for 3 launches";
/// One entry of the host name table, which no other code holds.
const TABLE_ANCHOR: &str = "Networking.request ";
/// `const/4 v0, #1` then `return v0`.
const RETURN_TRUE: [u8; 4] = [0x12, 0x10, 0x0F, 0x00];

/// The methods whose code loads a string starting with `prefix`.
fn loading<'dex>(dex: &'dex Dex, prefix: &str) -> Result<Vec<Method<'dex>>, Error> {
   let strings = dex.strings();
   let anchors = (0..strings.len())
      .filter(|&index| {
         strings
            .get(index)
            .is_some_and(|text| text.starts_with(prefix))
      })
      .collect::<Vec<_>>();
   let mut found = Vec::new();
   if anchors.is_empty() {
      return Ok(found);
   }

   for method in dex.methods() {
      for index in method.string_loads().map_err(Error::Parse)? {
         if anchors.contains(&index.map_err(Error::Parse)?) {
            found.push(method);
            break;
         }
      }
   }
   Ok(found)
}

/// Fills `numbers` from the `"<name> <number>"` entries of Phoenix's host name
/// table, read from the strings of the method that builds it, and fails when
/// the table gives one name two numbers.
pub fn host_numbers(
   data: Vec<u8>,
   names: &[&str],
   numbers: &mut [Option<u16>],
) -> Result<(), Error> {
   let dex = Dex::parse(data).map_err(Error::Parse)?;
   for method in loading(&dex, TABLE_ANCHOR)? {
      for index in method.string_loads().map_err(Error::Parse)? {
         let Some(entry) = dex.strings().get(index.map_err(Error::Parse)?) else {
            continue;
         };
         let Some((name, number)) = entry.rsplit_once(' ') else {
            continue;
         };
         let Ok(number) = number.parse::<u16>() else {
            continue;
         };
         for (wanted, slot) in names.iter().zip(numbers.iter_mut()) {
            if *wanted != name {
               continue;
            }
            match *slot {
               Some(seen) if seen != number => {
                  return Err(Error::Dex(format!(
                     "host name {wanted} is both {seen} and {number}"
                  )));
               },
               _ => *slot = Some(number),
            }
         }
      }
   }
   Ok(())
}

/// Makes the launcher method return true, or `None` when this dex does not
/// hold it.
pub fn patch(data: Vec<u8>) -> Result<Option<Vec<u8>>, Error> {
   let dex = Dex::parse(data).map_err(Error::Parse)?;
   let methods = loading(&dex, ANCHOR)?;
   let method = match &methods[..] {
      [] => return Ok(None),
      [method] => method,
      _ => {
         return Err(Error::Dex(format!(
            "launcher anchor: expected one method, found {}",
            methods.len()
         )));
      },
   };
   let code = method
      .decode()
      .map_err(Error::Parse)?
      .ok_or_else(|| Error::Dex("launcher method has no code".into()))?;
   if !method.prototype().ends_with(")Z") || code.registers() < 1 {
      return Err(Error::Dex(format!(
         "launcher method is {} with {} registers",
         method.prototype(),
         code.registers()
      )));
   }
   let insns = code.instructions_range().start;

   let mut data = dex.into_bytes();
   data
      .get_mut(insns..insns + RETURN_TRUE.len())
      .ok_or(Error::Truncated)?
      .copy_from_slice(&RETURN_TRUE);
   Checksums::rehash(&mut data).map_err(Error::Parse)?;
   Ok(Some(data))
}
