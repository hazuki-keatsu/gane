use clap::{Args, Parser, Subcommand};
use std::{path::PathBuf, process::ExitCode};

mod single_file;

const GLOBAL_USAGE: &str = "\
Development driver for Gane.

There will be a lot of command for tests.
";

#[derive(Debug, Parser)]
#[command(name = "gane-driver", version, about = GLOBAL_USAGE, arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Compile one .go source file and write AST, sema, IR, and LLVM IR dumps.
    Single(SingleArgs),
}

#[derive(Args, Debug)]
struct SingleArgs {
    /// Go source file to compile.
    #[arg(value_name = "FILE")]
    input: PathBuf,

    /// Directory for generated dumps.
    #[arg(short, long, default_value = "out", value_name = "DIR")]
    out_dir: PathBuf,

    /// Render diagnostics with ANSI colors on stderr.
    #[arg(long)]
    disable_color: bool,

    /// Display trace information if Trapped or Unreachable.
    #[arg(long)]
    verbose: bool,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Single(args) => {
            single_file::run(args.input, args.out_dir, args.disable_color, args.verbose)
        }
    }
}
