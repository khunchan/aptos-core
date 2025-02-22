// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use crate::{
    common::{check_network, handle_request, strip_hex_prefix, with_context},
    error::{ApiError, ApiResult},
    types::{Block, BlockRequest, BlockResponse},
    RosettaContext,
};
use aptos_crypto::HashValue;
use aptos_logger::{debug, trace};
use aptos_rest_client::Transaction;
use std::str::FromStr;
use warp::Filter;

pub fn routes(
    server_context: RosettaContext,
) -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    warp::post().and(
        warp::path!("block")
            .and(warp::body::json())
            .and(with_context(server_context))
            .and_then(handle_request(block)),
    )
}

/// Retrieves a block (in this case a single transaction) given it's identifier.
///
/// Our implementation allows for by `index`, which is the ledger `version` or by
/// transaction `hash`.
///
/// [API Spec](https://www.rosetta-api.org/docs/BlockApi.html#block)
async fn block(request: BlockRequest, server_context: RosettaContext) -> ApiResult<BlockResponse> {
    debug!("/block");
    trace!(
        request = ?request,
        server_context = ?server_context,
        "block",
    );

    check_network(request.network_identifier, &server_context)?;

    let rest_client = server_context.rest_client()?;

    // Retrieve by block or by hash, both or neither is not allowed
    let (parent_transaction, transaction): (Transaction, _) = match (
        &request.block_identifier.index,
        &request.block_identifier.hash,
    ) {
        (Some(version), None) => {
            // For the genesis block, we populate parent_block_identifier with the
            // same genesis block. Refer to
            // https://www.rosetta-api.org/docs/common_mistakes.html#malformed-genesis-block
            if *version == 0 {
                let response = rest_client.get_transaction_by_version(*version).await?;
                let txn = response.into_inner();
                (txn.clone(), txn)
            } else {
                let response = rest_client
                    .get_transactions(Some(*version - 1), Some(2))
                    .await?;
                let txns = response.into_inner();
                if txns.len() != 2 {
                    return Err(ApiError::AptosError(
                        "Failed to get transaction and parent transaction".to_string(),
                    ));
                }
                (
                    txns.first().cloned().unwrap(),
                    txns.last().cloned().unwrap(),
                )
            }
        }
        (None, Some(hash)) => {
            // Allow 0x in front of hash
            let hash = HashValue::from_str(strip_hex_prefix(hash))
                .map_err(|err| ApiError::AptosError(err.to_string()))?;
            let response = rest_client.get_transaction(hash).await?;
            let txn = response.into_inner();
            let version = txn.version().unwrap();

            // If this is genesis, set parent to genesis txn
            if version == 0 {
                (txn.clone(), txn)
            } else {
                let parent_response = rest_client.get_transaction_by_version(version - 1).await?;
                (parent_response.into_inner(), txn)
            }
        }
        (None, None) => {
            // Get current version
            let response = rest_client.get_transactions(None, Some(2)).await?;
            let txns = response.into_inner();
            if txns.len() != 2 {
                return Err(ApiError::AptosError(
                    "Failed to get transaction and parent transaction".to_string(),
                ));
            }
            (
                txns.first().cloned().unwrap(),
                txns.last().cloned().unwrap(),
            )
        }
        (_, _) => return Err(ApiError::BadBlockRequest),
    };

    // Build up the transaction, which should contain the `operations` as the change set
    let transaction_info = transaction.transaction_info()?;
    let transactions = vec![transaction_info.into()];

    // note: timestamps are in microseconds, so we convert to milliseconds
    let timestamp = transaction.timestamp() / 1000;

    let block = Block {
        block_identifier: transaction_info.into(),
        parent_block_identifier: parent_transaction.transaction_info()?.into(),
        timestamp,
        transactions,
    };

    let response = BlockResponse {
        block: Some(block),
        other_transactions: None,
    };

    Ok(response)
}
