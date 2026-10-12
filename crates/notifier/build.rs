use std::{env, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = tonic_prost_build::Config::new();
    if env::var_os("PROTOC").is_none() {
        config.protoc_executable(protoc_bin_vendored::protoc_bin_path()?);
    }
    let well_known = protoc_bin_vendored::include_path()?;
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);

    tonic_prost_build::configure()
        .file_descriptor_set_path(out_dir.join("notifications_descriptor.bin"))
        .compile_with_config(
            config,
            &[PathBuf::from("proto/notifications.proto")],
            &[PathBuf::from("proto"), well_known],
        )?;
    println!("cargo:rerun-if-changed=proto");
    Ok(())
}
