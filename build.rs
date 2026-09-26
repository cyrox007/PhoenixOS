use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR"));

    let kernel = PathBuf::from(
        std::env::var_os("CARGO_BIN_FILE_KERNEL_kernel")
            .expect("artifact dependency must provide the PhoenixOS kernel"),
    );

    let uefi_path = out_dir.join("phoenixos-uefi.img");

    bootloader::UefiBoot::new(&kernel)
        .create_disk_image(&uefi_path)
        .expect("failed to create PhoenixOS UEFI disk image");

    println!(
        "cargo:rustc-env=PHOENIXOS_UEFI_IMAGE={}",
        uefi_path.display()
    );
}
