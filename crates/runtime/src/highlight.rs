//! Token colors for JavaScript and JSON syntax highlighting across mobile and
//! desktop views.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
   Plain,
   Comment,
   String,
   Keyword,
   Number,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonKind {
   Plain,
   Key,
   String,
   Number,
   Bool,
   Null,
   Punctuation,
}

const KEYWORDS: &[&str] = &[
   "async",
   "await",
   "break",
   "case",
   "catch",
   "class",
   "const",
   "continue",
   "default",
   "delete",
   "do",
   "else",
   "export",
   "extends",
   "false",
   "finally",
   "for",
   "from",
   "function",
   "if",
   "import",
   "in",
   "instanceof",
   "let",
   "new",
   "null",
   "of",
   "return",
   "static",
   "super",
   "switch",
   "this",
   "throw",
   "true",
   "try",
   "typeof",
   "undefined",
   "var",
   "void",
   "while",
   "yield",
];

/// Splits JavaScript `source` into colored runs that together are all of it.
// ponytail: no regex literals or template `${}` nesting; both read as strings
// or plain text, which only miscolors, never drops text.
#[must_use]
pub fn js(source: &str) -> Vec<(Kind, &str)> {
   let bytes = source.as_bytes();
   let len = bytes.len();
   let mut runs: Vec<(Kind, usize, usize)> = Vec::new();
   let mut at = 0;
   while at < len {
      let start = at;
      let byte = bytes[at];
      let next = bytes.get(at + 1).copied();
      let kind = match byte {
         b'/' if next == Some(b'/') => {
            at = source[at..].find('\n').map_or(len, |end| at + end);
            Kind::Comment
         },
         b'/' if next == Some(b'*') => {
            at = source[at + 2..]
               .find("*/")
               .map_or(len, |end| at + 2 + end + 2);
            Kind::Comment
         },
         b'"' | b'\'' | b'`' => {
            at += 1;
            while at < len {
               match bytes[at] {
                  b'\\' => at += 2,
                  quote if quote == byte => {
                     at += 1;
                     break;
                  },
                  b'\n' if byte != b'`' => break,
                  _ => at += 1,
               }
            }
            at = at.min(len);
            // An escape may have stepped into a multibyte character.
            while !source.is_char_boundary(at) {
               at += 1;
            }
            Kind::String
         },
         b'0'..=b'9' => {
            while at < len && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'.') {
               at += 1;
            }
            Kind::Number
         },
         _ if byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$' => {
            while at < len
               && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_' || bytes[at] == b'$')
            {
               at += 1;
            }
            if KEYWORDS.contains(&&source[start..at]) {
               Kind::Keyword
            } else {
               Kind::Plain
            }
         },
         _ => {
            at += 1;
            while at < len && !source.is_char_boundary(at) {
               at += 1;
            }
            Kind::Plain
         },
      };
      match runs.last_mut() {
         Some(last) if last.0 == kind && last.2 == start => last.2 = at,
         _ => runs.push((kind, start, at)),
      }
   }
   runs
      .into_iter()
      .map(|(kind, start, end)| (kind, &source[start..end]))
      .collect()
}

/// Splits JSON `source` into colored runs distinguishing object keys, string
/// values, numbers, booleans, `null`, and structural punctuation.
#[must_use]
pub fn json(source: &str) -> Vec<(JsonKind, &str)> {
   let bytes = source.as_bytes();
   let len = bytes.len();
   let mut runs: Vec<(JsonKind, usize, usize)> = Vec::new();
   let mut at = 0;
   while at < len {
      let start = at;
      let byte = bytes[at];
      let kind = match byte {
         b'"' => {
            at += 1;
            while at < len {
               match bytes[at] {
                  b'\\' => at += 2,
                  b'"' => {
                     at += 1;
                     break;
                  },
                  b'\n' => break,
                  _ => at += 1,
               }
            }
            at = at.min(len);
            while !source.is_char_boundary(at) {
               at += 1;
            }
            // Look ahead past spaces/tabs for `:` to distinguish JSON object keys.
            let mut look = at;
            while look < len && matches!(bytes[look], b' ' | b'\t') {
               look += 1;
            }
            if look < len && bytes[look] == b':' {
               JsonKind::Key
            } else {
               JsonKind::String
            }
         },
         b'-' if bytes.get(at + 1).is_some_and(u8::is_ascii_digit) => {
            at += 1;
            while at < len
               && (bytes[at].is_ascii_digit()
                  || matches!(bytes[at], b'.' | b'e' | b'E' | b'+' | b'-'))
            {
               at += 1;
            }
            JsonKind::Number
         },
         b'0'..=b'9' => {
            while at < len
               && (bytes[at].is_ascii_digit()
                  || matches!(bytes[at], b'.' | b'e' | b'E' | b'+' | b'-'))
            {
               at += 1;
            }
            JsonKind::Number
         },
         b'{' | b'}' | b'[' | b']' | b':' | b',' => {
            at += 1;
            JsonKind::Punctuation
         },
         _ if byte.is_ascii_alphabetic() => {
            while at < len && bytes[at].is_ascii_alphabetic() {
               at += 1;
            }
            match &source[start..at] {
               "true" | "false" => JsonKind::Bool,
               "null" => JsonKind::Null,
               _ => JsonKind::Plain,
            }
         },
         _ => {
            at += 1;
            while at < len && !source.is_char_boundary(at) {
               at += 1;
            }
            JsonKind::Plain
         },
      };
      match runs.last_mut() {
         Some(last) if last.0 == kind && last.2 == start => last.2 = at,
         _ => runs.push((kind, start, at)),
      }
   }
   runs
      .into_iter()
      .map(|(kind, start, end)| (kind, &source[start..end]))
      .collect()
}

/// Returns true if `text` looks like JSON (starts with `{` or `[` after
/// whitespace).
#[must_use]
pub fn looks_like_json(text: &str) -> bool {
   matches!(text.trim_start().as_bytes().first(), Some(b'{' | b'['))
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn covers_the_source_and_colors_tokens() {
      let source = "const a = \"x\\\"y\"; // hé\nlet n = 1.5 /* c */ + `t` + 'é\\é';\nreturn ünï;";
      let runs = js(source);
      assert_eq!(runs.iter().map(|(_, run)| *run).collect::<String>(), source);
      let kind = |text: &str| {
         runs
            .iter()
            .find(|(_, run)| *run == text)
            .map(|(kind, _)| *kind)
      };
      assert_eq!(kind("const"), Some(Kind::Keyword));
      assert_eq!(kind("\"x\\\"y\""), Some(Kind::String));
      assert_eq!(kind("// hé"), Some(Kind::Comment));
      assert_eq!(kind("1.5"), Some(Kind::Number));
      assert_eq!(kind("/* c */"), Some(Kind::Comment));
      assert_eq!(kind("'é\\é'"), Some(Kind::String));
      assert_eq!(js("\"unterminated \\"), vec![(
         Kind::String,
         "\"unterminated \\"
      )]);
   }

   #[test]
   fn json_distinguishes_keys_strings_numbers_bools_and_null() {
      let src = "{\n  \"id\": \"123\",\n  \"ok\": true,\n  \"count\": -42.5,\n  \"err\": null\n}";
      let runs = json(src);
      assert_eq!(runs.iter().map(|(_, run)| *run).collect::<String>(), src);
      let kind = |text: &str| {
         runs
            .iter()
            .find(|(_, run)| *run == text)
            .map(|(kind, _)| *kind)
      };
      assert_eq!(kind("\"id\""), Some(JsonKind::Key));
      assert_eq!(kind("\"123\""), Some(JsonKind::String));
      assert_eq!(kind("true"), Some(JsonKind::Bool));
      assert_eq!(kind("-42.5"), Some(JsonKind::Number));
      assert_eq!(kind("null"), Some(JsonKind::Null));
      assert_eq!(kind(":"), Some(JsonKind::Punctuation));
   }
}
