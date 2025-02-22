// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use crate::{transaction_generator::TransactionGenerator, Pipeline};
use aptos_config::{
    config::{RocksdbConfig, StoragePrunerConfig},
    utils::get_genesis_txn,
};
use aptos_jellyfish_merkle::metrics::{
    APTOS_JELLYFISH_INTERNAL_ENCODED_BYTES, APTOS_JELLYFISH_LEAF_ENCODED_BYTES,
    APTOS_JELLYFISH_STORAGE_READS,
};
use aptos_vm::AptosVM;
use aptosdb::{metrics::ROCKSDB_PROPERTIES, schema::JELLYFISH_MERKLE_NODE_CF_NAME, AptosDB};
use executor::{
    block_executor::BlockExecutor,
    db_bootstrapper::{generate_waypoint, maybe_bootstrap},
};
use std::{fs, path::Path};
use storage_interface::DbReaderWriter;

pub fn run(
    num_accounts: usize,
    init_account_balance: u64,
    block_size: usize,
    db_dir: impl AsRef<Path>,
    storage_pruner_config: StoragePrunerConfig,
    verify_sequence_numbers: bool,
) {
    println!("Initializing...");

    if db_dir.as_ref().exists() {
        panic!("data-dir exists already.");
    }
    // create if not exists
    fs::create_dir_all(db_dir.as_ref()).unwrap();

    let (config, genesis_key) = aptos_genesis::test_utils::test_config();
    // Create executor.
    let (db, db_rw) = DbReaderWriter::wrap(
        AptosDB::open(
            &db_dir,
            false,                 /* readonly */
            storage_pruner_config, /* pruner */
            RocksdbConfig::default(),
        )
        .expect("DB should open."),
    );

    // Bootstrap db with genesis
    let waypoint = generate_waypoint::<AptosVM>(&db_rw, get_genesis_txn(&config).unwrap()).unwrap();
    maybe_bootstrap::<AptosVM>(&db_rw, get_genesis_txn(&config).unwrap(), waypoint).unwrap();

    let executor = BlockExecutor::new(db_rw.clone());
    let (pipeline, block_sender) = Pipeline::new(db_rw, executor, 0);
    let mut generator =
        TransactionGenerator::new_with_sender(genesis_key, num_accounts, block_sender);
    generator.run_mint(init_account_balance, block_size);
    generator.drop_sender();
    pipeline.join();

    if verify_sequence_numbers {
        println!("Verifying sequence numbers...");
        // Do a sanity check on the sequence number to make sure all transactions are committed.
        generator.verify_sequence_numbers(db.clone());
    }

    let final_version = generator.version();
    // Write metadata
    generator.write_meta(&db_dir);

    db.update_rocksdb_properties().unwrap();
    let db_size = ROCKSDB_PROPERTIES
        .with_label_values(&[
            JELLYFISH_MERKLE_NODE_CF_NAME,
            "aptos_rocksdb_live_sst_files_size_bytes",
        ])
        .get();
    let data_size = ROCKSDB_PROPERTIES
        .with_label_values(&[
            JELLYFISH_MERKLE_NODE_CF_NAME,
            "aptos_rocksdb_total-sst-files-size",
        ])
        .get();
    let reads = APTOS_JELLYFISH_STORAGE_READS.get();
    let leaf_bytes = APTOS_JELLYFISH_LEAF_ENCODED_BYTES.get();
    let internal_bytes = APTOS_JELLYFISH_INTERNAL_ENCODED_BYTES.get();
    println!("=============FINISHED DB CREATION =============");
    println!(
        "created a AptosDB til version {} with {} accounts.",
        final_version, num_accounts,
    );
    println!("DB dir: {}", db_dir.as_ref().display());
    println!("Jellyfish Merkle physical size: {}", db_size);
    println!("Jellyfish Merkle logical size: {}", data_size);
    println!("Total reads from storage: {}", reads);
    println!(
        "Total written internal nodes value size: {} bytes",
        internal_bytes
    );
    println!("Total written leaf nodes value size: {} bytes", leaf_bytes);
}
