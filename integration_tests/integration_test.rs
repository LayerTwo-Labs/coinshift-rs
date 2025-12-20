use bip300301_enforcer_integration_tests::{
    setup::{Mode, Network},
    util::AsyncTrial,
};
use futures::{FutureExt, future::BoxFuture};

use crate::{
    setup::{Init, PostSetup},
    swap_creation::{swap_creation_fixed_trial, swap_creation_open_trial},
    util::BinPaths,
};

#[allow(dead_code)]
fn deposit_withdraw_roundtrip(
    bin_paths: BinPaths,
) -> AsyncTrial<BoxFuture<'static, anyhow::Result<()>>> {
    AsyncTrial::new("deposit_withdraw_roundtrip", async move {
        bip300301_enforcer_integration_tests::integration_test::deposit_withdraw_roundtrip::<PostSetup>(
            bin_paths.others, Network::Regtest, Mode::Mempool,
            Init {
                coinshift_app: bin_paths.coinshift_app,
                data_dir_suffix: None,
            },
        ).await
    }.boxed())
}

pub fn tests(
    bin_paths: BinPaths,
) -> Vec<AsyncTrial<BoxFuture<'static, anyhow::Result<()>>>> {
    vec![
        // deposit_withdraw_roundtrip(bin_paths.clone()),
        // ibd_trial(bin_paths.clone()),
        swap_creation_fixed_trial(bin_paths.clone()),
        swap_creation_open_trial(bin_paths.clone()),
        // unknown_withdrawal_trial(bin_paths),
    ]
}

