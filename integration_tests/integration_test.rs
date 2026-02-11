use bip300301_enforcer_integration_tests::{
    setup::{Mode, Network},
    util::{AsyncTrial, TestFailureCollector, TestFileRegistry},
};
use futures::{FutureExt, future::BoxFuture};

use crate::{
    confirmations_block_inclusion::confirmations_block_inclusion_trial,
    ibd::ibd_trial,
    l1_rpc_dependency::l1_rpc_dependency_trial,
    l1_txid_uniqueness::l1_txid_uniqueness_trial,
    l1_verification_rpc_only::l1_verification_rpc_only_trial,
    multi_node_verification::multi_node_verification_trial,
    setup::{Init, PostSetup},
    swap_creation::{
        swap_creation_fixed_trial, swap_creation_open_fill_trial,
        swap_creation_open_trial,
    },
    unknown_withdrawal::unknown_withdrawal_trial,
    util::BinPaths,
};

#[allow(dead_code)]
fn deposit_withdraw_roundtrip(
    bin_paths: BinPaths,
    file_registry: TestFileRegistry,
    failure_collector: TestFailureCollector,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new(
        "deposit_withdraw_roundtrip",
        async move {
            let (res_tx, _) = futures::channel::mpsc::unbounded();
            let post_setup = bip300301_enforcer_integration_tests::setup::setup(
                &bin_paths.others,
                Network::Regtest,
                Mode::Mempool,
                res_tx
            ).await?;
            bip300301_enforcer_integration_tests::integration_test::deposit_withdraw_roundtrip::<PostSetup>(
                    post_setup,
                    Init {
                        coinshift_app: bin_paths.coinshift_app,
                        data_dir_suffix: None,
                    },
                ).await
        }.boxed(),
        file_registry,
        failure_collector,
    )
}

pub fn tests(
    bin_paths: BinPaths,
    file_registry: TestFileRegistry,
    failure_collector: TestFailureCollector,
) -> Vec<AsyncTrial<BoxFuture<'static, anyhow::Result<()>>>> {
    vec![
        deposit_withdraw_roundtrip(
            bin_paths.clone(),
            file_registry.clone(),
            failure_collector.clone(),
        ),
        ibd_trial(
            bin_paths.clone(),
            file_registry.clone(),
            failure_collector.clone(),
        ),
        swap_creation_fixed_trial(
            bin_paths.clone(),
            file_registry.clone(),
            failure_collector.clone(),
        ),
        swap_creation_open_trial(
            bin_paths.clone(),
            file_registry.clone(),
            failure_collector.clone(),
        ),
        swap_creation_open_fill_trial(
            bin_paths.clone(),
            file_registry.clone(),
            failure_collector.clone(),
        ),
        l1_txid_uniqueness_trial(
            bin_paths.clone(),
            file_registry.clone(),
            failure_collector.clone(),
        ),
        confirmations_block_inclusion_trial(
            bin_paths.clone(),
            file_registry.clone(),
            failure_collector.clone(),
        ),
        l1_verification_rpc_only_trial(
            bin_paths.clone(),
            file_registry.clone(),
            failure_collector.clone(),
        ),
        l1_rpc_dependency_trial(
            bin_paths.clone(),
            file_registry.clone(),
            failure_collector.clone(),
        ),
        multi_node_verification_trial(
            bin_paths.clone(),
            file_registry.clone(),
            failure_collector.clone(),
        ),
        unknown_withdrawal_trial(bin_paths, file_registry, failure_collector),
    ]
}
