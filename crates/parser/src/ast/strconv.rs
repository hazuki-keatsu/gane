//! A minimal subset of Go's `strconv` package: unquoting of Go string
//! literals, as needed by the `go/ast` port (directive argument parsing).
//!
//! Only double-quoted strings (with the full Go escape set) and back-quoted
//! raw strings are supported. Go error texts are not reproduced; callers wrap
//! failures in their own messages.
//!
//! Escape rules (as in Go):
//!
//! - `\a` `\b` `\f` `\n` `\r` `\t` `\v` `\\` `\'` `\"`
//! - octal escape `\nnn` (up to three octal digits, value <= 255)
//! - `\xhh` (two hex digits), `\uhhhh` (four), `\Uhhhhhhhh` (eight; must be a
//!   valid Unicode scalar value)
//!
//! Raw strings may not contain backquotes; carriage returns (`\r`) inside
//! raw strings are discarded from the value, as in Go.

/// Returns the parsed form of the Go quoted string `s` (which must consist of
/// exactly one literal, quotes included), or `Err(())` if it is malformed.
pub(crate) fn unquote(s: &str) -> Result<String, ()> {
    let (end, value) = scan_quoted(s)?;
    if end != s.len() {
        return Err(());
    }
    Ok(value)
}

/// Returns the Go quoted-string prefix of `s` (quotes included), or `Err(())`
/// if `s` does not start with a valid literal. Mirrors Go's
/// `strconv.QuotedPrefix`.
pub(crate) fn quoted_prefix(s: &str) -> Result<&str, ()> {
    let (end, _) = scan_quoted(s)?;
    Ok(&s[..end])
}

/// Scans a Go quoted or raw string literal at the start of `s`. Returns the
/// number of bytes the literal occupies (including both quotes) and its
/// unquoted value.
fn scan_quoted(s: &str) -> Result<(usize, String), ()> {
    let bytes = s.as_bytes();
    match bytes.first() {
        Some(b'`') => {
            // Raw string: no escapes, backquotes not allowed.
            // Any errors are reported via the value: raw strings may not
            // contain backquotes, and carriage returns are discarded.
            let mut value = String::new();
            let mut i = 1;
            while i < bytes.len() {
                let b = bytes[i];
                if b == b'`' {
                    return Ok((i + 1, value));
                }
                if b != b'\r' {
                    value.push(b as char);
                }
                i += 1;
            }
            Err(())
        }
        Some(b'"') => {
            let mut value = String::new();
            let mut i = 1;
            while i < bytes.len() {
                let b = bytes[i];
                if b == b'"' {
                    return Ok((i + 1, value));
                }
                if b == b'\\' {
                    let (ch, n) = unescape(&bytes[i + 1..])?;
                    value.push(ch);
                    i += 1 + n;
                    continue;
                }
                if b.is_ascii() {
                    value.push(b as char);
                    i += 1;
                    continue;
                }
                // Non-ASCII byte: copy the whole UTF-8 sequence.
                let rest = &s[i..];
                let ch = rest.chars().next().ok_or(())?;
                value.push(ch);
                i += ch.len_utf8();
            }
            Err(()) // unterminated
        }
        _ => Err(()),
    }
}

/// Parses one escape sequence (the bytes following a backslash). Returns the
/// resulting character and the number of bytes consumed.
fn unescape(rest: &[u8]) -> Result<(char, usize), ()> {
    let Some(&b) = rest.first() else {
        return Err(());
    };
    let simple = |c: char| Ok((c, 1));
    match b {
        b'a' => simple('\x07'),
        b'b' => simple('\x08'),
        b'f' => simple('\x0c'),
        b'n' => simple('\n'),
        b'r' => simple('\r'),
        b't' => simple('\t'),
        b'v' => simple('\x0b'),
        b'\\' => simple('\\'),
        b'\'' => simple('\''),
        b'"' => simple('"'),
        b'x' => {
            let v = hex_value(&rest[1..], 2)?;
            Ok((char::from_u32(v as u32).ok_or(())?, 3))
        }
        b'u' => {
            let v = hex_value(&rest[1..], 4)?;
            Ok((char::from_u32(v).ok_or(())?, 5))
        }
        b'U' => {
            let v = hex_value(&rest[1..], 8)?;
            Ok((char::from_u32(v).ok_or(())?, 9))
        }
        b'0'..=b'7' => {
            // octal escape: up to three octal digits (including the first)
            let mut v = 0u32;
            let mut n = 0;
            while n < 3 {
                let Some(&d) = rest.get(n) else { break };
                if !(b'0'..=b'7').contains(&d) {
                    break;
                }
                v = v * 8 + (d - b'0') as u32;
                n += 1;
            }
            if v > 255 {
                return Err(());
            }
            Ok((char::from_u32(v).ok_or(())?, n))
        }
        _ => Err(()),
    }
}

/// Reads `n` hex digits from `rest`.
fn hex_value(rest: &[u8], n: usize) -> Result<u32, ()> {
    if rest.len() < n {
        return Err(());
    }
    let mut v = 0u32;
    for &b in &rest[..n] {
        let d = match b {
            b'0'..=b'9' => (b - b'0') as u32,
            b'a'..=b'f' => (b - b'a' + 10) as u32,
            b'A'..=b'F' => (b - b'A' + 10) as u32,
            _ => return Err(()),
        };
        v = v * 16 + d;
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unquote_double_quoted() {
        let cases = [
            ("\"abc\"", "abc"),
            ("\"foo\\U0001F60Abar\"", "foo\u{1F60A}bar"),
            (
                "\"\\a\\b\\f\\n\\r\\t\\v\\\\\\'\\\"\"",
                "\x07\x08\x0c\n\r\t\x0b\\'\"",
            ),
            ("\"\\x41\\101\"", "AA"),
            ("\"\\u4e2d\"", "中"),
            ("\"\\xFF\"", "\u{FF}"),
        ];
        for (inp, want) in cases {
            assert_eq!(unquote(inp), Ok(want.to_string()), "unquote({inp:?})");
        }
    }

    #[test]
    fn unquote_raw() {
        // Carriage returns inside raw strings are discarded, as in Go.
        assert_eq!(unquote("`foo bar`"), Ok("foo bar".to_string()));
        assert_eq!(unquote("`a\\tb`"), Ok("a\\tb".to_string()));
        assert_eq!(unquote("`foo\r\nbar`"), Ok("foo\nbar".to_string()));
        assert_eq!(unquote("`a\\tb`"), Ok("a\\tb".to_string()));
    }

    #[test]
    fn unquote_errors() {
        let cases = [
            "",                // empty
            "abc",             // not quoted
            "`foo",            // unterminated raw
            "\"foo",           // unterminated quoted
            "\"foo\\q\"",      // unknown escape
            "\"\\x4\"",        // short hex
            "\"\\U00110000\"", // invalid scalar value
            "\"\\777\"",       // octal value > 255
            "\"abc\"x",        // trailing garbage
        ];
        for inp in cases {
            assert!(unquote(inp).is_err(), "unquote({inp:?}) should fail");
        }
    }

    #[test]
    fn quoted_prefix_prefixes() {
        assert_eq!(quoted_prefix("\"foo bar\" baz"), Ok("\"foo bar\""));
        assert_eq!(quoted_prefix("`foo` `bar`"), Ok("`foo`"));
        assert!(quoted_prefix("no quote here").is_err());
        assert!(quoted_prefix("\"unterminated").is_err());
    }
}
