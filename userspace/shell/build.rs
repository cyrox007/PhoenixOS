use std::path::PathBuf;

fn main() {
    let script = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo должен передать каталог пакета"),
    )
    .join("linker.ld");

    println!("cargo:rerun-if-changed={}", script.display());
    println!("cargo:rustc-link-arg-bin=phxsh=-T{}", script.display());
    println!("cargo:rustc-link-arg-bin=phxsh=--build-id=none");
    println!("cargo:rustc-link-arg-bin=phxsh=-no-pie");
}
