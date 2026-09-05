//! `gane-driver`: turn a Go source file into an AST and save it as a debug
//! dump - the porting-workbench entry point for the `go/parser` port.
//!
//! Reads one Go source file, parses it with the ported parser and writes
//! generated files named after the input (for an input `foo.go`):
//!
//! - `<output-dir>/foo.go.ast.txt` - the parsed AST, debug-printed
//!   (`format!("{:#?}")` of the [`gane_parser::ast::File`]). It is written even when the
//!   source has syntax errors: like Go, the result is then a partial AST
//!   with `Bad*` nodes (or an empty file when parsing bailed out).
//! - `<output-dir>/foo.go.err.txt` - the parse errors, one per line; only
//!   created when the source had errors.
//!
//! By default the deprecated identifier resolution ([`SKIP_OBJECT_RESOLUTION`],
//! the mode recommended by Go for new programs) is skipped: the AST
//! back-pointers it sets (`Ident::obj`, `File::scope` objects) are printed by
//! the derived `Debug` impls, and since `Object::decl` holds by-value copies
//! of the declaration subtrees, printing them re-expands those subtrees for
//! every referencing identifier - dumps grow quadratically with the file.
//! `--resolve` turns the resolution on for inspecting its results on small
//! files.
//!
//! Exit codes: 0 ok, 1 the source had parse errors (also reported on
//! stderr), 2 usage or I/O error.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use gane_parser::parser::{Mode, SKIP_OBJECT_RESOLUTION, parse_file};
use gane_parser::token::FileSet;

const USAGE: &str = "\
usage: gane-driver [--resolve] <file.go> [output-dir]

Parses the Go source file <file.go> with the ported go/parser and writes the
AST as generated files named after the input (for an input \"foo.go\"):

    <output-dir>/foo.go.ast.txt    the parsed AST, debug-printed ({:#?})
    <output-dir>/foo.go.err.txt    the parse errors, one per line
                                   (created only when errors occur)

    --resolve       also run the deprecated identifier resolution
                    (resolver.go). Off by default: the Ident/Scope
                    back-pointers it sets are debug-printed by value, which
                    makes dumps grow quadratically with the file.
output-dir defaults to \"out\". Exit codes: 0 ok, 1 parse errors, 2 usage or
I/O error.
";

fn main() -> ExitCode {
    // --- command line -----------------------------------------------------
    let mut resolve = false;
    let mut positional: Vec<PathBuf> = Vec::new();
    for arg in env::args_os().skip(1) {
        let s = arg.to_string_lossy().into_owned();
        match s.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--resolve" => resolve = true,
            _ if s.starts_with('-') => {
                eprintln!("gane-driver: unknown option `{s}`\n\n{USAGE}");
                return ExitCode::from(2);
            }
            _ => positional.push(PathBuf::from(arg)),
        }
    }
    let (input, out_dir) = match positional.as_slice() {
        [input] => (input.clone(), PathBuf::from("out")),
        [input, out_dir] => (input.clone(), out_dir.clone()),
        _ => {
            eprintln!("gane-driver: expected <file.go> [output-dir]\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    // --- src -> AST -------------------------------------------------------
    let src = match fs::read(&input) {
        Ok(src) => src,
        Err(e) => {
            eprintln!("gane-driver: cannot read `{}`: {e}", input.display());
            return ExitCode::from(2);
        }
    };
    let filename = input.to_string_lossy().into_owned();
    let mode = if resolve {
        Mode::default()
    } else {
        SKIP_OBJECT_RESOLUTION
    };
    let mut fset = FileSet::new();
    let (ast, errors) = parse_file(&mut fset, &filename, &src, mode);

    // --- save the generated files ----------------------------------------
    let stem = match input.file_name() {
        Some(stem) => stem.to_string_lossy().into_owned(),
        None => {
            eprintln!("gane-driver: `{}` has no file name", input.display());
            return ExitCode::from(2);
        }
    };
    if let Err(e) = fs::create_dir_all(&out_dir) {
        eprintln!(
            "gane-driver: cannot create output dir `{}`: {e}",
            out_dir.display()
        );
        return ExitCode::from(2);
    }

    let dump_path = out_dir.join(format!("{stem}.ast.txt"));
    let dump = format!("{ast:#?}");
    if let Err(e) = fs::write(&dump_path, &dump) {
        eprintln!("gane-driver: cannot write `{}`: {e}", dump_path.display());
        return ExitCode::from(2);
    }
    println!(
        "gane-driver: wrote {} ({} bytes, {} top-level declarations)",
        dump_path.display(),
        dump.len(),
        ast.decls.len()
    );

    if let Some(errors) = &errors {
        let mut text = String::new();
        for e in &errors.0 {
            text.push_str(&e.to_string());
            text.push('\n');
        }
        let err_path = out_dir.join(format!("{stem}.err.txt"));
        if let Err(e) = fs::write(&err_path, &text) {
            eprintln!("gane-driver: cannot write `{}`: {e}", err_path.display());
            return ExitCode::from(2);
        }
        for e in &errors.0 {
            eprintln!("gane-driver: {}", e);
        }
        eprintln!(
            "gane-driver: source has {} parse error(s), see `{}`",
            errors.len(),
            err_path.display()
        );
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
