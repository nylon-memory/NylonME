fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 使用 vendored protoc，不要求系统安装 protobuf 编译器
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&["nylon/v1/memory.proto"], &["../../../proto"])?;
    Ok(())
}
