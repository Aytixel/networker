fn main() -> Result<(), Box<dyn std::error::Error>> {
    // clients
    tonic_build::configure()
        .build_server(false)
        .build_client(true)
        .compile_protos(&["../../proto/kernel.proto"], &["../../proto"])?;
    tonic_build::configure()
        .build_server(false)
        .build_client(true)
        .compile_protos(&["../../proto/rib.proto"], &["../../proto"])?;

    // server
    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(&["../../proto/bgp.proto"], &["../../proto"])?;
    Ok(())
}
