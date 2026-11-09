use std::path::PathBuf;

fn main() {
    let output = PathBuf::from(std::env::var_os("OUT_DIR").expect("Нет OUT_DIR"));
    let kernel = PathBuf::from(
        std::env::var_os("CARGO_BIN_FILE_PHOENIX_DMA_TEST_KERNEL_phoenix-dma-test-kernel")
            .expect("Не найдено отдельное ядро испытания DMA"),
    );
    let image = output.join("phoenixos-dma-qemu-only-uefi.img");
    bootloader::UefiBoot::new(&kernel)
        .create_disk_image(&image)
        .expect("Не удалось создать испытательный образ DMA");
    println!(
        "cargo:rustc-env=PHOENIXOS_DMA_QEMU_IMAGE={}",
        image.display()
    );
}
