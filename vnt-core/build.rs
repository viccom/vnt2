fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = prost_build::Config::new();

    match protoc_bin_vendored::protoc_bin_path() {
        Ok(protoc_path) => {
            config.protoc_executable(protoc_path);
        }
        Err(error) => {
            println!(
                "cargo:warning=vendored protoc unavailable ({error:?}); falling back to protoc from PATH"
            );
        }
    }

    config.protoc_arg("--experimental_allow_proto3_optional");
    // 生成枚举变体共享 GenericCtrl 前缀是协议语义（状态码族），非命名缺陷
    config.type_attribute(
        "protocol.generic_ctrl.GenericCtrlStatus",
        "#[allow(clippy::enum_variant_names)]",
    );
    // Keep selective-broadcast payloads reference counted. This avoids building an
    // intermediate Vec for every broadcast and lets decoding from `Bytes` retain a
    // slice of the received frame instead of copying the embedded packet.
    config.bytes([".protocol.control_message.SelectiveBroadcast.data"]);

    config.compile_protos(
        &[
            "proto/control_message.proto",
            "proto/rpc.proto",
            "proto/client.proto",
            "proto/fec.proto",
            "proto/generic_ctrl.proto",
        ],
        &["proto"],
    )?;
    Ok(())
}
