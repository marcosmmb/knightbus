fn main() -> Result<(), wdk_build::ConfigError> {
    assert_eq!(
        std::env::var("TARGET").expect("Cargo supplies TARGET"),
        "x86_64-pc-windows-msvc",
        "Only the measured Windows x64 ABI is under investigation"
    );
    let directory = std::path::PathBuf::from(
        std::env::var_os("KNIGHTBUS_UDE_STUB_DIRECTORY")
            .expect("The build helper must select the measured x64 UDE 1.1 library directory"),
    );
    assert!(directory.join("udecxstub.lib").is_file());
    wdk_build::configure_wdk_binary_build()?;
    println!("cargo:rerun-if-env-changed=KNIGHTBUS_UDE_STUB_DIRECTORY");
    println!("cargo:rustc-link-search=native={}", directory.display());
    println!("cargo:rustc-link-lib=static=udecxstub");
    Ok(())
}
