use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    println!("cargo:rerun-if-changed=user_test.bin");

    let elf_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("user_test.bin");
    if elf_path.exists() {
        let data = fs::read(&elf_path).expect("Failed to read user_test.bin");
        let bytes: Vec<String> = data.iter().map(|b| format!("0x{:02X}", b)).collect();

        let code = format!(
            "[{}]",
            bytes.join(", ")
        );

        let dest_path = Path::new(&out_dir).join("user_program_data.rs");
        fs::write(&dest_path, code).expect("Failed to write user_program_data.rs");
    }
}
