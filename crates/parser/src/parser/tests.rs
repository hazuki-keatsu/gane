//! Ported Go parser tests.
//!
//! This module holds the test suites ported from go/parser's test files:
//! error_test.go (wholesale), short_test.go (wholesale), the
//! resolution-related subsets of parser_test.go and resolver_test.go
//! (wholesale minus the concurrency test; see the resolver.rs module
//! documentation), and the example_test.go example. The omitted suites
//! depend on comment collection, tracing, or ast printing, which this port
//! drops (see the parent module documentation); their helpers are omitted
//! along with them. Go's file reading (`ParseFile` with a nil src) is done
//! here with `std::fs`, and the testdata fixtures live at
//! `src/parser/testdata` (cargo runs tests with the crate root as cwd).

use std::collections::BTreeMap;
use std::fs;
use std::rc::Rc;
use std::sync::Once;

use regex::Regex;

use super::*;
use crate::ast::{NodeRef, inspect};
use crate::parser::interface::*;
use crate::scanner::{ErrorList, SCAN_COMMENTS, Scanner};
use crate::token::{File, FileSet, NoPos, Pos};

/// Silences the default panic hook for parser bailouts only: a `Bailout`
/// panic is an internal control-flow device (like Go's recover in ParseFile)
/// and would otherwise print a panic message for every test that exercises
/// error recovery. All other panics keep the default hook.
fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let old = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if info.payload().downcast_ref::<Bailout>().is_none() {
                old(info);
            }
        }));
    });
}

/// Reports the collected failures and panics if there are any.
fn expect_failures(failures: Vec<String>) {
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The path prefix of the copied go/parser testdata (Go: "testdata").
const TESTDATA: &str = "src/parser/testdata";

fn read_test_file(filename: &str) -> Vec<u8> {
    fs::read(filename).unwrap_or_else(|e| panic!("reading {filename}: {e}"))
}

// ----------------------------------------------------------------------------
// error_test.go harness (ported wholesale; Go's `traceErrs` flag is dropped
// with the Trace mode)

// getFile assumes that each filename occurs at most once
fn get_file(fset: &FileSet, filename: &str) -> Option<Rc<File>> {
    let mut base: Option<i64> = None;
    fset.iterate(|f| {
        if f.name() == filename {
            if base.is_some() {
                panic!("{filename} used multiple times");
            }
            base = Some(f.base());
        }
        true
    });
    base.and_then(|b| fset.file(Pos::from_int(b)))
}

fn get_pos(fset: &FileSet, filename: &str, offset: i64) -> Pos {
    if let Some(f) = get_file(fset, filename) {
        return f.pos(offset);
    }
    NoPos
}

// ERROR comments must be of the form /* ERROR "rx" */ and rx is
// a regular expression that matches the expected error message.
// The special form /* ERROR HERE "rx" */ must be used for error
// messages that appear immediately after a token, rather than at
// a token's position, and ERROR AFTER means after the comment
// (e.g. at end of line).
fn err_rx() -> Regex {
    Regex::new(r#"^/\* *ERROR *(HERE|AFTER)? *"([^"]*)" *\*/$"#).unwrap()
}

/// expected_errors collects the regular expressions of ERROR comments found
/// in `src` and returns them as a map of error positions to error messages.
fn expected_errors(fset: &FileSet, filename: &str, src: &[u8]) -> BTreeMap<Pos, String> {
    let mut errors: BTreeMap<Pos, String> = BTreeMap::new();

    let file = get_file(fset, filename).expect("file not found");
    let mut s = Scanner::new(file, src, None, SCAN_COMMENTS);
    let mut prev = NoPos; // position of last non-comment, non-semicolon token
    let mut here = NoPos; // position immediately after the token at position prev

    let rx = err_rx();
    loop {
        let (pos, tok, lit) = s.scan();
        let end = s.end();

        match tok {
            Token::EOF => return errors,
            Token::Comment => {
                if let Some(caps) = rx.captures(&lit) {
                    if caps.len() == 3 {
                        let mut pos = pos;
                        if caps.get(1).is_some_and(|m| m.as_str() == "HERE") {
                            pos = here; // position right after the previous token prior to comment
                        } else if caps.get(1).is_some_and(|m| m.as_str() == "AFTER") {
                            pos = pos + lit.len() as i64; // end of comment
                        } else {
                            pos = prev; // token prior to comment
                        }
                        errors.insert(pos, caps[2].to_string());
                    }
                }
            }
            Token::Semicolon => {
                // don't use the position of auto-inserted (invisible) semicolons
                if lit != ";" {
                    continue;
                }
                prev = pos;
                here = end;
            }
            _ => {
                prev = pos;
                here = end;
            }
        }
    }
}

/// compare_errors compares the map of expected error messages with the list
/// of found errors and reports discrepancies (Go's `compareErrors`, adapted
/// to collect failure messages instead of `t.Errorf`).
fn compare_errors(
    fset: &FileSet,
    expected: &mut BTreeMap<Pos, String>,
    found: &ErrorList,
    failures: &mut Vec<String>,
) {
    for error in &found.0 {
        // error.pos is a token.Position, but we want
        // a token.Pos so we can do a map lookup
        let pos = get_pos(fset, &error.pos.file_name, error.pos.offset);
        if let Some(msg) = expected.get(&pos) {
            // we expect a message at pos; check if it matches.
            // (Go's regexp treats an unpaired "{" as a literal; the regex
            // crate requires it to be escaped. No expected message uses a
            // counted repetition, so escaping both braces is exact.)
            let msg = msg.replace('{', "\\{").replace('}', "\\}");
            match Regex::new(&msg) {
                Ok(rx) => {
                    if !rx.is_match(&error.msg) {
                        failures.push(format!(
                            "{}: {:?} does not match {:?}",
                            error.pos, error.msg, msg
                        ));
                        continue;
                    }
                    // we have a match - eliminate this error
                    expected.remove(&pos);
                }
                Err(e) => {
                    failures.push(format!("{}: {e}", error.pos));
                    continue;
                }
            }
        } else {
            // To keep in mind when analyzing failed test output:
            // If the same error position occurs multiple times in errors,
            // this message will be triggered (because the first error at
            // the position removes this position from the expected errors).
            failures.push(format!("{}: unexpected error: {}", error.pos, error.msg));
        }
    }

    // there should be no expected errors left
    if !expected.is_empty() {
        failures.push(format!("{} errors not reported:", expected.len()));
        for (pos, msg) in expected {
            failures.push(format!("{}: {}", fset.position(*pos), msg));
        }
    }
}

fn check_errors(filename: &str, input: &[u8], mode: Mode, expect_errors: bool) -> Vec<String> {
    let mut fset = FileSet::new();
    let (_f, err) = parse_file(&mut fset, filename, input, mode);
    let mut found = match err {
        Some(err) => err,
        None => ErrorList::default(),
    };
    found.remove_multiples();

    let mut expected: BTreeMap<Pos, String> = BTreeMap::new();
    if expect_errors {
        // we are expecting the following errors
        // (collect these after parsing a file so that it is found in the file set)
        expected = expected_errors(&fset, filename, input);
    }

    // verify errors returned by the parser
    let mut failures = Vec::new();
    compare_errors(&fset, &mut expected, &found, &mut failures);
    failures
}

#[test]
fn test_errors() {
    init();
    let entries = fs::read_dir(TESTDATA).expect("read testdata dir");
    let mut names: Vec<String> = entries
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    for name in names {
        if name.starts_with('.') || !(name.ends_with(".src") || name.ends_with(".go2")) {
            continue;
        }
        // (Go: `if !d.IsDir()`; the testdata dir has no subdirectories among
        // the .src/.go2 files.)
        let filename = format!("{TESTDATA}/{name}");
        let src = read_test_file(&filename);
        let mode = DECLARATION_ERRORS | ALL_ERRORS;
        let failures = check_errors(&filename, &src, mode, true);
        assert!(failures.is_empty(), "{name}:\n{}", failures.join("\n"));
    }
}

// ----------------------------------------------------------------------------
// short_test.go (ported wholesale)

const VALIDS: &[&str] = &[
    "package p\n",
    "package p;",
    r#"package p; import "fmt"; func f() { fmt.Println("Hello, World!") };"#,
    "package p; func f() { if f(T{}) {} };",
    "package p; func f() { _ = <-chan int(nil) };",
    "package p; func f() { _ = (<-chan int)(nil) };",
    "package p; func f() { _ = (<-chan <-chan int)(nil) };",
    "package p; func f() { _ = <-chan <-chan <-chan <-chan <-int(nil) };",
    "package p; func f(func() func() func());",
    "package p; func f(...T);",
    "package p; func f(float, ...int);",
    "package p; func f(x int, a ...int) { f(0, a...); f(1, a...,) };",
    "package p; func f(int,) {};",
    "package p; func f(...int,) {};",
    "package p; func f(x ...int,) {};",
    "package p; type T []int; var a []bool; func f() { if a[T{42}[0]] {} };",
    "package p; type T []int; func g(int) bool { return true }; func f() { if g(T{42}[0]) {} };",
    "package p; type T []int; func f() { for _ = range []int{T{42}[0]} {} };",
    "package p; var a = T{{1, 2}, {3, 4}}",
    "package p; func f() { select { case <- c: case c <- d: case c <- <- d: case <-c <- d: } };",
    "package p; func f() { select { case x := (<-c): } };",
    "package p; func f() { if ; true {} };",
    "package p; func f() { switch ; {} };",
    r#"package p; func f() { for _ = range "foo" + "bar" {} };"#,
    "package p; func f() { var s []int; g(s[:], s[i:], s[:j], s[i:j], s[i:j:k], s[:j:k]) };",
    "package p; var ( _ = (struct {*T}).m; _ = (interface {T}).m )",
    "package p; func ((T),) m() {}",
    "package p; func ((*T),) m() {}",
    "package p; func (*(T),) m() {}",
    "package p; func _(x []int) { for range x {} }",
    "package p; func _() { if [T{}.n]int{} {} }",
    "package p; func _() { map[int]int{}[0]++; map[int]int{}[0] += 1 }",
    "package p; func _(x interface{f()}) { interface{f()}(x).f() }",
    "package p; func _(x chan int) { chan int(x) <- 0 }",
    // go.dev/issue/9639
    "package p; const (x = 0; y; z)",
    "package p; var _ = map[P]int{P{}:0, {}:1}",
    "package p; var _ = map[*P]int{&P{}:0, {}:1}",
    "package p; type T = int",
    "package p; type (T = p.T; _ = struct{}; x = *T)",
    "package p; type T (*int)",
    "package p; type _ struct{ int }",
    "package p; type _ struct{ pkg.T }",
    "package p; type _ struct{ *pkg.T }",
    "package p; var _ = func()T(nil)",
    "package p; func _(T (P))",
    "package p; func _(T []E)",
    "package p; func _(T [P]E)",
    "package p; type _ [A+B]struct{}",
    "package p; func (R) _()",
    "package p; type _ struct{ f [n]E }",
    "package p; type _ struct{ f [a+b+c+d]E }",
    "package p; type I1 interface{}; type I2 interface{ I1 }",
    // generic code
    "package p; type _ []T[int]",
    "package p; type T[P any] struct { P }",
    "package p; type T[P comparable] struct { P }",
    "package p; type T[P comparable[P]] struct { P }",
    "package p; type T[P1, P2 any] struct { P1; f []P2 }",
    "package p; func _[T any]()()",
    "package p; func _(T (P))",
    "package p; func f[A, B any](); func _() { _ = f[int, int] }",
    "package p; func _(x T[P1, P2, P3])",
    "package p; func _(x p.T[Q])",
    "package p; func _(p.T[Q])",
    "package p; type _[A interface{},] struct{}",
    "package p; type _[A interface{}] struct{}",
    "package p; type _[A,  B any,] struct{}",
    "package p; type _[A, B any] struct{}",
    "package p; type _[A any,] struct{}",
    "package p; type _[A any]struct{}",
    "package p; type _[A any] struct{ A }",
    "package p; func _[T any]()",
    "package p; func _[T any](x T)",
    "package p; func _[T1, T2 any](x T)",
    "package p; func _[A, B any](a A) B",
    "package p; func _[A, B C](a A) B",
    "package p; func _[A, B C[A, B]](a A) B",
    "package p; type _[A, B any] interface { _(a A) B }",
    "package p; type _[A, B C[A, B]] interface { _(a A) B }",
    "package p; func _[T1, T2 interface{}](x T1) T2",
    "package p; func _[T1 interface{ m() }, T2, T3 interface{}](x T1, y T3) T2",
    "package p; var _ = []T[int]{}",
    "package p; var _ = [10]T[int]{}",
    "package p; var _ = func()T[int]{}",
    "package p; var _ = map[T[int]]T[int]{}",
    "package p; var _ = chan T[int](x)",
    "package p; func _(_ T[P], T P) T[P]",
    "package p; var _ T[chan int]",
    "package p; func (_ R[P]) _(x T)",
    "package p; func (_ R[ P, Q]) _(x T)",
    "package p; func (R[P]) _()",
    "package p; func _(T[P])",
    "package p; func _(T[P1, P2, P3 ])",
    "package p; func _(T[P]) T[P]",
    "package p; type _ struct{ T[P]}",
    "package p; type _ struct{ T[struct{a, b, c int}] }",
    "package p; type _ interface{int|float32; bool; m(); string;}",
    "package p; type I1[T any] interface{}; type I2 interface{ I1[int] }",
    "package p; type I1[T any] interface{}; type I2[T any] interface{ I1[T] }",
    "package p; type _ interface { N[T] }",
    "package p; type T[P any] = T0",
];

#[test]
fn test_valid() {
    init();
    for src in VALIDS {
        let failures = check_errors(src, src.as_bytes(), DECLARATION_ERRORS | ALL_ERRORS, false);
        assert!(failures.is_empty(), "src {src:?}:\n{}", failures.join("\n"));
    }
}

// TestSingle is useful to track down a problem with a single short test program.
#[test]
fn test_single() {
    const SRC: &str = "package p; var _ = T{}";
    let failures = check_errors(SRC, SRC.as_bytes(), DECLARATION_ERRORS | ALL_ERRORS, true);
    expect_failures(failures);
}

const INVALIDS: &[&str] = &[
    r#"foo /* ERROR "expected 'package'" */ !"#,
    r#"package p; func f() { if { /* ERROR "missing condition" */ } };"#,
    r#"package p; func f() { if ; /* ERROR "missing condition" */ {} };"#,
    r#"package p; func f() { if f(); /* ERROR "missing condition" */ {} };"#,
    r#"package p; func f() { if _ = range /* ERROR "expected operand" */ x; true {} };"#,
    r#"package p; func f() { switch _ /* ERROR "expected switch expression" */ = range x; true {} };"#,
    r#"package p; func f() { for _ = range x ; /* ERROR "expected '{'" */ ; {} };"#,
    r#"package p; func f() { for ; ; _ = range /* ERROR "expected operand" */ x {} };"#,
    r#"package p; func f() { for ; _ /* ERROR "expected boolean or range expression" */ = range x ; {} };"#,
    r#"package p; func f() { switch t = /* ERROR "expected ':=', found '='" */ t.(type) {} };"#,
    r#"package p; func f() { switch t /* ERROR "expected switch expression" */ , t = t.(type) {} };"#,
    r#"package p; func f() { switch t /* ERROR "expected switch expression" */ = t.(type), t {} };"#,
    r#"package p; func f() { _ = (<-<- /* ERROR "expected 'chan'" */ chan int)(nil) };"#,
    r#"package p; func f() { _ = (<-chan<-chan<-chan<-chan<-chan<- /* ERROR "expected channel type" */ int)(nil) };"#,
    r#"package p; func f() { if x := g(); x /* ERROR "expected boolean expression" */ = 0 {}};"#,
    r#"package p; func f() { _ = x = /* ERROR "expected '=='" */ 0 {}};"#,
    r#"package p; func f() { _ = 1 == func()int { var x bool; x = x = /* ERROR "expected '=='" */ true; return x }() };"#,
    r#"package p; func f() { var s []int; _ = s[] /* ERROR "expected operand" */ };"#,
    r#"package p; func f() { var s []int; _ = s[i:j: /* ERROR "final index required" */ ] };"#,
    r#"package p; func f() { var s []int; _ = s[i: /* ERROR "middle index required" */ :k] };"#,
    r#"package p; func f() { var s []int; _ = s[i: /* ERROR "middle index required" */ :] };"#,
    r#"package p; func f() { var s []int; _ = s[: /* ERROR "middle index required" */ :] };"#,
    r#"package p; func f() { var s []int; _ = s[: /* ERROR "middle index required" */ ::] };"#,
    r#"package p; func f() { var s []int; _ = s[i:j:k: /* ERROR "expected ']'" */ l] };"#,
    r#"package p; func f() { for x /* ERROR "boolean or range expression" */ = []string {} }"#,
    r#"package p; func f() { for x /* ERROR "boolean or range expression" */ := []string {} }"#,
    r#"package p; func f() { for i /* ERROR "boolean or range expression" */ , x = []string {} }"#,
    r#"package p; func f() { for i /* ERROR "boolean or range expression" */ , x := []string {} }"#,
    r#"package p; func f() { go f /* ERROR HERE "must be function call" */ }"#,
    r#"package p; func f() { go ( /* ERROR "must not be parenthesized" */ f()) }"#,
    r#"package p; func f() { defer func() {} /* ERROR HERE "must be function call" */ }"#,
    r#"package p; func f() { defer ( /* ERROR "must not be parenthesized" */ f()) }"#,
    r#"package p; func f() { go func() { func() { f(x func /* ERROR "missing ','" */ (){}) } } }"#,
    r#"package p; func _() (type /* ERROR "found 'type'" */ T)(T)"#,
    r#"package p; func (type /* ERROR "found 'type'" */ T)(T) _()"#,
    r#"package p; type _[A+B, /* ERROR "unexpected comma" */ ] int"#,
    r#"package p; type _ struct{ [ /* ERROR "expected '}', found '\['" */ ]byte }"#,
    r#"package p; type _ struct{ ( /* ERROR "cannot parenthesize embedded type" */ int) }"#,
    r#"package p; type _ struct{ ( /* ERROR "cannot parenthesize embedded type" */ []byte) }"#,
    r#"package p; type _ struct{ *( /* ERROR "cannot parenthesize embedded type" */ int) }"#,
    r#"package p; type _ struct{ *( /* ERROR "cannot parenthesize embedded type" */ []byte) }"#,
    // go.dev/issue/8656
    r#"package p; func f() (a b string /* ERROR "missing ','" */ , ok bool)"#,
    // go.dev/issue/9639
    r#"package p; var x, y, z; /* ERROR "expected type" */"#,
    // go.dev/issue/12437
    r#"package p; var _ = struct { x int, /* ERROR "expected ';', found ','" */ }{};"#,
    r#"package p; var _ = struct { x int, /* ERROR "expected ';', found ','" */ y float }{};"#,
    // go.dev/issue/11611
    r#"package p; type _ struct { int, } /* ERROR "expected 'IDENT', found '}'" */ ;"#,
    r#"package p; type _ struct { int, float } /* ERROR "expected type, found '}'" */ ;"#,
    // go.dev/issue/13475
    r#"package p; func f() { if true {} else ; /* ERROR "expected if statement or block" */ }"#,
    r#"package p; func f() { if true {} else defer /* ERROR "expected if statement or block" */ f() }"#,
    // variadic parameter lists
    r#"package p; func f(a, b ... /* ERROR "can only use ... with final parameter" */ int)"#,
    r#"package p; func f(a ... /* ERROR "can only use ... with final parameter" */ int, b int)"#,
    r#"package p; func f(... /* ERROR "can only use ... with final parameter" */ int, int)"#,
    r#"package p; func f() (... /* ERROR "invalid use of ..." */ int)"#,
    r#"package p; func f() (a, b ... /* ERROR "invalid use of ..." */ int)"#,
    r#"package p; func f[T ... /* ERROR "invalid use of ..." */ C]()() {}"#,
    // generic code
    r#"package p; type _[_ any] int; var _ = T[] /* ERROR "expected operand" */ {}"#,
    r#"package p; var _ func[ /* ERROR "must have no type parameters" */ T any](T)"#,
    r#"package p; func _[]/* ERROR "empty type parameter list" */()"#,
    r#"package p; type _[A,] /* ERROR "missing type constraint" */ struct{ A }"#,
    r#"package p; func _[type /* ERROR "found 'type'" */ P, *Q interface{}]()"#,
    r#"package p; func (T) _[A, B any](a A) B"#,
    r#"package p; func (T) _[A, B C](a A) B"#,
    r#"package p; func (T) _[A, B C[A, B]](a A) B"#,
    // go.dev/issue/50956
    r#"package p; func(*T[e, e /* ERROR "e redeclared" */ ]) _()"#,
    // go.dev/issue/70957
    r#"package p; func f() {goto; /* ERROR "expected 'IDENT', found ';'" */ }"#,
    r#"package p; func f() {goto} /* ERROR "expected 'IDENT', found '}'" */ }"#,
];

#[test]
fn test_invalid() {
    init();
    for src in INVALIDS {
        let failures = check_errors(src, src.as_bytes(), DECLARATION_ERRORS | ALL_ERRORS, true);
        assert!(failures.is_empty(), "src {src:?}:\n{}", failures.join("\n"));
    }
}

// ----------------------------------------------------------------------------
// parser_test.go (ported subset; comment- and resolution-dependent tests are
// omitted - see the module documentation)

const VALID_FILES: &[&str] = &[
    "parser.go",
    "parser_test.go",
    "error_test.go",
    "short_test.go",
];

#[test]
fn test_parse() {
    for filename in VALID_FILES {
        let path = format!("{TESTDATA}/go/{filename}");
        let src = read_test_file(&path);
        let mut fset = FileSet::new();
        let (_f, err) = parse_file(&mut fset, filename, &src, DECLARATION_ERRORS);
        assert!(err.is_none(), "ParseFile({filename}): {err:?}");
    }
}

fn name_filter(filename: &str) -> bool {
    match filename {
        "parser.go" | "interface.go" | "parser_test.go" => true,
        "parser.go.orig" => true, // permit but should be ignored by ParseDir
        _ => false,
    }
}

#[test]
fn test_parse_file() {
    init();
    let src = "package p\nvar _=s[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]";
    let mut fset = FileSet::new();
    let (_f, err) = parse_file(&mut fset, "", src.as_bytes(), Mode::default());
    assert!(err.is_some(), "ParseFile({src}) succeeded unexpectedly");
}

#[test]
fn test_parse_expr_from() {
    init();
    let src = "s[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]+\ns[::]";
    let mut fset = FileSet::new();
    let (_x, err) = parse_expr_from(&mut fset, "", src.as_bytes(), Mode::default());
    assert!(err.is_some(), "ParseExprFrom({src}) succeeded unexpectedly");
}

#[test]
fn test_parse_dir() {
    let path = format!("{TESTDATA}/go");
    let mut fset = FileSet::new();
    let (pkgs, err) = parse_dir(&mut fset, &path, Some(&name_filter), Mode::default());
    assert!(err.is_none(), "ParseDir({path}): {err:?}");
    assert_eq!(pkgs.len(), 1, "got {} packages; want 1", pkgs.len());
    let pkg = pkgs.get("parser").expect("package \"parser\" not found");
    assert_eq!(
        pkg.files.len(),
        3,
        "got {} package files; want 3",
        pkg.files.len()
    );
    for filename in pkg.files.keys() {
        // (Go runs ParseDir with path ".", so its file keys are plain names
        // like "parser.go"; here the fixture path prefix is stripped.)
        let name = std::path::Path::new(filename)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| filename.clone());
        assert!(name_filter(&name), "unexpected package file: {filename}");
    }
}

#[test]
fn test_issue42951() {
    let path = format!("{TESTDATA}/issue42951");
    let mut fset = FileSet::new();
    let (_pkgs, err) = parse_dir(&mut fset, &path, None, Mode::default());
    assert!(err.is_none(), "ParseDir({path}): {err:?}");
}

#[test]
fn test_parse_expr() {
    // just kicking the tires:
    // a valid arithmetic expression
    let mut src = "a + b";
    let (x, err) = parse_expr(src);
    assert!(err.is_none(), "ParseExpr({src:?}): {err:?}");
    // sanity check
    assert!(
        matches!(x, Some(Expr::BinaryExpr(_))),
        "ParseExpr({src:?}): got {x:?}, want *ast.BinaryExpr"
    );

    // a valid type expression
    src = "struct{x *int}";
    let (x, err) = parse_expr(src);
    assert!(err.is_none(), "ParseExpr({src:?}): {err:?}");
    // sanity check
    assert!(
        matches!(x, Some(Expr::StructType(_))),
        "ParseExpr({src:?}): got {x:?}, want *ast.StructType"
    );

    // an invalid expression
    src = "a + *";
    let (x, err) = parse_expr(src);
    assert!(err.is_some(), "ParseExpr({src:?}): got no error");
    assert!(x.is_some(), "ParseExpr({src:?}): got no (partial) result");
    assert!(
        matches!(x, Some(Expr::BinaryExpr(_))),
        "ParseExpr({src:?}): got {x:?}, want *ast.BinaryExpr"
    );

    // a valid expression followed by extra tokens is invalid
    src = "a[i] := x";
    let (_x, err) = parse_expr(src);
    assert!(err.is_some(), "ParseExpr({src:?}): got no error");

    // a semicolon is not permitted unless automatically inserted
    src = "a + b\n";
    let (_x, err) = parse_expr(src);
    assert!(err.is_none(), "ParseExpr({src:?}): got error {err:?}");
    src = "a + b;";
    let (_x, err) = parse_expr(src);
    assert!(err.is_some(), "ParseExpr({src:?}): got no error");

    // various other stuff following a valid expression
    const VALID_EXPR: &str = "a + b";
    const ANYTHING: &str = "dh3*#D)#_";
    for c in "!)]};,".chars() {
        let s = format!("{VALID_EXPR}{c}{ANYTHING}");
        let (_x, err) = parse_expr(&s);
        assert!(err.is_some(), "ParseExpr({s:?}): got no error");
    }

    // ParseExpr must not crash
    for src in VALIDS {
        let _ = parse_expr(src);
    }
}

// TestIssue9979 verifies that empty statements are contained within their
// enclosing blocks.
#[test]
fn test_issue9979() {
    let sources = [
        "package p; func f() {;}",
        "package p; func f() {L:}",
        "package p; func f() {L:;}",
        "package p; func f() {L:\n}",
        "package p; func f() {L:\n;}",
        "package p; func f() { ; }",
        "package p; func f() { L: }",
        "package p; func f() { L: ; }",
        "package p; func f() { L: \n}",
        "package p; func f() { L: \n; }",
    ];
    for src in sources {
        let mut fset = FileSet::new();
        let (f, err) = parse_file(&mut fset, "", src.as_bytes(), Mode::default());
        assert!(err.is_none(), "{src}: {err:?}");

        let mut pos = NoPos;
        let mut end = NoPos;
        let mut failures = Vec::new();
        inspect(NodeRef::File(&f), &mut |n| {
            match n {
                Some(NodeRef::BlockStmt(s)) => {
                    pos = s.pos() + 1; // exclude "{"
                    end = s.end() - 1; // exclude "}"
                }
                Some(NodeRef::LabeledStmt(s)) => {
                    pos = s.pos() + 2; // exclude "L:"
                    end = s.end();
                }
                Some(NodeRef::EmptyStmt(s)) => {
                    // check containment
                    if s.pos() < pos || s.end() > end {
                        failures.push(format!(
                            "{src}: EmptyStmt[{}, {}] not inside [{pos}, {end}]",
                            s.pos(),
                            s.end()
                        ));
                    }
                    // check semicolon
                    let offs = fset.position(s.pos()).offset as usize;
                    let ch = src.as_bytes()[offs] as char;
                    // (Go: `ch != ';' != s.Implicit` - the semicolon must be
                    // explicit exactly when Implicit is false)
                    if (ch == ';') == s.implicit {
                        let want = if s.implicit {
                            "but ';' is implicit"
                        } else {
                            "want ';'"
                        };
                        failures.push(format!("{src}: found {ch:?} at offset {offs}; {want}"));
                    }
                }
                _ => {}
            }
            true
        });
        expect_failures(failures);
    }
}

#[test]
fn test_file_start_end_pos() {
    let src = "// Copyright\n\n//+build tag\n\n// Package p doc comment.\npackage p\n\nvar lastDecl int\n\n/* end of file */\n";
    let mut fset = FileSet::new();
    let (f, err) = parse_file(&mut fset, "file.go", src.as_bytes(), Mode::default());
    assert!(err.is_none(), "{err:?}");

    // File{Start,End} spans the entire file, not just the declarations.
    assert_eq!(
        fset.position(f.file_start).to_string(),
        "file.go:1:1",
        "for File.FileStart"
    );
    // The end position is the newline at the end of the /* end of file */ line.
    assert_eq!(
        fset.position(f.file_end).to_string(),
        "file.go:10:19",
        "for File.FileEnd"
    );
}

// TestIncompleteSelection ensures that an incomplete selector
// expression is parsed as a (blank) *ast.SelectorExpr, not a
// *ast.BadExpr.
#[test]
fn test_incomplete_selection() {
    let sources = [
        "package p; var _ = fmt.",             // at EOF
        "package p; var _ = fmt.\ntype X int", // not at EOF
    ];
    for src in sources {
        let mut fset = FileSet::new();
        let (f, err) = parse_file(&mut fset, "", src.as_bytes(), Mode::default());
        assert!(err.is_some(), "ParseFile({src}) succeeded unexpectedly");

        const WANT_ERR: &str = "expected selector or type assertion";
        let err = err.unwrap();
        assert!(
            err.0[0].msg.contains(WANT_ERR),
            "ParseFile returned wrong error {:?}, want {WANT_ERR:?}",
            err.0[0].msg
        );

        let mut sel_name: Option<String> = None;
        let mut sel_x_is_fmt = false;
        inspect(NodeRef::File(&f), &mut |n| {
            if let Some(NodeRef::SelectorExpr(s)) = n {
                sel_name = Some(s.sel.name.clone());
                sel_x_is_fmt = matches!(&s.x, Expr::Ident(id) if id.name == "fmt");
            }
            true
        });
        assert_eq!(sel_name.as_deref(), Some("_"), "found no blank selector");
        assert!(sel_x_is_fmt, "selector x is not fmt");
    }
}

// TestParseDepthLimit (only the parser depth is tested; the scope depth is
// a resolver concern). Runs on a dedicated large-stack thread: Go goroutine
// stacks grow, Rust thread stacks do not.
#[test]
fn test_parse_depth_limit() {
    init();
    struct DepthTest {
        name: &'static str,
        format: &'static str,
        parse_multiplier: i32,
    }
    let tests: &[DepthTest] = &[
        DepthTest {
            name: "array",
            format: "package main; var x «[1]»int",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "slice",
            format: "package main; var x «[]»int",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "struct",
            format: "package main; var x «struct { X «int» }»",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "pointer",
            format: "package main; var x «*»int",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "func",
            format: "package main; var x «func()»int",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "chan",
            format: "package main; var x «chan »int",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "chan2",
            format: "package main; var x «<-chan »int",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "interface",
            format: "package main; var x «interface { M() «int» }»",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "map",
            format: "package main; var x «map[int]»int",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "slicelit",
            format: "package main; var x = []any{«[]any{«»}»}",
            parse_multiplier: 3,
        }, // Parser nodes: UnaryExpr, CompositeLit
        DepthTest {
            name: "arraylit",
            format: "package main; var x = «[1]any{«nil»}»",
            parse_multiplier: 3,
        }, // Parser nodes: UnaryExpr, CompositeLit
        DepthTest {
            name: "structlit",
            format: "package main; var x = «struct{x any}{«nil»}»",
            parse_multiplier: 3,
        }, // Parser nodes: UnaryExpr, CompositeLit
        DepthTest {
            name: "maplit",
            format: "package main; var x = «map[int]any{1:«nil»}»",
            parse_multiplier: 3,
        }, // Parser nodes: CompositeLit, KeyValueExpr
        DepthTest {
            name: "dot",
            format: "package main; var x = «x.»x",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "index",
            format: "package main; var x = x«[1]»",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "slice",
            format: "package main; var x = x«[1:2]»",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "slice3",
            format: "package main; var x = x«[1:2:3]»",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "dottype",
            format: "package main; var x = x«.(any)»",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "callseq",
            format: "package main; var x = x«()»",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "methseq",
            format: "package main; var x = x«.m()»",
            parse_multiplier: 2,
        }, // Parser nodes: SelectorExpr, CallExpr
        DepthTest {
            name: "binary",
            format: "package main; var x = «1+»1",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "binaryparen",
            format: "package main; var x = «1+(«1»)»",
            parse_multiplier: 2,
        }, // Parser nodes: BinaryExpr, ParenExpr
        DepthTest {
            name: "unary",
            format: "package main; var x = «^»1",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "addr",
            format: "package main; var x = «& »x",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "star",
            format: "package main; var x = «*»x",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "recv",
            format: "package main; var x = «<-»x",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "call",
            format: "package main; var x = «f(«1»)»",
            parse_multiplier: 2,
        }, // Parser nodes: Ident, CallExpr
        DepthTest {
            name: "conv",
            format: "package main; var x = «(*T)(«1»)»",
            parse_multiplier: 2,
        }, // Parser nodes: ParenExpr, CallExpr
        DepthTest {
            name: "label",
            format: "package main; func main() { «Label:» }",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "if",
            format: "package main; func main() { «if true { «» }»}",
            parse_multiplier: 2,
        }, // Parser nodes: IfStmt, BlockStmt
        DepthTest {
            name: "ifelse",
            format: "package main; func main() { «if true {} else » {} }",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "switch",
            format: "package main; func main() { «switch { default: «» }»}",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "typeswitch",
            format: "package main; func main() { «switch x.(type) { default: «» }» }",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "for0",
            format: "package main; func main() { «for { «» }» }",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "for1",
            format: "package main; func main() { «for x { «» }» }",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "for3",
            format: "package main; func main() { «for f(); g(); h() { «» }» }",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "forrange0",
            format: "package main; func main() { «for range x { «» }» }",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "forrange1",
            format: "package main; func main() { «for x = range z { «» }» }",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "forrange2",
            format: "package main; func main() { «for x, y = range z { «» }» }",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "go",
            format: "package main; func main() { «go func() { «» }()» }",
            parse_multiplier: 2,
        }, // Parser nodes: GoStmt, FuncLit
        DepthTest {
            name: "defer",
            format: "package main; func main() { «defer func() { «» }()» }",
            parse_multiplier: 2,
        }, // Parser nodes: DeferStmt, FuncLit
        DepthTest {
            name: "select",
            format: "package main; func main() { «select { default: «» }» }",
            parse_multiplier: 0,
        },
        DepthTest {
            name: "block",
            format: "package main; func main() { «{«»}» }",
            parse_multiplier: 0,
        },
    ];

    for tt in tests {
        for size in ["small", "big"] {
            let mut n = super::MAX_NEST_LEV + 1;
            if tt.parse_multiplier > 0 {
                n /= tt.parse_multiplier;
            }
            if size == "small" {
                // Decrease the number of statements by 10, in order to check
                // that we do not fail when under the limit. 10 is used to
                // provide some wiggle room for cases where the surrounding
                // scaffolding syntax adds some noise to the depth that
                // changes on a per testcase basis.
                n -= 10;
            }

            let (pre, mid, post) = split(tt.format);
            let mid = if mid.contains('«') {
                let (left, base, right) = split(&mid);
                left.repeat(n as usize) + base + &right.repeat(n as usize)
            } else {
                mid.repeat(n as usize)
            };
            let input = format!("{pre}{mid}{post}");

            // The FileSet must be created inside the thread (it is !Send),
            // and the parse runs on a large stack (Go stacks grow).
            let handle = std::thread::Builder::new()
                .stack_size(256 * 1024 * 1024)
                .spawn(move || {
                    let mut fset = FileSet::new();
                    // (Go runs this test with ParseComments|SkipObjectResolution:
                    // the parser depth is what is being tested here, not the
                    // resolver's scope depth - parser_test.go:679.)
                    let (_f, err) = parse_file(
                        &mut fset,
                        "",
                        input.as_bytes(),
                        Mode::default() | SKIP_OBJECT_RESOLUTION,
                    );
                    let msg = err.map(|e| {
                        e.0.last()
                            .map(|er| er.msg.clone())
                            .unwrap_or_else(|| e.to_string())
                    });
                    msg
                })
                .expect("spawn depth-test thread");
            let msg = handle.join().expect("depth-test thread panicked");
            match size {
                "small" => assert!(
                    msg.is_none(),
                    "ParseFile({}): {msg:?} (want success)",
                    tt.name
                ),
                _ => {
                    const EXPECTED: &str = "exceeded max nesting depth";
                    assert!(
                        msg.as_deref().is_some_and(|m| m.ends_with(EXPECTED)),
                        "ParseFile({}) = _, {:?}, want {EXPECTED:?}",
                        tt.name,
                        msg
                    );
                }
            }
        }
    }
}

// split splits pre«mid»post into pre, mid, post.
// If the string does not have that form, split returns x, "", "".
fn split(x: &str) -> (&str, &str, &str) {
    match (x.find('«'), x.rfind('»')) {
        (Some(start), Some(end)) if start < end => (
            &x[..start],
            &x[start + '«'.len_utf8()..end],
            &x[end + '»'.len_utf8()..],
        ),
        _ => (x, "", ""),
    }
}

// proposal go.dev/issue/50429
#[test]
fn test_range_pos() {
    let testcases = [
        "package p; func _() { for range x {} }",
        "package p; func _() { for i = range x {} }",
        "package p; func _() { for i := range x {} }",
        "package p; func _() { for k, v = range x {} }",
        "package p; func _() { for k, v := range x {} }",
    ];

    for src in testcases {
        let mut fset = FileSet::new();
        let (f, err) = parse_file(&mut fset, src, src.as_bytes(), Mode::default());
        assert!(err.is_none(), "{src}: {err:?}");

        let mut failures = Vec::new();
        inspect(NodeRef::File(&f), &mut |n| {
            if let Some(NodeRef::RangeStmt(s)) = n {
                let offs = fset.position(s.range).offset;
                let want = src.find("range").unwrap() as i64;
                if offs != want {
                    failures.push(format!("{src}: got offset {offs}, want {want}"));
                }
            }
            true
        });
        expect_failures(failures);
    }
}

// TestIssue59180 tests that line number overflow doesn't cause an infinite
// loop.
#[test]
fn test_issue59180() {
    let testcases = [
        "package p\n//line :9223372036854775806\n\n//",
        "package p\n//line :1:9223372036854775806\n\n//",
        "package p\n//line file:9223372036854775806\n\n//",
    ];

    for src in testcases {
        let mut fset = FileSet::new();
        let (_f, err) = parse_file(&mut fset, "", src.as_bytes(), Mode::default());
        assert!(err.is_some(), "ParseFile({src}) succeeded unexpectedly");
    }
}

#[test]
fn test_issue57490() {
    let src = "package p; func f() { var x struct"; // program not correctly terminated
    let mut fset = FileSet::new();
    let (file, err) = parse_file(&mut fset, "", src.as_bytes(), Mode::default());
    assert!(
        err.is_some(),
        "syntax error expected, but no error reported"
    );

    // Because of the syntax error, the end position of the function declaration
    // is past the end of the file's position range.
    let func_end = file.decls[0].end();

    // Offset(funcEnd) must not panic
    // (panic: offset 35 out of bounds [0, 34] (position 36 out of bounds [1, 35]))
    let tok_file = fset.file(file.pos()).expect("file");
    let offset = tok_file.offset(func_end);
    assert_eq!(
        offset,
        tok_file.size(),
        "offset = {offset}, want {}",
        tok_file.size()
    );
}

#[test]
fn test_parse_type_params_as_paren_expr() {
    let src = "package p; type X[A (B),] struct{}";

    let mut fset = FileSet::new();
    let (f, err) = parse_file(&mut fset, "test.go", src.as_bytes(), Mode::default());
    assert!(err.is_none(), "{err:?}");

    match &f.decls[0] {
        Decl::GenDecl(gd) => match &gd.specs[0] {
            Spec::TypeSpec(ts) => {
                let tparams = ts.type_params.as_ref().expect("type params");
                assert!(
                    matches!(&tparams.list[0].typ, Some(Expr::ParenExpr(_))),
                    "typeParam is {:?}; want: *ast.ParenExpr",
                    tparams.list[0].typ
                );
            }
            other => panic!("expected TypeSpec, got {other:?}"),
        },
        other => panic!("expected GenDecl, got {other:?}"),
    }
}

// TestEmptyFileHasValidStartEnd is a regression test for go.dev/issue/70162.
#[test]
fn test_empty_file_has_valid_start_end() {
    // src  -> (Pos() FileStart FileEnd)
    let cases: &[(&str, (i64, i64, i64))] = &[
        ("", (0, 1, 1)),
        ("package ", (0, 1, 9)),
        ("package p", (1, 1, 10)),
        ("type T int", (0, 1, 11)),
    ];
    for (src, want) in cases {
        let mut fset = FileSet::new();
        let (f, _err) = parse_file(&mut fset, "a.go", src.as_bytes(), Mode::default());
        let got = (f.pos(), f.file_start, f.file_end);
        let want = (
            Pos::from_int(want.0),
            Pos::from_int(want.1),
            Pos::from_int(want.2),
        );
        assert_eq!(got, want, "src = {src:?}");
    }
}

// Tests of BasicLit.End(), which since Go 1.26 precisely records the Value
// token's end position instead of heuristically computing it (inaccurate
// for strings containing "\r").
#[test]
fn test_basic_lit_end() {
    // lit is a raw string literal containing [a b c \r \n],
    // denoting "abc\n", because the scanner normalizes \r\n to \n.
    let stringlit = "`abc\r\n`";

    // The semicolons exercise the case in which the next token
    // (a SEMICOLON implied by a \n) isn't immediate but follows
    // some horizontal space.
    let src = format!(
        "package p\n\nimport {stringlit} ;\n\ntype _ struct{{ x int {stringlit} }}\n\nconst _ = {stringlit} ;\n"
    );

    let mut fset = FileSet::new();
    let (f, _err) = parse_file(&mut fset, "", src.as_bytes(), Mode::default());
    let tok_file = fset.file(f.pos()).expect("file");

    let mut failures = Vec::new();
    let mut count = 0;
    inspect(NodeRef::File(&f), &mut |n| {
        if let Some(NodeRef::BasicLit(lit)) = n {
            count += 1;
            let start = tok_file.offset(lit.pos()) as usize;
            let end = tok_file.offset(lit.end()) as usize;

            // Check BasicLit.Value.
            if lit.value != "`abc\n`" {
                failures.push(format!(
                    "{}: BasicLit.Value = {:?}, want {:?}",
                    fset.position(lit.pos()),
                    lit.value,
                    "`abc\n`"
                ));
            }

            // Check source extent.
            let got = &src[start..end];
            if got != stringlit {
                failures.push(format!(
                    "{}: src[BasicLit.Pos:End] = {got:?}, want {stringlit:?}",
                    fset.position(lit.pos())
                ));
            }
        }
        true
    });
    assert_eq!(count, 3, "found {count} BasicLit, want 3");
    expect_failures(failures);
}

// ----------------------------------------------------------------------------
// example_test.go (ported as a plain test)

#[test]
fn example_parse_file() {
    let src = "package foo\n\nimport (\n\t\"fmt\"\n\t\"time\"\n)\n\nfunc bar() {\n\tfmt.Println(time.Now())\n}";

    // Parse src but stop after processing the imports.
    // (Go: parser.ImportsOnly|parser.SkipObjectResolution)
    let mut fset = FileSet::new();
    let (f, err) = parse_file(&mut fset, "", src.as_bytes(), IMPORTS_ONLY);
    assert!(err.is_none(), "{err:?}");

    // Print the imports from the file's AST.
    let mut got = Vec::new();
    for s in &f.imports {
        got.push(s.path.value.clone());
    }

    // output:
    //
    // "fmt"
    // "time"
    assert_eq!(got, vec!["\"fmt\"".to_string(), "\"time\"".to_string()]);
}

// ----------------------------------------------------------------------------
// Smoke tests kept from the initial wiring round.

fn parse_src(src: &str) -> (crate::ast::File, Option<ErrorList>) {
    let mut fset = FileSet::new();
    parse_file(&mut fset, "main.go", src.as_bytes(), DECLARATION_ERRORS)
}

#[test]
fn smoke_parse_simple_file() {
    let src = "package main\n\nimport \"fmt\"\n\nfunc main() {\n\t// comment\n\tfmt.Println(\"hello\") // trailing\n}\n";
    let (f, err) = parse_src(src);
    assert!(err.is_none(), "unexpected errors: {err:?}");
    assert_eq!(f.name.name, "main");
    assert_eq!(f.decls.len(), 2);
    assert_eq!(f.imports.len(), 1);
    assert!(f.imports[0].name.is_none());
    match &f.decls[1] {
        Decl::FuncDecl(fd) => {
            assert_eq!(fd.name.name, "main");
            let body = fd.body.as_ref().expect("body");
            assert_eq!(body.list.len(), 1);
        }
        other => panic!("expected FuncDecl, got {other:?}"),
    }
    // FileStart/FileEnd are always set (Go's ParseFile defer).
    assert!(f.file_start.is_valid());
    assert!(f.file_end.is_valid());
}

#[test]
fn smoke_block_comment_newline_semi() {
    // A /*...*/ comment containing a newline synthesizes a semicolon; the
    // statement after it must be parsed as a separate statement.
    let src = "package p\n\nfunc f() {\n\ta := 1 /* c\n */\n\tb := 2\n\t_ = a + b\n}\n";
    let (f, err) = parse_src(src);
    assert!(err.is_none(), "unexpected errors: {err:?}");
    match &f.decls[0] {
        Decl::FuncDecl(fd) => {
            let body = fd.body.as_ref().expect("body");
            assert_eq!(body.list.len(), 3, "expected 3 statements");
        }
        other => panic!("expected FuncDecl, got {other:?}"),
    }
}

#[test]
fn smoke_parse_expr_binary() {
    let (x, err) = parse_expr("a + b*c");
    assert!(err.is_none(), "unexpected errors: {err:?}");
    let x = x.expect("expr");
    // a + (b * c): outer binary +, right operand binary *.
    match x {
        Expr::BinaryExpr(b) => {
            assert_eq!(b.op, Token::Add);
            assert!(matches!(&b.y, Expr::BinaryExpr(inner) if inner.op == Token::Mul));
        }
        other => panic!("expected binary +, got {other:?}"),
    }
}

#[test]
fn smoke_generics_and_interfaces() {
    let src = "package p\n\ntype Set[E comparable] map[E]struct{}\n\nfunc (s Set[E]) Add(e E) { s[e] = struct{}{} }\n\ntype Stringer interface {\n\t~string | ~[]byte\n\tString() string\n}\n";
    let (f, err) = parse_src(src);
    assert!(err.is_none(), "unexpected errors: {err:?}");
    assert_eq!(f.decls.len(), 3);
}

#[test]
fn smoke_error_message() {
    let (x, err) = parse_expr("a +");
    // Like Go, a partial AST is returned: `a + BadExpr` (the missing
    // operand is replaced by a bad expression).
    assert!(x.is_some());
    let err = err.expect("errors");
    let msg = err.0[0].msg.clone();
    assert!(
        msg.contains("expected operand"),
        "unexpected error message: {msg}"
    );
}

#[test]
fn smoke_syntax_error_partial_ast() {
    let src = "package p\n\nfunc f() {\n\tif {\n\t}\n}\n";
    let (f, err) = parse_src(src);
    assert!(err.is_some(), "expected errors");
    // Partial AST: the if statement still parses (with a bad condition).
    match &f.decls[0] {
        Decl::FuncDecl(fd) => {
            let body = fd.body.as_ref().expect("body");
            assert_eq!(body.list.len(), 1, "expected the if statement");
        }
        other => panic!("expected FuncDecl, got {other:?}"),
    }
}

// ----------------------------------------------------------------------------
// Resolver tests: restored parser_test.go suites plus resolver_test.go ports.
// (The suites that depend on comment collection remain unported; see the
// module documentation. TestResolveFilePanicConcurrent is not ported: the
// port is single-threaded and files are owned by value, so the resolver
// keeps no shared failure cache - see parser/resolver.rs module docs.)

fn func_body_stmt<'a>(f: &'a crate::ast::File, body_idx: usize) -> &'a Stmt {
    match &f.decls[0] {
        Decl::FuncDecl(fd) => &fd.body.as_ref().expect("body").list[body_idx],
        other => panic!("expected FuncDecl, got {other:?}"),
    }
}

fn as_assign(s: &Stmt) -> &AssignStmt {
    match s {
        Stmt::AssignStmt(a) => a,
        other => panic!("expected AssignStmt, got {other:?}"),
    }
}

fn ident_of(e: &Expr) -> &Ident {
    match e {
        Expr::Ident(id) => id,
        other => panic!("expected Ident, got {other:?}"),
    }
}

#[test]
fn test_colon_equals_scope() {
    let src = "package p; func f() { x, y, z := x, y, z }";
    let (f, err) = parse_src(src);
    assert!(err.is_none(), "{err:?}");

    // RHS refers to undefined globals; LHS does not.
    let as_ = as_assign(func_body_stmt(&f, 0));
    for v in &as_.rhs {
        assert!(
            ident_of(v).obj.is_none(),
            "rhs {} has Obj, should not",
            ident_of(v).name
        );
    }
    for v in &as_.lhs {
        assert!(
            ident_of(v).obj.is_some(),
            "lhs {} does not have Obj, should",
            ident_of(v).name
        );
    }
}

#[test]
fn test_var_scope() {
    let src = "package p; func f() { var x, y, z = x, y, z }";
    let (f, err) = parse_src(src);
    assert!(err.is_none(), "{err:?}");

    // RHS refers to undefined globals; LHS does not.
    let vs = match func_body_stmt(&f, 0) {
        Stmt::DeclStmt(ds) => match &ds.decl {
            Decl::GenDecl(gd) => match &gd.specs[0] {
                Spec::ValueSpec(vs) => vs,
                other => panic!("expected ValueSpec, got {other:?}"),
            },
            other => panic!("expected GenDecl, got {other:?}"),
        },
        other => panic!("expected DeclStmt, got {other:?}"),
    };
    for v in &vs.values {
        assert!(
            ident_of(v).obj.is_none(),
            "rhs {} has Obj, should not",
            ident_of(v).name
        );
    }
    for id in &vs.names {
        assert!(
            id.obj.is_some(),
            "lhs {} does not have Obj, should",
            id.name
        );
    }
}

#[test]
fn test_objects() {
    let src = "package p\nimport fmt \"fmt\"\nconst pi = 3.14\ntype T struct{}\nvar x int\nfunc f() { L: }\n";
    let (f, err) = parse_src(src);
    assert!(err.is_none(), "{err:?}");

    let mut failures = Vec::new();
    inspect(NodeRef::File(&f), &mut |n| {
        if let Some(NodeRef::Ident(id)) = n {
            // (Go's expected-kind table; the package name, the import name
            // and `int` resolve to nothing in this file.)
            let want_kind = match id.name.as_str() {
                "pi" => Some(ObjKind::Con),
                "T" => Some(ObjKind::Typ),
                "x" => Some(ObjKind::Var),
                "f" => Some(ObjKind::Fun),
                "L" => Some(ObjKind::Lbl),
                _ => None,
            };
            match (&id.obj, want_kind) {
                (None, None) => {}
                (None, Some(_)) => failures.push(format!("no object for {}", id.name)),
                (Some(obj), None) => {
                    if !matches!(obj.kind, ObjKind::Bad) {
                        failures.push(format!(
                            "unexpected object for {} (kind {})",
                            id.name, obj.kind
                        ));
                    }
                }
                (Some(obj), Some(kind)) => {
                    if obj.name != id.name {
                        failures.push(format!(
                            "names don't match: obj.Name = {}, ident.Name = {}",
                            obj.name, id.name
                        ));
                    }
                    if obj.kind != kind {
                        failures.push(format!(
                            "{}: obj.Kind = {}; want {}",
                            id.name, obj.kind, kind
                        ));
                    }
                }
            }
        }
        true
    });
    expect_failures(failures);
}

#[test]
fn test_unresolved() {
    let src = "package p\n//\nfunc f1a(int)\nfunc f2a(byte, int, float)\nfunc f3a(a, b int, c float)\nfunc f4a(...complex)\nfunc f5a(a s1a, b ...complex)\n//\nfunc f1b(*int)\nfunc f2b([]byte, (int), *float)\nfunc f3b(a, b *int, c []float)\nfunc f4b(...*complex)\nfunc f5b(a s1a, b ...[]complex)\n//\ntype s1a struct { int }\ntype s2a struct { byte; int; s1a }\ntype s3a struct { a, b int; c float }\n//\ntype s1b struct { *int }\ntype s2b struct { byte; int; *float }\ntype s3b struct { a, b *s3b; c []float }\n";
    let (f, err) = parse_src(src);
    assert!(err.is_none(), "{err:?}");

    let want = "int byte int float int float complex complex int byte int float int float complex complex int byte int int float int byte int float float ";

    // collect unresolved identifiers
    let mut got = String::new();
    for u in &f.unresolved {
        got.push_str(&u.name);
        got.push(' ');
    }

    assert_eq!(got, want, "\ngot:  {got}\nwant: {want}");
}

// ----------------------------------------------------------------------------
// Scope depth limit (Go parser_test.go TestScopeDepthLimit, 695-739): only
// the rows whose scope flag is set are exercised; resolution is on by
// default, so the nested inputs overflow the resolver's scope depth.

struct DepthScopeTest {
    name: &'static str,
    format: &'static str,
    scope_multiplier: i32,
}

const SCOPE_DEPTH_TESTS: &[DepthScopeTest] = &[
    DepthScopeTest {
        name: "struct",
        format: "package main; var x «struct { X «int» }»",
        scope_multiplier: 0,
    },
    DepthScopeTest {
        name: "func",
        format: "package main; var x «func()»int",
        scope_multiplier: 0,
    },
    DepthScopeTest {
        name: "interface",
        format: "package main; var x «interface { M() «int» }»",
        scope_multiplier: 2,
    },
    DepthScopeTest {
        name: "if",
        format: "package main; func main() { «if true { «» }»}",
        scope_multiplier: 2,
    },
    DepthScopeTest {
        name: "ifelse",
        format: "package main; func main() { «if true {} else » {} }",
        scope_multiplier: 0,
    },
    DepthScopeTest {
        name: "switch",
        format: "package main; func main() { «switch { default: «» }»}",
        scope_multiplier: 2,
    },
    DepthScopeTest {
        name: "typeswitch",
        format: "package main; func main() { «switch x.(type) { default: «» }» }",
        scope_multiplier: 2,
    },
    DepthScopeTest {
        name: "for0",
        format: "package main; func main() { «for { «» }» }",
        scope_multiplier: 2,
    },
    DepthScopeTest {
        name: "for1",
        format: "package main; func main() { «for x { «» }» }",
        scope_multiplier: 2,
    },
    DepthScopeTest {
        name: "for3",
        format: "package main; func main() { «for f(); g(); h() { «» }» }",
        scope_multiplier: 2,
    },
    DepthScopeTest {
        name: "forrange0",
        format: "package main; func main() { «for range x { «» }» }",
        scope_multiplier: 2,
    },
    DepthScopeTest {
        name: "forrange1",
        format: "package main; func main() { «for x = range z { «» }» }",
        scope_multiplier: 2,
    },
    DepthScopeTest {
        name: "forrange2",
        format: "package main; func main() { «for x, y = range z { «» }» }",
        scope_multiplier: 2,
    },
    DepthScopeTest {
        name: "go",
        format: "package main; func main() { «go func() { «» }()» }",
        scope_multiplier: 0,
    },
    DepthScopeTest {
        name: "defer",
        format: "package main; func main() { «defer func() { «» }()» }",
        scope_multiplier: 0,
    },
    DepthScopeTest {
        name: "select",
        format: "package main; func main() { «select { default: «» }» }",
        scope_multiplier: 0,
    },
    DepthScopeTest {
        name: "block",
        format: "package main; func main() { «{«»}» }",
        scope_multiplier: 0,
    },
];

/// Builds the depth-test input for `format` with `n` repetitions (Go's
/// "small"/"big" expansion).
fn depth_test_input(format: &str, n: i32) -> String {
    let (pre, mid, post) = split(format);
    let mid = if mid.contains('«') {
        let (left, base, right) = split(&mid);
        left.repeat(n as usize) + base + &right.repeat(n as usize)
    } else {
        mid.repeat(n as usize)
    };
    format!("{pre}{mid}{post}")
}

/// Runs a parse of `input` on a dedicated large-stack thread (Go goroutine
/// stacks grow; Rust thread stacks do not) and returns the last error
/// message, if any.
fn parse_on_big_stack(input: String, mode: Mode) -> Option<String> {
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let mut fset = FileSet::new();
            let (_f, err) = parse_file(&mut fset, "", input.as_bytes(), mode);
            err.map(|e| {
                e.0.last()
                    .map(|er| er.msg.clone())
                    .unwrap_or_else(|| e.to_string())
            })
        })
        .expect("spawn depth-test thread");
    handle.join().expect("depth-test thread panicked")
}

#[test]
fn test_scope_depth_limit() {
    init();
    for tt in SCOPE_DEPTH_TESTS {
        for size in ["small", "big"] {
            let mut n = crate::parser::resolver::MAX_SCOPE_DEPTH + 1;
            if tt.scope_multiplier > 0 {
                n /= tt.scope_multiplier;
            }
            if size == "small" {
                // Decrease the number of statements by 10, in order to check
                // that we do not fail when under the limit. 10 is used to
                // provide some wiggle room for cases where the surrounding
                // scaffolding syntax adds some noise to the depth that
                // changes on a per testcase basis.
                n -= 10;
            }
            let input = depth_test_input(tt.format, n);
            let msg = parse_on_big_stack(input, DECLARATION_ERRORS);
            match size {
                "small" => assert!(
                    msg.is_none(),
                    "ParseFile({}): {msg:?} (want success)",
                    tt.name
                ),
                _ => {
                    const EXPECTED: &str = "exceeded max scope depth during object resolution";
                    assert!(
                        msg.as_deref().is_some_and(|m| m.ends_with(EXPECTED)),
                        "ParseFile({}) = _, {:?}, want {EXPECTED:?}",
                        tt.name,
                        msg
                    );
                }
            }
        }
    }
}

// ----------------------------------------------------------------------------
// resolver_test.go ports

/// Collects the map associating an identifier position with its declaration
/// position (Go's `declsFromParser`).
fn decls_from_parser(file: &crate::ast::File) -> BTreeMap<Pos, Pos> {
    let mut objmap = BTreeMap::new();
    inspect(NodeRef::File(file), &mut |n| {
        // Ignore blank identifiers to reduce noise.
        if let Some(NodeRef::Ident(id)) = n {
            if id.name != "_" {
                if let Some(obj) = &id.obj {
                    objmap.insert(id.name_pos, obj.pos());
                }
            }
        }
        true
    });
    objmap
}

/// Joins declaration and use markers on name, building the map of
/// use->decl (Go's `declsFromComments`).
fn decls_from_comments(handle: &Rc<File>, src: &[u8]) -> BTreeMap<Pos, Pos> {
    let (decls, uses) = position_markers(handle, src);

    let mut objmap = BTreeMap::new();
    for (name, posns) in uses {
        let declpos = decls.get(&name).unwrap_or_else(|| {
            panic!("missing declaration for {name}");
        });
        for pos in posns {
            objmap.insert(pos, *declpos);
        }
    }
    objmap
}

/// Extracts named positions denoted by comments prefixed with '='
/// (declarations) and '@' (uses): for example '@foo' or '=@bar' (Go's
/// `positionMarkers`).
fn position_markers(
    handle: &Rc<File>,
    src: &[u8],
) -> (BTreeMap<String, Pos>, BTreeMap<String, Vec<Pos>>) {
    let mut s = Scanner::new(handle.clone(), src, None, SCAN_COMMENTS);
    let mut decls: BTreeMap<String, Pos> = BTreeMap::new();
    let mut uses: BTreeMap<String, Vec<Pos>> = BTreeMap::new();
    let mut prev = NoPos; // position of last non-comment, non-semicolon token

    loop {
        let (pos, tok, lit) = s.scan();
        match tok {
            Token::EOF => return (decls, uses),
            Token::Comment => {
                let (name, decl, use_) = annotated_obj(&lit);
                if !name.is_empty() {
                    if decl {
                        if decls.contains_key(&name) {
                            panic!("duplicate declaration markers for {name}");
                        }
                        decls.insert(name.clone(), prev);
                    }
                    if use_ {
                        uses.entry(name).or_default().push(prev);
                    }
                }
            }
            Token::Semicolon => {
                // ignore automatically inserted semicolons
                if lit == "\n" {
                    continue;
                }
                prev = pos;
            }
            _ => {
                prev = pos;
            }
        }
    }
}

/// Parses one annotation comment (Go's `annotatedObj`).
fn annotated_obj(lit: &str) -> (String, bool, bool) {
    let bytes = lit.as_bytes();
    let lit = if bytes[1] == b'*' {
        &lit[..lit.len() - 2] // strip trailing */
    } else {
        lit
    };
    let lit = lit[2..].trim();
    let (mut decl, mut use_) = (false, false);
    for (idx, ch) in lit.char_indices() {
        match ch {
            '=' => decl = true,
            '@' => use_ = true,
            _ => {
                return (lit[idx..].to_string(), decl, use_);
            }
        }
    }
    (String::new(), decl, use_)
}

/// Renders a position like Go's subtest helper (filename implied, so
/// omitted).
fn pos_str(fset: &FileSet, pos: Pos) -> String {
    let p = fset.position(pos);
    if p.is_valid() {
        format!("{}:{}", p.line, p.column)
    } else {
        "-".to_string()
    }
}

// TestResolution checks that identifiers are resolved to the declarations
// annotated in the source, by comparing the positions of the resulting
// Ident.Obj.Decl to positions marked in the source via special comments.
#[test]
fn test_resolution() {
    let dir = format!("{TESTDATA}/resolution");
    let entries = fs::read_dir(&dir).expect("read resolution testdata");
    let mut names: Vec<String> = entries
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    for name in names {
        let path = format!("{dir}/{name}");
        let src = read_test_file(&path);
        let mut fset = FileSet::new();
        let (file, err) = parse_file(&mut fset, &path, &src, Mode::default());
        assert!(err.is_none(), "{name}: {err:?}");

        // Compare the positions of objects resolved during parsing
        // (fromParser) to those annotated in source comments (fromComments).
        let handle = fset.file(file.pos()).expect("file");
        let mut from_parser = decls_from_parser(&file);
        let from_comments = decls_from_comments(&handle, &src);

        let mut failures = Vec::new();
        for (k, want) in &from_comments {
            match from_parser.get(k) {
                Some(got) => {
                    if got != want {
                        failures.push(format!(
                            "{} resolved to {}, want {}",
                            pos_str(&fset, *k),
                            pos_str(&fset, *got),
                            pos_str(&fset, *want)
                        ));
                    }
                    from_parser.remove(k);
                }
                None => {
                    failures.push(format!(
                        "{} resolved to none, want {}",
                        pos_str(&fset, *k),
                        pos_str(&fset, *want)
                    ));
                }
            }
        }
        // What remains in fromParser are unexpected resolutions.
        for (k, got) in from_parser {
            failures.push(format!(
                "{} resolved to {}, want no object",
                pos_str(&fset, k),
                pos_str(&fset, got)
            ));
        }
        assert!(failures.is_empty(), "{name}:\n{}", failures.join("\n"));
    }
}

#[test]
fn test_resolve_file() {
    const SRC: &str = "package p; var x = 1; var y = x";
    let mut fset = FileSet::new();
    let (mut f, err) = parse_file(
        &mut fset,
        "example.com/p",
        SRC.as_bytes(),
        SKIP_OBJECT_RESOLUTION,
    );
    assert!(err.is_none(), "{err:?}");

    // before
    let xspec = match &f.decls[0] {
        Decl::GenDecl(gd) => match &gd.specs[0] {
            Spec::ValueSpec(vs) => vs,
            other => panic!("expected ValueSpec, got {other:?}"),
        },
        other => panic!("expected GenDecl, got {other:?}"),
    };
    let yspec = match &f.decls[1] {
        Decl::GenDecl(gd) => match &gd.specs[0] {
            Spec::ValueSpec(vs) => vs,
            other => panic!("expected ValueSpec, got {other:?}"),
        },
        other => panic!("expected GenDecl, got {other:?}"),
    };
    let x = &xspec.names[0];
    let y = &yspec.names[0];
    let xref = ident_of(&yspec.values[0]);

    assert_eq!(x.name, "x", "x: wrong Ident");
    assert_eq!(y.name, "y", "y: wrong Ident");
    assert_eq!(xref.name, "x", "xref: wrong Ident");
    assert!(
        x.obj.is_none(),
        "ParseFile(SkipObjectResolution) unexpectedly set x.Obj"
    );
    assert!(
        y.obj.is_none(),
        "ParseFile(SkipObjectResolution) unexpectedly set y.Obj"
    );
    assert!(
        xref.obj.is_none(),
        "ParseFile(SkipObjectResolution) unexpectedly set xref.Obj"
    );

    // (The by-value decl copies are positionally equal to the tree nodes;
    // Go asserts pointer identity here - a documented deviation.)
    let xspec_pos = x.name_pos;
    let yspec_pos = y.name_pos;

    // ResolveFile(f)
    let handle = fset.file(f.pos()).expect("file");
    crate::parser::resolver::resolve_file(&mut f, handle, None);

    // after
    let xspec = match &f.decls[0] {
        Decl::GenDecl(gd) => match &gd.specs[0] {
            Spec::ValueSpec(vs) => vs,
            other => panic!("expected ValueSpec, got {other:?}"),
        },
        other => panic!("expected GenDecl, got {other:?}"),
    };
    let yspec = match &f.decls[1] {
        Decl::GenDecl(gd) => match &gd.specs[0] {
            Spec::ValueSpec(vs) => vs,
            other => panic!("expected ValueSpec, got {other:?}"),
        },
        other => panic!("expected GenDecl, got {other:?}"),
    };
    let x = &xspec.names[0];
    let y = &yspec.names[0];
    let xref = ident_of(&yspec.values[0]);

    let xobj = x.obj.clone().expect("after ResolveFile, x.Obj is nil");
    let yobj = y.obj.clone().expect("after ResolveFile, y.Obj is nil");
    let xrefobj = xref
        .obj
        .clone()
        .expect("after ResolveFile, xref.Obj is nil");

    assert_eq!(xobj.kind, ObjKind::Var, "after ResolveFile, x.Obj.Kind");
    assert_eq!(yobj.kind, ObjKind::Var, "after ResolveFile, y.Obj.Kind");
    assert_eq!(
        xrefobj.kind,
        ObjKind::Var,
        "after ResolveFile, xref.Obj.Kind"
    );
    assert_eq!(xobj.name, "x", "after ResolveFile, x.Obj.Name");
    assert_eq!(yobj.name, "y", "after ResolveFile, y.Obj.Name");
    assert!(
        Rc::ptr_eq(&xobj, &xrefobj),
        "after ResolveFile, x.Obj != xref.Obj"
    );
    // (Go: x.Obj.Decl == xspec / y.Obj.Decl == yspec; position equality
    // replaces pointer identity - see the scope.rs module documentation.)
    match &xobj.decl {
        Some(ObjectDecl::ValueSpec(vs)) => {
            assert_eq!(
                vs.names[0].name_pos, xspec_pos,
                "after ResolveFile, x.Obj.Decl position"
            );
        }
        other => panic!("x.Obj.Decl is {other:?}, want ValueSpec"),
    }
    match &yobj.decl {
        Some(ObjectDecl::ValueSpec(vs)) => {
            assert_eq!(
                vs.names[0].name_pos, yspec_pos,
                "after ResolveFile, y.Obj.Decl position"
            );
        }
        other => panic!("y.Obj.Decl is {other:?}, want ValueSpec"),
    }

    // ResolveFile(f) again - idempotent; no ill effects.
    let handle = fset.file(f.pos()).expect("file");
    crate::parser::resolver::resolve_file(&mut f, handle, None);
    let x2 = match &f.decls[0] {
        Decl::GenDecl(gd) => match &gd.specs[0] {
            Spec::ValueSpec(vs) => &vs.names[0],
            other => panic!("expected ValueSpec, got {other:?}"),
        },
        other => panic!("expected GenDecl, got {other:?}"),
    };
    assert!(x2.obj.is_some(), "after second ResolveFile, x.Obj is nil");
}

// TestResolveFilePanic checks that two calls to ResolveFile observe the
// same behavior when it panics.
//
// (Each call runs a fresh resolution - no shared failure cache. The depth
// guard fires before any object is written, so both calls panic with the
// identical payload. Go additionally caches the panic and supports
// concurrent callers; see the resolver.rs module docs. Everything runs on a
// dedicated large-stack thread: both the ~1000-level parse and the
// resolution recursion exceed the default test-thread stack, and `File`
// becomes !Send once it carries `Rc` objects.)
#[test]
fn test_resolve_file_panic() {
    init();
    const N: usize = 1001;
    let src = format!(
        "package p; func f() {{{}{}}}",
        "if true {".repeat(N),
        "}".repeat(N)
    );

    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let mut fset = FileSet::new();
            let (mut f, err) = parse_file(&mut fset, "", src.as_bytes(), SKIP_OBJECT_RESOLUTION);
            assert!(err.is_none(), "{err:?}");
            let handle = fset.file(f.pos()).expect("file");

            // The first run stops at the scope-depth bailout; its traversal
            // has already marked identifiers (e.g. the `true` conditions)
            // with the unresolved sentinel, so a second fresh run panics in
            // the "already declared or resolved" guard instead of reaching
            // the depth limit. (Go re-panics the cached first payload; with
            // the singleflight cache dropped, only "both calls panic and
            // the scope stays nil" is portable - see the resolver docs.)
            let run = |f: &mut crate::ast::File, handle: &Rc<File>| -> Bailout {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::parser::resolver::resolve_file(f, handle.clone(), None);
                })) {
                    Ok(()) => panic!("ResolveFile did not panic"),
                    Err(payload) => match payload.downcast::<Bailout>() {
                        Ok(b) => *b,
                        Err(payload) => std::panic::resume_unwind(payload),
                    },
                }
            };

            let first = run(&mut f, &handle);
            assert_eq!(
                first.msg, "exceeded max scope depth during object resolution",
                "first panic {first:?}"
            );
            assert!(
                f.scope.is_none(),
                "ResolveFile set File.Scope after a failed resolution"
            );

            // Second call: must panic too (payload type is not portable -
            // see above).
            let second = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::parser::resolver::resolve_file(&mut f, handle.clone(), None);
            }));
            assert!(
                second.is_err(),
                "ResolveFile returned after a failed resolution"
            );
            assert!(
                f.scope.is_none(),
                "ResolveFile set File.Scope after a failed resolution"
            );
        })
        .expect("spawn resolve-file-panic thread");
    handle.join().expect("resolve-file-panic thread panicked");
}
