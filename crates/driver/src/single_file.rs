use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use gane_codegen::LlvmBackend;
use gane_diagnostics::{DiagnosticMode, Diagnostics};
use gane_interpreter::{interpret, interpret_with_stack_details};
use gane_ir::{CompilerError, compile};
use gane_parser::parser::{Mode, parse_file};
use gane_parser::token::FileSet;
use gane_sema::{FileId, PackageInput, analyze_package};

pub(crate) fn run(input: PathBuf, out_dir: PathBuf, disable_color: bool, trace: bool) -> ExitCode {
    colored::control::set_override(!disable_color);
    // --- src -> AST -------------------------------------------------------
    let src = match fs::read(&input) {
        Ok(src) => src,
        Err(e) => {
            eprintln!("gane-driver: cannot read `{}`: {e}", input.display());
            return ExitCode::from(2);
        }
    };
    let filename = input.to_string_lossy().into_owned();
    let diagnostic_mode = if disable_color {
        DiagnosticMode::PLAIN
    } else {
        DiagnosticMode::COLOR
    };
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
        let diagnostics = Diagnostics::from(errors);
        let mut text = String::new();
        for diagnostic in diagnostics.iter() {
            text.push_str(
                &diagnostic
                    .display(DiagnosticMode::PLAIN)
                    .expect("parser diagnostics have resolved positions")
                    .to_string(),
            );
            text.push('\n');
        }
        let err_path = out_dir.join(format!("{stem}.err.txt"));
        if let Err(e) = fs::write(&err_path, &text) {
            eprintln!("gane-driver: cannot write `{}`: {e}", err_path.display());
            return ExitCode::from(2);
        }
        for diagnostic in diagnostics.iter() {
            eprintln!(
                "gane-driver: {}",
                diagnostic
                    .display(diagnostic_mode)
                    .expect("parser diagnostics have resolved positions")
            );
        }
        eprintln!(
            "gane-driver: source has {} parse error(s), see `{}`",
            errors.len(),
            err_path.display()
        );
        return ExitCode::FAILURE;
    }

    // Only a syntax-clean AST reaches sema.
    let package = PackageInput::single("main", FileId::from_raw(1), &ast);
    let mut analysis = analyze_package(package.clone());
    if let Err(error) = analysis.resolve_diagnostics(&package, &fset) {
        eprintln!("gane-driver: cannot resolve semantic diagnostic positions: {error}");
        return ExitCode::FAILURE;
    }
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
            text.push_str(
                &diagnostic
                    .display(DiagnosticMode::PLAIN)
                    .expect("resolved semantic diagnostics")
                    .to_string(),
            );
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
            eprint!(
                "gane-driver: {}",
                diagnostic
                    .display(diagnostic_mode)
                    .expect("resolved semantic diagnostics")
            );
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

    let ir = match compile(&package, &analysis, backend.target_spec().clone()) {
        Ok(result) => result,
        Err(error) => match error {
            CompilerError::Lower(error) => {
                eprintln!("gane-driver: IR lowering failed: {error}");
                return ExitCode::FAILURE;
            }
            CompilerError::Verifier(diagnostics) => {
                for diagnostic in diagnostics {
                    eprintln!("gane-driver: IR verification failed: {diagnostic}");
                }
                return ExitCode::FAILURE;
            }
        },
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

    if trace {
        match interpret_with_stack_details(&ir) {
            Ok(()) => {
                println!("gane-driver: interpreter completed successfully");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("gane-driver: interpreter failed: {error}");
                ExitCode::FAILURE
            }
        }
    } else {
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
}
