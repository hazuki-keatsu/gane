//! `gane-driver`: run one Go source file through the implemented compiler stages.
//!
//! Reads one Go source file, parses it, semantically checks it, lowers verified IR, emits LLVM
//! IR, interprets it, and writes
//! generated files named after the input (for an input `foo.go`):
//!
//! - `<output-dir>/foo.go.ast.txt` - the parsed AST, debug-printed
//!   (`format!("{:#?}")` of the [`gane_parser::ast::File`]). It is written even when the
//!   source has syntax errors: like Go, the result is then a partial AST
//!   with `Bad*` nodes (or an empty file when parsing bailed out).
//! - `<output-dir>/foo.go.sema.txt` - the semantic analysis result,
//!   debug-printed after a successful parse.
//! - `<output-dir>/foo.go.err.txt` - the parse errors, one per line; only
//!   created when the source had errors.
//! - `<output-dir>/foo.go.sema.err.txt` - semantic diagnostics; only created
//!   when semantic analysis reports an error.
//! - `<output-dir>/foo.go.ir.txt` - verified IR, only created after successful lowering and
//!   escape checking.
//! - `<output-dir>/foo.go.ll` - verified LLVM IR, only created after successful codegen.
//!
//! Exit codes: 0 ok, 1 a compiler stage or the interpreter failed (also reported
//! on stderr), 2 usage or I/O error.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use gane_codegen::LlvmBackend;
use gane_ir::{interpret, lower_package, verify_and_check_escape};
use gane_parser::parser::{Mode, parse_file};
use gane_parser::token::FileSet;
use gane_sema::{FileId, PackageInput, analyze_package};

const USAGE: &str = "\
usage: gane-driver <file.go> [output-dir]

Parses, semantically checks, lowers, verifies, and interprets <file.go>,
writing generated files named after the input (for an input \"foo.go\"):

    <output-dir>/foo.go.ast.txt    the parsed AST, debug-printed ({:#?})
    <output-dir>/foo.go.sema.txt   the semantic analysis result, debug-printed
    <output-dir>/foo.go.err.txt    the parse errors, one per line
                                   (created only when errors occur)
    <output-dir>/foo.go.sema.err.txt semantic diagnostics with source locations
                                     (created only when errors occur)
    <output-dir>/foo.go.ir.txt    verified IR (created only on success)
    <output-dir>/foo.go.ll        verified LLVM IR (created only on success)

output-dir defaults to \"out\". Exit codes: 0 ok, 1 a compiler stage or the
interpreter failed, 2 usage or I/O error.
";

fn main() -> ExitCode {
    // --- command line -----------------------------------------------------
    let mut positional: Vec<PathBuf> = Vec::new();
    for arg in env::args_os().skip(1) {
        let s = arg.to_string_lossy().into_owned();
        match s.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
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
    let mut fset = FileSet::new();
    let (ast, errors) = parse_file(&mut fset, &filename, &src, Mode::default());

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
        for e in errors.iter() {
            text.push_str(&e.to_string());
            text.push('\n');
        }
        let err_path = out_dir.join(format!("{stem}.err.txt"));
        if let Err(e) = fs::write(&err_path, &text) {
            eprintln!("gane-driver: cannot write `{}`: {e}", err_path.display());
            return ExitCode::from(2);
        }
        for e in errors.iter() {
            eprintln!("gane-driver: {}", e);
        }
        eprintln!(
            "gane-driver: source has {} parse error(s), see `{}`",
            errors.len(),
            err_path.display()
        );
        return ExitCode::FAILURE;
    }

    // Only a syntax-clean AST reaches sema; semantic diagnostics then share
    // the parser's FileSet for source-aware rendering.
    let package = PackageInput::single("main", FileId::from_raw(1), &ast);
    let analysis = analyze_package(package.clone());
    let sema_dump_path = out_dir.join(format!("{stem}.sema.txt"));
    let sema_dump = format!("{analysis:#?}");
    if let Err(e) = fs::write(&sema_dump_path, &sema_dump) {
        eprintln!(
            "gane-driver: cannot write `{}`: {e}",
            sema_dump_path.display()
        );
        return ExitCode::from(2);
    }
    println!(
        "gane-driver: wrote {} ({} bytes, {} semantic diagnostic(s))",
        sema_dump_path.display(),
        sema_dump.len(),
        analysis.diagnostics.len()
    );

    if analysis.has_errors() {
        let mut text = String::new();
        for diagnostic in &analysis.diagnostics {
            text.push_str(&diagnostic.display_with(&fset).to_string());
        }
        let sema_err_path = out_dir.join(format!("{stem}.sema.err.txt"));
        if let Err(e) = fs::write(&sema_err_path, &text) {
            eprintln!(
                "gane-driver: cannot write `{}`: {e}",
                sema_err_path.display()
            );
            return ExitCode::from(2);
        }
        for diagnostic in &analysis.diagnostics {
            eprint!("gane-driver: {}", diagnostic.display_with(&fset));
        }
        eprintln!(
            "gane-driver: source has {} semantic error(s), see `{}`",
            analysis
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == gane_sema::Severity::Error)
                .count(),
            sema_err_path.display()
        );
        return ExitCode::FAILURE;
    }

    let backend = match LlvmBackend::for_host() {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!("gane-driver: LLVM backend initialization failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let raw_ir = match lower_package(&package, &analysis, backend.target_spec().clone()) {
        Ok(package) => package,
        Err(error) => {
            eprintln!("gane-driver: IR lowering failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let ir = match verify_and_check_escape(raw_ir) {
        Ok(package) => package,
        Err(diagnostics) => {
            for diagnostic in diagnostics {
                eprintln!("gane-driver: IR verification failed: {diagnostic}");
            }
            return ExitCode::FAILURE;
        }
    };
    let ir_path = out_dir.join(format!("{stem}.ir.txt"));
    let ir_dump = ir.to_string();
    if let Err(e) = fs::write(&ir_path, &ir_dump) {
        eprintln!("gane-driver: cannot write `{}`: {e}", ir_path.display());
        return ExitCode::from(2);
    }
    println!(
        "gane-driver: wrote {} ({} bytes, verified IR)",
        ir_path.display(),
        ir_dump.len()
    );

    let llvm_ir = match backend.emit_llvm_ir(&ir) {
        Ok(llvm_ir) => llvm_ir,
        Err(error) => {
            eprintln!("gane-driver: LLVM codegen failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let llvm_path = out_dir.join(format!("{stem}.ll"));
    if let Err(error) = fs::write(&llvm_path, &llvm_ir) {
        eprintln!(
            "gane-driver: cannot write `{}`: {error}",
            llvm_path.display()
        );
        return ExitCode::from(2);
    }
    println!(
        "gane-driver: wrote {} ({} bytes, verified LLVM IR)",
        llvm_path.display(),
        llvm_ir.len()
    );

    match interpret(&ir) {
        Ok(()) => {
            println!("gane-driver: interpreter completed successfully");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("gane-driver: interpreter failed: {error}");
            ExitCode::FAILURE
        }
    }
}
