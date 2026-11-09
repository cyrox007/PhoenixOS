use std::path::PathBuf;

fn main() {
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Нет OUT_DIR"));
    let kernel = PathBuf::from(
        std::env::var_os("CARGO_BIN_FILE_PHOENIX_DIAGNOSTIC_KERNEL_phoenix-diagnostic-kernel")
            .expect("Не найдено отдельное диагностическое ядро"),
    );
    let image = output.join("phoenixos-diagnostic-uefi.img");
    bootloader::UefiBoot::new(&kernel)
        .create_disk_image(&image)
        .expect("Не удалось создать диагностический образ UEFI");
    println!(
        "cargo:rustc-env=PHOENIXOS_DIAGNOSTIC_IMAGE={}",
        image.display()
    );
}
