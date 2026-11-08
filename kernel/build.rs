use std::path::PathBuf;

fn main() {
    let binary = PathBuf::from(
        std::env::var_os("CARGO_BIN_FILE_USERSPACE_SMOKE_smoke")
            .expect("Cargo должен собрать проверочную пользовательскую программу"),
    );

    println!(
        "cargo:rustc-env=PHOENIX_USERSPACE_SMOKE={}",
        binary.display()
    );

    let input_probe = PathBuf::from(
        std::env::var_os("CARGO_BIN_FILE_USERSPACE_INPUT_PROBE_input-probe")
            .expect("Cargo должен собрать проверочную программу ввода"),
    );
    println!(
        "cargo:rustc-env=PHOENIX_USERSPACE_INPUT_PROBE={}",
        input_probe.display()
    );

    let shell = PathBuf::from(
        std::env::var_os("CARGO_BIN_FILE_USERSPACE_SHELL_phxsh")
            .expect("Cargo должен собрать пользовательскую оболочку"),
    );
    println!("cargo:rustc-env=PHOENIX_USERSPACE_SHELL={}", shell.display());
}
