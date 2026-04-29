use std::{
    env, fs,
    path::{Path, PathBuf},
};

use prost::Message;

fn compile_protos_with_config<F>(
    file_descriptor_path: impl AsRef<Path>,
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    config_fn: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnOnce(
        &mut prost_build::Config,
    ) -> Result<(), Box<dyn std::error::Error>>,
{
    let mut config = prost_build::Config::new();
    config.enable_type_names();
    let () = config_fn(&mut config)?;
    tonic_build::configure()
        .skip_protoc_run()
        .file_descriptor_set_path(file_descriptor_path)
        .build_transport(false)
        .compile_protos_with_config(config, protos, includes)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let workspace_root = manifest_dir
        .parent()
        .expect("lib crate must live under coinshift-rs workspace root");
    let proto_include = [
        workspace_root.join("proto/proto"),
        workspace_root.join("proto"),
        workspace_root.join("../proto/proto"),
        workspace_root.join("../proto"),
    ]
    .into_iter()
    .find(|p| p.join("cusf/common/v1/common.proto").is_file())
    .expect(
        "proto/ (cusf_sidechain_proto git submodule) not found; run \
         `git submodule update --init --recursive`",
    );
    let common_proto = proto_include.join("cusf/common/v1/common.proto");
    let validator_proto = proto_include.join("cusf/mainchain/v1/validator.proto");
    let wallet_proto = proto_include.join("cusf/mainchain/v1/wallet.proto");
    let all_protos: Vec<&Path> = vec![
        common_proto.as_path(),
        validator_proto.as_path(),
        wallet_proto.as_path(),
    ];
    let includes: [&Path; 1] = [proto_include.as_path()];
    let file_descriptors = protox::compile(&all_protos, &includes)?;
    let file_descriptor_path = PathBuf::from(
        env::var("OUT_DIR").expect("OUT_DIR environment variable not set"),
    )
    .join("file_descriptor_set.bin");
    fs::write(&file_descriptor_path, file_descriptors.encode_to_vec())?;

    let () = compile_protos_with_config(
        &file_descriptor_path,
        &[common_proto.as_path()],
        &includes,
        |_| Ok(()),
    )?;
    let () = compile_protos_with_config(
        &file_descriptor_path,
        &[validator_proto.as_path(), wallet_proto.as_path()],
        &includes,
        |config| {
            config
                .extern_path(".cusf.common.v1", "crate::types::proto::common");
            Ok(())
        },
    )?;
    Ok(())
}
