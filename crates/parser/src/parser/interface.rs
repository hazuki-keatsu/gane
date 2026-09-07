//! Interface to parse Go source files.
//!
//! This module is ported from Go's standard `go/parser/interface.go`,
//! providing the exported parser entry points. The Rust port deviates from
//! Go in the ways described in the [`super`] module documentation; in
//! particular, `Mode` drops the `ParseComments` and `Trace` bits: ordinary
//! comments are ignored, while supported compiler command comments are always
//! collected and tracing is not ported.
//!
//! Go's `readSource` is dropped: the entry points always take `&[u8]` source
//! (file reading happens at the call site, and the Go test files read their
//! fixtures with `os.ReadFile`).

use std::collections::BTreeMap;
use std::fs;
use std::ops::{BitAnd, BitOr};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use crate::ast::{Expr, File, Package, new_ident};
use crate::scanner::ErrorList;
use crate::token::{FileSet, NO_POS, Pos};

use super::parser::{Bailout, Parser};

/// A Mode value is a set of flags (or 0). They control parser behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mode(u8);

/// Stop parsing after the package clause.
pub const PACKAGE_CLAUSE_ONLY: Mode = Mode(1);

/// Stop parsing after the imports are parsed.
pub const IMPORTS_ONLY: Mode = Mode(2);

// (Go's ParseComments and Trace mode bits are not ported; see the module
// documentation. Their bit values 1<<2 and 1<<3 are unused.)

/// Report declaration errors.
pub const DECLARATION_ERRORS: Mode = Mode(1 << 4);

/// Same as AllErrors, for backward-compatibility.
pub const SPURIOUS_ERRORS: Mode = Mode(1 << 5);

/// AllErrors is a legacy alias for SpuriousErrors.
pub const ALL_ERRORS: Mode = SPURIOUS_ERRORS;

impl BitAnd for Mode {
    type Output = Mode;

    fn bitand(self, rhs: Mode) -> Mode {
        Mode(self.0 & rhs.0)
    }
}

impl BitOr for Mode {
    type Output = Mode;

    fn bitor(self, rhs: Mode) -> Mode {
        Mode(self.0 | rhs.0)
    }
}

/// Runs a parse closure, translating Go's `bailout` panic into an Err result
/// (Go's `recover()` in the ParseFile/ParseExprFrom defers). Any other panic
/// is resumed, exactly like Go re-panics it (interface.go:99-107).
///
/// (Go's defer then re-adds `bail.msg` to the error list when it is
/// non-empty - that branch is Go's interface.go:104-106 and is applied by
/// the callers via [`add_bailout_msg`].)
fn catch_bailout<T>(f: impl FnOnce() -> T) -> Result<T, Bailout> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Ok(v),
        Err(payload) => match payload.downcast::<Bailout>() {
            Ok(b) => Err(*b),
            Err(payload) => resume_unwind(payload),
        },
    }
}

/// Adds a non-empty bailout message to the parse error list.
fn add_bailout_msg(
    errors: &std::rc::Rc<std::cell::RefCell<ErrorList>>,
    file: &crate::token::File,
    b: &Bailout,
) {
    if !b.msg.is_empty() {
        errors.borrow_mut().add(file.position(b.pos), b.msg.clone());
    }
}

/// ParseFile parses the source code of a single Go source file and returns
/// the corresponding [`File`] node.
///
/// Position information is recorded in the file set `fset`.
///
/// If the source was read but syntax errors were found, the result is a
/// partial AST (with `Bad*` nodes representing the fragments of erroneous
/// source code); the second result is the (sorted) error list.
///
/// (Go's nil-fset panic and source-reading paths are dropped: `fset` cannot
/// be nil in this port, and the source is always provided via `src`.)
pub fn parse_file(
    fset: &mut FileSet,
    filename: &str,
    src: &[u8],
    mode: Mode,
) -> (File, Option<ErrorList>) {
    let file = fset.add_file(filename, -1, src.len() as i64);
    let mut p = Parser::new(file.clone(), src, mode);

    let f = match catch_bailout(|| p.parse_file()) {
        Ok(f) => f,
        Err(b) => {
            add_bailout_msg(&p.errors_rc(), &file, &b);
            None
        }
    };

    // set result values
    let mut f = match f {
        Some(f) => f,
        None => {
            // source is not a valid Go source file - satisfy the ParseFile
            // API and return a valid (but) empty *ast.File
            File {
                commands: Vec::new(),
                package: NO_POS,
                name: new_ident(""),
                decls: Vec::new(),
                file_start: NO_POS,
                file_end: NO_POS,
                imports: Vec::new(),
            }
        }
    };

    // Ensure the start/end are consistent,
    // whether parsing succeeded or not.
    f.file_start = Pos::from_int(file.base());
    f.file_end = file.end();

    let err = p.sorted_errors();
    (f, err)
}

/// ParseDir calls [`parse_file`] for all files with names ending in ".go" in
/// the directory specified by `path` and returns a map of package name ->
/// package AST with all the packages found.
///
/// If `filter` is not None, only the files whose names pass through the
/// filter (and ending in ".go") are considered. The mode bits are passed to
/// [`parse_file`] unchanged. Position information is recorded in `fset`.
///
/// If the directory couldn't be read, an empty map and the respective error
/// are returned. If a parse error occurred, a non-nil but incomplete map and
/// the first error encountered are returned.
///
/// (Go's `filter func(fs.FileInfo) bool` is adapted to filter on the file
/// name, which is all the Go tests use.)
///
/// Deprecated: ParseDir does not consider build tags when associating files
/// with packages. (Go's deprecation note, kept verbatim.)
pub fn parse_dir(
    fset: &mut FileSet,
    path: &str,
    filter: Option<&dyn Fn(&str) -> bool>,
    mode: Mode,
) -> (BTreeMap<String, Package>, Option<ErrorList>) {
    let mut first: Option<ErrorList> = None;
    let mut pkgs: BTreeMap<String, Package> = BTreeMap::new();

    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) => {
            let mut errs = ErrorList::default();
            errs.add(
                crate::token::Position {
                    file_name: String::new(),
                    offset: 0,
                    line: 0,
                    column: 0,
                },
                format!("open {path}: {e}"),
            );
            return (pkgs, Some(errs));
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                if first.is_none() {
                    let mut errs = ErrorList::default();
                    errs.add(crate::token::Position::default(), e.to_string());
                    first = Some(errs);
                }
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().map_or(true, |t| t.is_dir()) || !name.ends_with(".go") {
            continue;
        }
        if let Some(filter) = filter {
            if !filter(&name) {
                continue;
            }
        }
        let filename = format!("{path}/{name}");
        let src = match fs::read(&filename) {
            Ok(src) => src,
            Err(e) => {
                if first.is_none() {
                    let mut errs = ErrorList::default();
                    errs.add(crate::token::Position::default(), e.to_string());
                    first = Some(errs);
                }
                continue;
            }
        };
        let (src, err) = parse_file(fset, &filename, &src, mode);
        if err.is_none() {
            let name = src.name.name.clone();
            let pkg = pkgs.entry(name.clone()).or_insert_with(|| Package {
                name,
                files: BTreeMap::new(),
            });
            pkg.files.insert(filename, src);
        } else if first.is_none() {
            first = err;
        }
    }

    (pkgs, first)
}

/// ParseExprFrom is a convenience function for parsing an expression.
/// The arguments have the same meaning as for [`parse_file`], but the source
/// must be a valid Go (type or value) expression.
///
/// If the source was read but syntax errors were found, the result is a
/// partial AST (with `Bad*` nodes representing the fragments of erroneous
/// source code); the second result is the (sorted) error list.
pub fn parse_expr_from(
    fset: &mut FileSet,
    filename: &str,
    src: &[u8],
    mode: Mode,
) -> (Option<Expr>, Option<ErrorList>) {
    let file = fset.add_file(filename, -1, src.len() as i64);
    let mut p = Parser::new(file.clone(), src, mode);

    let x = match catch_bailout(|| {
        // parse expr
        let x = p.parse_rhs();

        // If a semicolon was inserted, consume it;
        // report an error if there's more tokens.
        if p.tok == crate::token::Token::Semicolon && p.lit == "\n" {
            p.next();
        }
        p.expect(crate::token::Token::EOF);
        x
    }) {
        Ok(x) => Some(x),
        Err(b) => {
            // (No resolution runs for expressions, so b.msg is always
            // empty here; the branch is kept for symmetry with Go's
            // duplicated defer in ParseExprFrom.)
            add_bailout_msg(&p.errors_rc(), &file, &b);
            None
        }
    };

    let err = p.sorted_errors();
    (x, err)
}

/// ParseExpr is a convenience function for obtaining the AST of an
/// expression `x`. The position information recorded in the AST is
/// undefined. The filename used in error messages is the empty string.
///
/// If syntax errors were found, the result is a partial AST (with `Bad*`
/// nodes representing the fragments of erroneous source code); the second
/// result is the (sorted) error list.
pub fn parse_expr(x: &str) -> (Option<Expr>, Option<ErrorList>) {
    let mut fset = FileSet::new();
    parse_expr_from(&mut fset, "", x.as_bytes(), Mode::default())
}
