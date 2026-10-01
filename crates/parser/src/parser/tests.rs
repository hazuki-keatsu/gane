//! Ported Go parser tests.
//!
//! This module holds the test suites ported from go/parser's test files:
//! error_test.go (wholesale), short_test.go (wholesale), selected parser_test.go
//! cases, and the example_test.go example. Suites that depend on comment
//! collection, tracing, AST printing, or deprecated identifier resolution are
//! omitted. Go's file reading (`ParseFile` with a nil src) is done here with
//! `std::fs`, and the testdata fixtures live at
//! `src/parser/testdata` (cargo runs tests with the crate root as cwd).

use std::collections::BTreeMap;
use std::fs;
use std::rc::Rc;
use std::sync::Once;

use regex::Regex;

use super::*;
use crate::ast::{CommentCommandKind, NodeRef, inspect};
use crate::parser::interface::*;
use crate::scanner::{ErrorList, SCAN_COMMENTS, Scanner};
use crate::token::{File, FileSet, NO_POS, Pos};

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
    NO_POS
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
    let mut prev = NO_POS; // position of last non-comment, non-semicolon token
    let mut here = NO_POS; // position immediately after the token at position prev

    let rx = err_rx();
    loop {
        let (pos, tok, lit) = s.scan();
        let end = s.end();

        match tok {
            Token::EOF => return errors,
            Token::Comment => {
                if let Some(caps) = rx.captures(&lit)
                    && caps.len() == 3
                {
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
    for error in found.iter() {
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
    let mut found = err.unwrap_or_default();
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
    // Repeated receiver type parameters are a semantic-analysis concern.
    r#"package p; func(*T[e, e]) _()"#,
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

        let mut pos = NO_POS;
        let mut end = NO_POS;
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
        let first_err = err.iter().next().expect("ErrorList is empty");
        assert!(
            first_err.msg.contains(WANT_ERR),
            "ParseFile returned wrong error {:?}, want {WANT_ERR:?}",
            first_err.msg
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
                let (left, base, right) = split(mid);
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
                    // The parser depth is what is being tested here.
                    let (_f, err) = parse_file(&mut fset, "", input.as_bytes(), Mode::default());
                    err.map(|e| {
                        e.iter()
                            .last()
                            .map(|er| er.msg.clone())
                            .unwrap_or_else(|| e.to_string())
                    })
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
fn compiler_commands_bind_to_their_immediately_following_nodes() {
    let src = r#"//go:build linux && amd64
//gane:package enabled
package p

//gane:decl type declaration
type T struct {
	//go:field retained exactly
	Value int
}

var (
	//go:linkname local remote
	local int
)
"#;
    let (f, err) = parse_src(src);
    assert!(err.is_none(), "unexpected errors: {err:?}");

    assert_eq!(f.commands.len(), 2);
    assert_eq!(f.commands[0].kind, CommentCommandKind::Go);
    assert_eq!(f.commands[0].text, "build linux && amd64");
    assert_eq!(f.commands[1].kind, CommentCommandKind::Gane);
    assert_eq!(f.commands[1].text, "package enabled");

    let Decl::GenDecl(type_decl) = &f.decls[0] else {
        panic!("expected type declaration")
    };
    assert_eq!(type_decl.commands.len(), 1);
    assert_eq!(type_decl.commands[0].text, "decl type declaration");
    let Spec::TypeSpec(type_spec) = &type_decl.specs[0] else {
        panic!("expected type spec")
    };
    let Expr::StructType(struct_type) = &type_spec.typ else {
        panic!("expected struct type")
    };
    let field = &struct_type.fields.as_ref().expect("fields").list[0];
    assert_eq!(field.commands.len(), 1);
    assert_eq!(field.commands[0].text, "field retained exactly");

    let Decl::GenDecl(var_decl) = &f.decls[1] else {
        panic!("expected var declaration")
    };
    assert!(var_decl.commands.is_empty());
    let Spec::ValueSpec(value_spec) = &var_decl.specs[0] else {
        panic!("expected value spec")
    };
    assert_eq!(value_spec.commands.len(), 1);
    assert_eq!(value_spec.commands[0].text, "linkname local remote");
}

#[test]
fn file_commands_allow_a_blank_line_before_the_package_clause() {
    let src = r#"//go:build x86

package p
"#;
    let (f, err) = parse_src(src);
    assert!(err.is_none(), "unexpected errors: {err:?}");

    assert_eq!(f.commands.len(), 1);
    assert_eq!(f.commands[0].kind, CommentCommandKind::Go);
    assert_eq!(f.commands[0].text, "build x86");
}

#[test]
fn compiler_commands_require_a_leading_adjacent_line() {
    let src = r#"package p

//go:noescape

func skipped() {}

var trailing int //gane:ignored trailing

// Go:ignored case
//foo:ignored prefix
func clean() {}
"#;
    let (f, err) = parse_src(src);
    assert!(err.is_none(), "unexpected errors: {err:?}");
    assert!(f.commands.is_empty());

    let Decl::FuncDecl(skipped) = &f.decls[0] else {
        panic!("expected first function")
    };
    assert!(skipped.commands.is_empty(), "empty line must break binding");
    let Decl::GenDecl(var_decl) = &f.decls[1] else {
        panic!("expected variable declaration")
    };
    assert!(
        var_decl.commands.is_empty(),
        "trailing command must be ignored"
    );
    let Decl::FuncDecl(clean) = &f.decls[2] else {
        panic!("expected second function")
    };
    assert!(
        clean.commands.is_empty(),
        "unsupported prefixes must be ignored"
    );
}

#[test]
fn compiler_commands_bind_to_grouped_specs_and_signature_fields() {
    let src = r#"package p

import (
	//gane:import fmt package
	"fmt"
)

type (
	//go:type grouped type
	T int
)

func f(
	//gane:param first input
	x int,
	//go:param second input
	y string,
) (
	//gane:result output value
	out error,
) {}

type I interface {
	//gane:method interface member
	M()
}
"#;
    let (f, err) = parse_src(src);
    assert!(err.is_none(), "unexpected errors: {err:?}");

    let Decl::GenDecl(import_decl) = &f.decls[0] else {
        panic!("expected import declaration")
    };
    let Spec::ImportSpec(import_spec) = &import_decl.specs[0] else {
        panic!("expected import spec")
    };
    assert_eq!(import_spec.commands[0].text, "import fmt package");

    let Decl::GenDecl(type_decl) = &f.decls[1] else {
        panic!("expected type declaration")
    };
    let Spec::TypeSpec(type_spec) = &type_decl.specs[0] else {
        panic!("expected type spec")
    };
    assert_eq!(type_spec.commands[0].text, "type grouped type");

    let Decl::FuncDecl(func_decl) = &f.decls[2] else {
        panic!("expected function declaration")
    };
    let params = &func_decl.typ.params.as_ref().expect("parameters").list;
    assert_eq!(params.len(), 2);
    assert_eq!(params[0].commands[0].text, "param first input");
    assert_eq!(params[1].commands[0].text, "param second input");
    let results = &func_decl.typ.results.as_ref().expect("results").list;
    assert_eq!(results[0].commands[0].text, "result output value");

    let Decl::GenDecl(interface_decl) = &f.decls[3] else {
        panic!("expected interface declaration")
    };
    let Spec::TypeSpec(interface_spec) = &interface_decl.specs[0] else {
        panic!("expected interface type spec")
    };
    let Expr::InterfaceType(interface_type) = &interface_spec.typ else {
        panic!("expected interface type")
    };
    let method = &interface_type.methods.as_ref().expect("methods").list[0];
    assert_eq!(method.commands[0].text, "method interface member");
}

#[test]
fn compiler_commands_keep_order_and_use_physical_lines() {
    let src = concat!(
        "package p\n\n",
        "//line generated.go:100\n",
        "//go:one first\n",
        "//gane:two\\tsecond  \n",
        "func f() {}\n\n",
        "//go:\n",
        "func empty() {}\n",
    );
    let (f, err) = parse_src(src);
    assert!(err.is_none(), "unexpected errors: {err:?}");

    let Decl::FuncDecl(f_decl) = &f.decls[0] else {
        panic!("expected first function")
    };
    assert_eq!(f_decl.commands.len(), 2);
    assert_eq!(f_decl.commands[0].kind, CommentCommandKind::Go);
    assert_eq!(f_decl.commands[0].text, "one first");
    assert_eq!(f_decl.commands[1].kind, CommentCommandKind::Gane);
    assert_eq!(f_decl.commands[1].text, "two\\tsecond  ");

    let Decl::FuncDecl(empty_decl) = &f.decls[1] else {
        panic!("expected second function")
    };
    assert_eq!(empty_decl.commands.len(), 1);
    assert_eq!(empty_decl.commands[0].text, "");
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
    let msg = err.iter().next().expect("ErrorList is empty").msg.clone();
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
