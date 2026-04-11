fn main() -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: build scripts run single-threaded before any user code
    unsafe {
        std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path().unwrap());
    }
    tonic_prost_build::compile_protos("../../proto/supabased.proto")?;
    Ok(())
}
