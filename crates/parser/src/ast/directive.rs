//! Parsing of compiler directive comments.
//!
//! Ported from Go's standard `go/ast/directive.go`.
//!
//! A directive is a comment of this form:
//!
//! ```text
//! //tool:name args
//! ```
//!
//! For example, this directive:
//!
//! ```text
//! //go:generate stringer -type Op -trimprefix Op
//! ```
//!
//! would have Tool "go", Name "generate", and Args "stringer -type Op
//! -trimprefix Op".
//!
//! While Args does not have a strict syntax, by convention it is a
//! space-separated sequence of unquoted words, `"`-quoted Go strings, or
//! `` ` ``-quoted raw strings.
//!
//! See <https://go.dev/doc/comment#directives> for specification.
//!
//! (Note: Go defines the `isDirective` predicate in ast.go for its comment
//! machinery, which is not ported; the predicate lives in the test module
//! here because its only remaining consumer is the equivalence test.)

use std::fmt;

use crate::token::Pos;

use super::strconv;

/// A Directive is a comment of the form `//tool:name args`.
#[derive(Clone, Debug, PartialEq)]
pub struct Directive {
    pub tool: String,
    pub name: String,
    pub args: String, // no leading or trailing whitespace

    /// Slash is the position of the "//" at the beginning of the directive.
    pub slash: Pos,

    /// ArgsPos is the position where Args begins, based on the position
    /// passed to [`parse_directive`].
    pub args_pos: Pos,
}

impl Directive {
    pub fn pos(&self) -> Pos {
        self.slash
    }

    pub fn end(&self) -> Pos {
        self.args_pos + self.args.len() as i64
    }

    /// Parses the directive's arguments using the standard convention, which
    /// is a sequence of tokens, where each token may be a bare word, or a
    /// double quoted Go string, or a back quoted raw Go string. Each token
    /// must be separated by one or more Unicode spaces.
    ///
    /// If the arguments do not conform to this syntax, it returns an error.
    pub fn parse_args(&self) -> Result<Vec<DirectiveArg>, DirectiveArgsError> {
        let mut args = DirectiveScanner {
            s: &self.args,
            pos: self.args_pos,
        };

        let mut list = vec![];
        loop {
            args.skip_space();
            if args.s.is_empty() {
                break;
            }
            let arg_pos = args.pos;
            let arg;

            match args.s.as_bytes()[0] {
                b'`' | b'"' => {
                    let q =
                        strconv::quoted_prefix(args.s).map_err(|_| invalid_quoted(self, args.s))?;
                    // Any errors will have been returned by quoted_prefix.
                    let prefix = args.take(q.len());
                    arg = strconv::unquote(prefix).map_err(|_| invalid_quoted(self, args.s))?;

                    // Check that the quoted string is followed by a space (or
                    // nothing).
                    if let Some(r) = args.s.chars().next()
                        && !r.is_whitespace()
                    {
                        return Err(invalid_quoted(self, args.s));
                    }
                }
                _ => {
                    arg = args.take_non_space();
                }
            }

            list.push(DirectiveArg { arg, pos: arg_pos });
        }
        Ok(list)
    }
}

fn invalid_quoted(d: &Directive, rest: &str) -> DirectiveArgsError {
    DirectiveArgsError(format!(
        "invalid quoted string in //{}:{}: {}",
        d.tool, d.name, rest
    ))
}

/// An error returned by [`Directive::parse_args`]; mirrors Go's
/// `fmt.Errorf("invalid quoted string in //%s:%s: %s", ...)`.
#[derive(Clone, Debug, PartialEq)]
pub struct DirectiveArgsError(pub String);

impl fmt::Display for DirectiveArgsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DirectiveArgsError {}

/// A DirectiveArg is an argument to a directive comment.
#[derive(Clone, Debug, PartialEq)]
pub struct DirectiveArg {
    /// The parsed argument string. If the argument was a quoted string, this
    /// is its unquoted form.
    pub arg: String,
    /// The position of the first character in this argument.
    pub pos: Pos,
}

/// ParseDirective parses a single comment line for a directive comment.
///
/// If the line is not a directive comment, it returns `None`.
///
/// The provided text must be a single line and should include the leading
/// "//". If the text does not start with "//", it returns `None`.
///
/// The caller may provide a file position of the start of `c`. This will be
/// used to track the position of the arguments. If the caller passes
/// `NoPos`, then the positions are effectively byte offsets into the string
/// `c`.
pub fn parse_directive(pos: Pos, c: &str) -> Option<Directive> {
    let bytes = c.as_bytes();
    // Fast path to eliminate most non-directive comments. Must be a line
    // comment starting with [a-z0-9].
    if !(c.len() >= 3 && bytes[0] == b'/' && bytes[1] == b'/' && is_alnum(bytes[2])) {
        return None;
    }

    let mut buf = DirectiveScanner { s: c, pos };
    buf.skip(2);

    // Check for a valid directive and parse tool part.
    //
    // This logic matches isDirective (in the test module). (We could combine
    // them, but isDirective itself is duplicated in several places in Go.)
    let colon = buf.s.find(':').unwrap_or(usize::MAX);
    if colon == usize::MAX || colon == 0 || colon + 1 >= buf.s.len() {
        return None;
    }
    for i in 0..=colon + 1 {
        if i == colon {
            continue;
        }
        if !is_alnum(buf.s.as_bytes()[i]) {
            return None;
        }
    }
    let tool = buf.take(colon);
    buf.skip(1);

    // Parse name and args.
    let name = buf.take_non_space();
    buf.skip_space();
    let args_pos = buf.pos;
    let args = buf.s.trim_end_matches(char::is_whitespace);

    Some(Directive {
        tool: tool.to_string(),
        name,
        args: args.to_string(),
        slash: pos,
        args_pos,
    })
}

fn is_alnum(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit()
}

/// A helper for parsing directive comments while maintaining position
/// information.
///
/// Slicing is always done at ASCII-byte boundaries (directives only slice at
/// `//`, `:`, whitespace, or quote positions), which are character
/// boundaries in UTF-8, so byte indices are safe here.
struct DirectiveScanner<'a> {
    s: &'a str,
    pos: Pos,
}

impl<'a> DirectiveScanner<'a> {
    fn skip(&mut self, n: usize) {
        self.pos = self.pos + n as i64;
        self.s = &self.s[n..];
    }

    fn take(&mut self, n: usize) -> &'a str {
        let res = &self.s[..n];
        self.skip(n);
        res
    }

    fn take_non_space(&mut self) -> String {
        let i = self.s.find(char::is_whitespace).unwrap_or(self.s.len());
        self.take(i).to_string()
    }

    fn skip_space(&mut self) {
        let trimmed = self.s.trim_start_matches(char::is_whitespace);
        self.skip(self.s.len() - trimmed.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::Pos;

    fn p(n: i64) -> Pos {
        Pos::from_int(n)
    }

    // isDirective reports whether c is a comment directive.
    // This code is also in go/printer.
    // (In Go it lives in ast.go; the comment machinery that used it is not
    // ported, so it stays here as the reference for the tests below.)
    fn is_directive(c: &str) -> bool {
        // "//line " is a line directive.
        // "//extern " is for gccgo.
        // "//export " is for cgo.
        // (The // has been removed.)
        if c.starts_with("line ") || c.starts_with("extern ") || c.starts_with("export ") {
            return true;
        }

        // "//[a-z0-9]+:[a-z0-9]"
        // (The // has been removed.)
        let colon = c.find(':').unwrap_or(usize::MAX);
        if colon == usize::MAX || colon == 0 || colon + 1 >= c.len() {
            return false;
        }
        for i in 0..=colon + 1 {
            if i == colon {
                continue;
            }
            let b = c.as_bytes()[i];
            if !(b.is_ascii_lowercase() || b.is_ascii_digit()) {
                return false;
            }
        }
        true
    }

    // isDirectiveTests, from Go's ast_test.go.
    const IS_DIRECTIVE_TESTS: &[(&str, bool)] = &[
        ("abc", false),
        ("go:inline", true),
        ("Go:inline", false),
        ("go:Inline", false),
        (":inline", false),
        ("lint:ignore", true),
        ("lint:1234", true),
        ("1234:lint", true),
        ("go: inline", false),
        ("go:", false),
        ("go:*", false),
        ("go:x*", true),
        ("export foo", true),
        ("extern foo", true),
        ("expert foo", false),
    ];

    #[test]
    fn test_is_directive() {
        for (inp, want) in IS_DIRECTIVE_TESTS {
            assert_eq!(is_directive(inp), *want, "isDirective({inp:?})");
        }
    }

    #[test]
    fn test_parse_directive_matches_is_directive() {
        for (inp, ok) in IS_DIRECTIVE_TESTS {
            let mut want = *ok;
            if inp.starts_with("extern ") || inp.starts_with("export ") {
                // parse_directive does NOT support extern or export, unlike
                // is_directive.
                want = false;
            }
            let got = parse_directive(p(0), &format!("//{inp}"));
            assert_eq!(got.is_some(), want, "ParseDirective(0, \"//{inp}\")");
        }
    }

    fn d(tool: &str, name: &str, args: &str, slash: i64, args_pos: i64) -> Directive {
        Directive {
            tool: tool.into(),
            name: name.into(),
            args: args.into(),
            slash: p(slash),
            args_pos: p(args_pos),
        }
    }

    #[test]
    fn test_parse_directive() {
        let cases: Vec<(&str, &str, i64, Option<Directive>)> = vec![
            (
                "valid",
                "//go:generate stringer -type Op -trimprefix Op",
                10,
                Some(d(
                    "go",
                    "generate",
                    "stringer -type Op -trimprefix Op",
                    10,
                    10 + "//go:generate ".len() as i64,
                )),
            ),
            (
                "no args",
                "//go:build ignore",
                20,
                Some(d(
                    "go",
                    "build",
                    "ignore",
                    20,
                    20 + "//go:build ".len() as i64,
                )),
            ),
            ("not a directive", "// not a directive", 30, None),
            ("not a comment", "go:generate", 40, None),
            ("empty", "", 50, None),
            ("just slashes", "//", 60, None),
            ("no name", "//go:", 70, None),
            ("no tool", "//:generate", 80, None),
            (
                "multiple spaces",
                "//go:build  foo bar",
                90,
                Some(d(
                    "go",
                    "build",
                    "foo bar",
                    90,
                    90 + "//go:build  ".len() as i64,
                )),
            ),
            (
                "trailing space",
                "//go:build foo ",
                100,
                Some(d(
                    "go",
                    "build",
                    "foo",
                    100,
                    100 + "//go:build ".len() as i64,
                )),
            ),
        ];

        for (name, inp, pos, want) in cases {
            let got = parse_directive(p(pos), inp);
            assert_eq!(got, want, "case {name}: ParseDirective({inp:?})");
        }
    }

    #[test]
    fn test_parse_args() {
        let cases: Vec<(&str, Directive, Option<Vec<DirectiveArg>>)> = vec![
            (
                "simple",
                d("go", "generate", "stringer -type Op", 0, 10),
                Some(vec![
                    DirectiveArg {
                        arg: "stringer".into(),
                        pos: p(10),
                    },
                    DirectiveArg {
                        arg: "-type".into(),
                        pos: p(10 + "stringer ".len() as i64),
                    },
                    DirectiveArg {
                        arg: "Op".into(),
                        pos: p(10 + "stringer -type ".len() as i64),
                    },
                ]),
            ),
            (
                "quoted",
                d("go", "generate", "\"foo bar\" baz", 0, 10),
                Some(vec![
                    DirectiveArg {
                        arg: "foo bar".into(),
                        pos: p(10),
                    },
                    DirectiveArg {
                        arg: "baz".into(),
                        pos: p(10 + "\"foo bar\" ".len() as i64),
                    },
                ]),
            ),
            (
                "raw quoted",
                d("go", "generate", "`foo bar` baz", 0, 10),
                Some(vec![
                    DirectiveArg {
                        arg: "foo bar".into(),
                        pos: p(10),
                    },
                    DirectiveArg {
                        arg: "baz".into(),
                        pos: p(10 + "`foo bar` ".len() as i64),
                    },
                ]),
            ),
            (
                "escapes",
                d("go", "generate", "\"foo\\U0001F60Abar\" `a\\tb`", 0, 10),
                Some(vec![
                    DirectiveArg {
                        arg: "foo\u{1F60A}bar".into(),
                        pos: p(10),
                    },
                    DirectiveArg {
                        arg: "a\\tb".into(),
                        pos: p(10 + "\"foo\\U0001F60Abar\" ".len() as i64),
                    },
                ]),
            ),
            ("empty args", d("go", "build", "", 0, 10), Some(vec![])),
            (
                "spaces",
                d("go", "build", "  foo   bar  ", 0, 10),
                Some(vec![
                    DirectiveArg {
                        arg: "foo".into(),
                        pos: p(10 + 2),
                    },
                    DirectiveArg {
                        arg: "bar".into(),
                        pos: p(10 + "  foo   ".len() as i64),
                    },
                ]),
            ),
            (
                "unterminated quote",
                d("go", "generate", "`foo", 0, 0),
                None,
            ),
            (
                "no space after quote",
                d("go", "generate", "\"foo\"bar", 0, 0),
                None,
            ),
        ];

        for (name, inp, want) in cases {
            let got = inp.parse_args();
            match (got, want) {
                (Err(_), None) => {}
                (Ok(args), Some(want)) => assert_eq!(args, want, "case {name}"),
                (got, want) => panic!("case {name}: got {got:?}, want {want:?}"),
            }
        }
    }
}
