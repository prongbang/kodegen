fn main() -> Result<(), Box<dyn std::error::Error>> {
    // compile every proto in proto/ so adding a file needs no build.rs change;
    // server codegen is used by api/*_grpc.rs adapters and examples/greeter_server.rs
    let protos: Vec<std::path::PathBuf> = std::fs::read_dir("proto")?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "proto"))
        .collect();
    println!("cargo:rerun-if-changed=proto");
    tonic_prost_build::configure()
        .build_server(true)
        .compile_protos(&protos, &["proto".into()])?;
    Ok(())
}
