use std::process::Command;

#[test]
fn help_describes_the_coordinator() {
    let output = Command::new(env!("CARGO_BIN_EXE_chainsaw"))
        .arg("--help")
        .output()
        .expect("chainsaw binary should run");

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Coordinate a Chainsaw"));
    assert!(output.stderr.is_empty());
}
