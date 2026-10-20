use std::path::PathBuf;

fn main() {
    let binary = PathBuf::from(
        std::env::var_os("CARGO_BIN_FILE_USERSPACE_SMOKE_smoke")
            .expect("Cargo должен собрать проверочную пользовательскую программу"),
    );

    println!("cargo:rustc-env=PHOENIX_USERSPACE_SMOKE={}", binary.display());
}
