fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        // server codegen is only used by examples/greeter_server.rs (local testing)
        .build_server(true)
        .compile_protos(&["proto/greeter.proto"], &["proto"])?;
    Ok(())
}
