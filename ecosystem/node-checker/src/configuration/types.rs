// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use crate::{metric_evaluator::StateSyncMetricsEvaluatorArgs, runner::BlockingRunnerArgs};
use anyhow::Result;
use clap::Parser;
use once_cell::sync::Lazy;
use poem_openapi::{types::Example, Object as PoemObject};
use serde::{Deserialize, Serialize};
use url::Url;

pub const DEFAULT_METRICS_PORT: u16 = 9101;
pub const DEFAULT_API_PORT: u16 = 8080;
pub const DEFAULT_NOISE_PORT: u16 = 6180;

pub static DEFAULT_METRICS_PORT_STR: Lazy<String> =
    Lazy::new(|| format!("{}", DEFAULT_METRICS_PORT));
pub static DEFAULT_API_PORT_STR: Lazy<String> = Lazy::new(|| format!("{}", DEFAULT_API_PORT));
pub static DEFAULT_NOISE_PORT_STR: Lazy<String> = Lazy::new(|| format!("{}", DEFAULT_NOISE_PORT));

// To briefly explain why many of these structs derive 3 different classes of traits:
// - Parser (clap): To allow users to generate configs easily using nhc configuration create
// - Serialize / Deserialize (serde): So we can read / write configs from / to disk
// - PoemObject: So we can return the configuration over the API

#[derive(Clone, Debug, Deserialize, Parser, PoemObject, Serialize)]
#[clap(author, version, about, long_about = None)]
pub struct NodeConfiguration {
    #[clap(flatten)]
    pub node_address: NodeAddress,

    /// This is the name we expect clients to send over the wire to select
    /// which configuration they want to use. e.g. devnet_fullnode
    #[clap(long)]
    pub configuration_name: String,

    /// This is the name we will show for this configuration to users.
    /// For example, if someone opens the NHC frontend, they will see this name
    /// in a dropdown list of configurations they can test their node against.
    /// e.g. "Devnet FullNode", "Testnet Validator Node", etc.
    #[clap(long)]
    pub configuration_name_pretty: String,

    /// The chain ID we expect to find when we speak to the baseline node
    /// at `node_address`. Regardless of whether this is set, at startup we
    /// will contact the node to see what its chain ID is. If `chain_id` is
    /// set here and doesn't match the chain ID returned by the node, we
    /// will exit, signalling a configuration error.
    #[clap(long)]
    chain_id: Option<u16>,

    /// This works the same as `chain_id` above, but for role type. Example
    /// values: "full_node", "validator", etc.
    #[clap(long)]
    role_type: Option<String>,

    /// The evaluators to use, e.g. state_sync_version, consensus_proposals, etc.
    #[clap(long, required = true, min_values = 1, use_value_delimiter = true)]
    pub evaluators: Vec<String>,

    #[clap(flatten)]
    pub evaluator_args: EvaluatorArgs,

    #[clap(flatten)]
    pub runner_args: RunnerArgs,
}

impl NodeConfiguration {
    /// Only call this after fetch_additional_configuration has been called.
    #[allow(dead_code)]
    pub fn get_chain_id(&self) -> u16 {
        self.chain_id
            .expect("get_chain_id called before fetch_additional_configuration")
    }

    /// Only call this after fetch_additional_configuration has been called.
    #[allow(dead_code)]
    pub fn get_role_type(&self) -> &str {
        self.role_type
            .as_ref()
            .expect("get_role_type called before fetch_additional_configuration")
    }

    /// In this function we fetch the chain ID and role type from the node.
    /// If chain_id and role_type are already set, we validate that the values
    /// match up. If they're not set, we set them using the values we find.
    pub async fn fetch_additional_configuration(&mut self) -> Result<()> {
        // TODO: Dummy code prior to https://github.com/aptos-labs/aptos-core/pull/1466.
        self.chain_id = Some(16);
        self.role_type = Some("full_node".to_string());
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Parser, PoemObject, Serialize)]
pub struct EvaluatorArgs {
    #[clap(flatten)]
    pub state_sync_evaluator_args: StateSyncMetricsEvaluatorArgs,
}

#[derive(Clone, Debug, Deserialize, Parser, PoemObject, Serialize)]
pub struct RunnerArgs {
    #[clap(flatten)]
    pub blocking_runner_args: BlockingRunnerArgs,
}

#[derive(Clone, Debug, Deserialize, Parser, PoemObject, Serialize)]
#[oai(example)]
pub struct NodeAddress {
    /// Target URL. This should include a scheme (e.g. http://). If there is
    /// no scheme, we will prepend http://.
    #[clap(long)]
    pub url: Url,

    /// Metrics port.
    #[clap(long, default_value = &DEFAULT_METRICS_PORT_STR)]
    #[oai(default = "Self::default_metrics_port")]
    #[serde(default = "NodeAddress::default_metrics_port")]
    pub metrics_port: u16,

    /// API port.
    #[clap(long, default_value = &DEFAULT_API_PORT_STR)]
    #[oai(default = "Self::default_api_port")]
    #[serde(default = "NodeAddress::default_api_port")]
    pub api_port: u16,

    /// Validator communication port.
    #[clap(long, default_value = &DEFAULT_NOISE_PORT_STR)]
    #[oai(default = "Self::default_noise_port")]
    #[serde(default = "NodeAddress::default_noise_port")]
    pub noise_port: u16,
}

impl NodeAddress {
    pub fn default_metrics_port() -> u16 {
        DEFAULT_METRICS_PORT
    }

    pub fn default_api_port() -> u16 {
        DEFAULT_API_PORT
    }

    pub fn default_noise_port() -> u16 {
        DEFAULT_NOISE_PORT
    }
}

impl Example for NodeAddress {
    fn example() -> Self {
        Self {
            url: Url::parse("http://mynode.mysite.com").unwrap(),
            metrics_port: Self::default_metrics_port(),
            api_port: Self::default_api_port(),
            noise_port: Self::default_noise_port(),
        }
    }
}
