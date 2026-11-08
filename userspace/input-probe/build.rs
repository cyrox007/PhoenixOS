use std::path::PathBuf;

fn main() {
    let script =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("отсутствует каталог пакета"))
            .join("linker.ld");

    println!("cargo:rerun-if-changed={}", script.display());
    println!(
        "cargo:rustc-link-arg-bin=input-probe=-T{}",
        script.display()
    );
    println!("cargo:rustc-link-arg-bin=input-probe=--build-id=none");
    println!("cargo:rustc-link-arg-bin=input-probe=-no-pie");
}
