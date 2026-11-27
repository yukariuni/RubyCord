use std::fmt;

use memchr::memmem;

/// A byte-pattern signature such as `"48 8B ?? ?? 89"`. `??` and `?` match
/// any byte.
///
/// Patterns are checked when the constant is evaluated, so a typo fails the
/// build.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Pattern(&'static str);

const fn hex_digit(byte: u8) -> Option<u8> {
   match byte {
      b'0'..=b'9' => Some(byte - b'0'),
      b'a'..=b'f' => Some(byte - b'a' + 10),
      b'A'..=b'F' => Some(byte - b'A' + 10),
      _ => None,
   }
}

const fn valid(source: &str) -> bool {
   let bytes = source.as_bytes();
   let mut index = 0;
   let mut literals = 0;
   while index < bytes.len() {
      if bytes[index] == b' ' {
         index += 1;
         continue;
      }
      let mut end = index;
      while end < bytes.len() && bytes[end] != b' ' {
         end += 1;
      }
      match end - index {
         1 if bytes[index] == b'?' => {},
         2 if bytes[index] == b'?' && bytes[index + 1] == b'?' => {},
         2 if hex_digit(bytes[index]).is_some() && hex_digit(bytes[index + 1]).is_some() => {
            literals += 1;
         },
         _ => return false,
      }
      index = end;
   }
   literals > 0
}

impl Pattern {
   #[must_use]
   pub const fn new(source: &'static str) -> Self {
      assert!(
         valid(source),
         "pattern tokens are two hex digits, `?` or `??`, with at least one literal byte"
      );
      Self(source)
   }

   /// A pattern typed at runtime, like in the debug shell. Each one leaks its
   /// text, which the debug tools can afford.
   #[must_use]
   pub fn leak(source: String) -> Option<Self> {
      valid(&source).then(|| Self(Box::leak(source.into_boxed_str())))
   }

   fn tokens(self) -> Vec<Option<u8>> {
      self
         .0
         .split_ascii_whitespace()
         .map(|token| {
            match token.as_bytes() {
               [high, low] => Some(hex_digit(*high)? << 4 | hex_digit(*low)?),
               _ => None,
            }
         })
         .collect()
   }

   /// Offsets in `haystack` where the whole pattern matches. The longest
   /// literal run is located with `memmem`, then each candidate is checked byte
   /// by byte.
   #[must_use]
   pub fn find_in(self, haystack: &[u8]) -> Vec<usize> {
      let tokens = self.tokens();
      let (mut best, mut run) = ((0, 0), (0, 0));
      for (index, token) in tokens.iter().enumerate() {
         if token.is_some() {
            run = if run.1 == 0 {
               (index, 1)
            } else {
               (run.0, run.1 + 1)
            };
            if run.1 > best.1 {
               best = run;
            }
         } else {
            run = (0, 0);
         }
      }

      let (anchor_start, anchor_len) = best;
      let anchor = tokens[anchor_start..anchor_start + anchor_len]
         .iter()
         .flatten()
         .copied()
         .collect::<Vec<u8>>();

      memmem::find_iter(haystack, &anchor)
         .filter_map(|found| {
            let start = found.checked_sub(anchor_start)?;
            let window = haystack.get(start..start + tokens.len())?;
            let hit = tokens
               .iter()
               .zip(window)
               .all(|(token, byte)| token.is_none_or(|want| want == *byte));
            hit.then_some(start)
         })
         .collect()
   }
}

impl fmt::Display for Pattern {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      formatter.write_str(self.0)
   }
}

impl fmt::Debug for Pattern {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      write!(formatter, "Pattern({:?})", self.0)
   }
}
