use std::{
    fs,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn fixture_dir(name: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("gane-driver-{name}-{}-{nonce}", std::process::id()))
}

struct FixtureDir(std::path::PathBuf);

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn semantic_errors_are_dumped_and_rendered_after_a_successful_parse() {
    let root = fixture_dir("sema");
    let _fixture = FixtureDir(root.clone());
    let out = root.join("out");
    fs::create_dir_all(&root).expect("create fixture directory");
    let input = root.join("main.go");
    fs::write(&input, b"package main\nfunc main() { missing }\n").expect("write fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_gane_driver"))
        .arg(&input)
        .arg(&out)
        .output()
        .expect("run driver");

    assert!(!output.status.success());
    assert!(out.join("main.go.ast.txt").is_file());
    let semantic_dump =
        fs::read_to_string(out.join("main.go.sema.txt")).expect("semantic result dump");
    assert!(semantic_dump.contains("AnalysisResult"));
    let diagnostics =
        fs::read_to_string(out.join("main.go.sema.err.txt")).expect("semantic diagnostic output");
    assert!(diagnostics.contains("error[E2201]: undefined name `missing`"));
    assert!(diagnostics.contains(" --> "));
}

#[test]
fn valid_program_is_lowered_verified_and_interpreted() {
    let root = fixture_dir("pipeline");
    let _fixture = FixtureDir(root.clone());
    let out = root.join("out");
    fs::create_dir_all(&root).expect("create fixture directory");
    let input = root.join("main.go");
    fs::write(
        &input,
        b"package main\nfunc increment(x int) int { return x + 1 }\nfunc main() { var x int; x = increment(2) }\n",
    )
    .expect("write fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_gane_driver"))
        .arg(&input)
        .arg(&out)
        .output()
        .expect("run driver");

    assert!(output.status.success(), "{output:?}");
    let ir = fs::read_to_string(out.join("main.go.ir.txt")).expect("verified IR dump");
    assert!(ir.contains("func @"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("interpreter completed successfully"));
}
