// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use crate::{
    ChainInfo, FullNode, HealthCheckError, LocalNode, LocalVersion, Node, NodeExt, Swarm, SwarmExt,
    Validator, Version,
};
use anyhow::{anyhow, bail, Result};
use aptos_config::{config::NodeConfig, keys::ConfigKey};
use aptos_genesis::builder::FullnodeNodeConfig;
use aptos_sdk::{
    crypto::ed25519::Ed25519PrivateKey,
    types::{
        chain_id::ChainId, transaction::Transaction, waypoint::Waypoint, AccountKey, LocalAccount,
        PeerId,
    },
};
use std::{
    collections::HashMap,
    fs, mem,
    num::NonZeroUsize,
    ops,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tempfile::TempDir;

#[derive(Debug)]
pub enum SwarmDirectory {
    Persistent(PathBuf),
    Temporary(TempDir),
}

impl SwarmDirectory {
    pub fn persist(&mut self) {
        match self {
            SwarmDirectory::Persistent(_) => {}
            SwarmDirectory::Temporary(_) => {
                let mut temp = SwarmDirectory::Persistent(PathBuf::new());
                mem::swap(self, &mut temp);
                let _ = mem::replace(self, temp.into_persistent());
            }
        }
    }

    pub fn into_persistent(self) -> Self {
        match self {
            SwarmDirectory::Temporary(tempdir) => SwarmDirectory::Persistent(tempdir.into_path()),
            SwarmDirectory::Persistent(dir) => SwarmDirectory::Persistent(dir),
        }
    }
}

impl ops::Deref for SwarmDirectory {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        match self {
            SwarmDirectory::Persistent(dir) => dir.deref(),
            SwarmDirectory::Temporary(dir) => dir.path(),
        }
    }
}

impl AsRef<Path> for SwarmDirectory {
    fn as_ref(&self) -> &Path {
        match self {
            SwarmDirectory::Persistent(dir) => dir.as_ref(),
            SwarmDirectory::Temporary(dir) => dir.as_ref(),
        }
    }
}

pub struct LocalSwarmBuilder {
    versions: Arc<HashMap<Version, LocalVersion>>,
    initial_version: Option<Version>,
    template: NodeConfig,
    number_of_validators: NonZeroUsize,
    dir: Option<PathBuf>,
    genesis_modules: Option<Vec<Vec<u8>>>,
    min_price_per_gas_unit: u64,
}

impl LocalSwarmBuilder {
    pub fn new(versions: Arc<HashMap<Version, LocalVersion>>) -> Self {
        Self {
            versions,
            initial_version: None,
            template: NodeConfig::default_for_validator(),
            number_of_validators: NonZeroUsize::new(1).unwrap(),
            dir: None,
            genesis_modules: None,
            min_price_per_gas_unit: 1,
        }
    }

    pub fn initial_version(mut self, initial_version: Version) -> Self {
        self.initial_version = Some(initial_version);
        self
    }

    pub fn template(mut self, template: NodeConfig) -> Self {
        self.template = template;
        self
    }

    pub fn number_of_validators(mut self, number_of_validators: NonZeroUsize) -> Self {
        self.number_of_validators = number_of_validators;
        self
    }

    pub fn dir<T: AsRef<Path>>(mut self, dir: T) -> Self {
        self.dir = Some(dir.as_ref().into());
        self
    }

    pub fn genesis_modules(mut self, genesis_modules: Vec<Vec<u8>>) -> Self {
        self.genesis_modules = Some(genesis_modules);
        self
    }

    pub fn min_price_per_gas_unit(mut self, min_price_per_gas_unit: u64) -> Self {
        self.min_price_per_gas_unit = min_price_per_gas_unit;
        self
    }

    pub fn build<R>(mut self, rng: R) -> Result<LocalSwarm>
    where
        R: ::rand::RngCore + ::rand::CryptoRng,
    {
        let dir = if let Some(dir) = self.dir {
            if dir.exists() {
                fs::remove_dir_all(&dir)?;
            }
            fs::create_dir_all(&dir)?;
            SwarmDirectory::Persistent(dir)
        } else {
            SwarmDirectory::Temporary(TempDir::new()?)
        };

        // Single node orders blocks too fast which would trigger backpressure and stall for 1 sec
        // which cause flakiness in tests.
        if self.number_of_validators.get() == 1 {
            // this delays empty block by (30-1) * 30ms
            self.template.consensus.quorum_store_poll_count = 30;
        }

        let (root_key, genesis, genesis_waypoint, validators) =
            aptos_genesis::builder::Builder::new(
                &dir,
                self.genesis_modules
                    .unwrap_or_else(|| cached_framework_packages::module_blobs().to_vec()),
            )?
            .with_num_validators(self.number_of_validators)
            .with_template(self.template)
            .with_min_price_per_gas_unit(self.min_price_per_gas_unit)
            .build(rng)?;

        // Get the initial version to start the nodes with, either the one provided or fallback to
        // using the the latest version
        let versions = self.versions;
        let initial_version = self.initial_version.unwrap_or_else(|| {
            versions
                .iter()
                .max_by(|v1, v2| v1.0.cmp(v2.0))
                .unwrap()
                .0
                .clone()
        });
        let version = versions.get(&initial_version).unwrap();

        let validators = validators
            .into_iter()
            .map(|v| {
                let node = LocalNode::new(version.to_owned(), v.name, v.dir)?;
                Ok((node.peer_id(), node))
            })
            .collect::<Result<HashMap<_, _>>>()?;
        let root_key = ConfigKey::new(root_key);
        let root_account = LocalAccount::new(
            aptos_sdk::types::account_config::aptos_root_address(),
            AccountKey::from_private_key(root_key.private_key()),
            0,
        );

        Ok(LocalSwarm {
            node_name_counter: validators.len() as u64,
            genesis,
            genesis_waypoint,
            versions,
            validators,
            fullnodes: HashMap::new(),
            dir,
            root_account,
            chain_id: ChainId::test(),
            root_key,
        })
    }
}

#[derive(Debug)]
pub struct LocalSwarm {
    node_name_counter: u64,
    genesis: Transaction,
    genesis_waypoint: Waypoint,
    versions: Arc<HashMap<Version, LocalVersion>>,
    validators: HashMap<PeerId, LocalNode>,
    fullnodes: HashMap<PeerId, LocalNode>,
    dir: SwarmDirectory,
    root_account: LocalAccount,
    chain_id: ChainId,
    root_key: ConfigKey<Ed25519PrivateKey>,
}

impl LocalSwarm {
    pub fn builder(versions: Arc<HashMap<Version, LocalVersion>>) -> LocalSwarmBuilder {
        LocalSwarmBuilder::new(versions)
    }

    pub async fn launch(&mut self) -> Result<()> {
        // Start all the validators
        for validator in self.validators.values_mut() {
            validator.start()?;
        }

        // Wait for all of them to startup
        let deadline = Instant::now() + Duration::from_secs(60);
        self.wait_for_startup().await?;
        self.wait_for_connectivity(deadline).await?;
        self.liveness_check(deadline).await?;

        println!("Swarm launched successfully.");
        Ok(())
    }

    async fn wait_for_startup(&mut self) -> Result<()> {
        let num_attempts = 10;
        let mut done = vec![false; self.validators.len()];
        for i in 0..num_attempts {
            println!("Wait for startup attempt: {} of {}", i, num_attempts);
            for (node, done) in self.validators.values_mut().zip(done.iter_mut()) {
                if *done {
                    continue;
                }
                match node.health_check().await {
                    Ok(()) => *done = true,

                    Err(HealthCheckError::Unknown(e)) => {
                        return Err(anyhow!(
                            "Node '{}' is not running! Error: {}",
                            node.name(),
                            e
                        ));
                    }
                    Err(HealthCheckError::NotRunning(error)) => {
                        return Err(anyhow!(
                            "Node '{}' is not running! Error: {:?}",
                            node.name(),
                            error
                        ));
                    }
                    Err(HealthCheckError::Failure(e)) => {
                        println!("health check failure: {}", e);
                        break;
                    }
                }
            }

            // Check if all the nodes have been successfully launched
            if done.iter().all(|status| *status) {
                return Ok(());
            }

            tokio::time::sleep(::std::time::Duration::from_millis(1000)).await;
        }

        Err(anyhow!("Launching Swarm timed out"))
    }

    pub async fn add_validator_fullnode(
        &mut self,
        version: &Version,
        template: NodeConfig,
        validator_peer_id: PeerId,
    ) -> Result<PeerId> {
        let validator = self
            .validators
            .get_mut(&validator_peer_id)
            .ok_or_else(|| anyhow!("no validator with peer_id: {}", validator_peer_id))?;

        if self.fullnodes.contains_key(&validator_peer_id) {
            bail!("VFN for validator {} already configured", validator_peer_id);
        }

        let mut validator_config = validator.config().clone();
        let name = self.node_name_counter.to_string();
        self.node_name_counter += 1;
        let fullnode_config = FullnodeNodeConfig::validator_fullnode(
            name,
            self.dir.as_ref(),
            template,
            &mut validator_config,
            &self.genesis_waypoint,
            &self.genesis,
        )?;

        // Since the validator's config has changed we need to save it
        validator_config.save(validator.config_path())?;
        *validator.config_mut() = validator_config;
        validator.restart().await?;

        let version = self.versions.get(version).unwrap();
        let mut fullnode = LocalNode::new(
            version.to_owned(),
            fullnode_config.name,
            fullnode_config.dir,
        )?;

        let peer_id = fullnode.peer_id();
        assert_eq!(peer_id, validator_peer_id);
        fullnode.start()?;

        self.fullnodes.insert(peer_id, fullnode);

        Ok(peer_id)
    }

    fn add_fullnode(&mut self, version: &Version, template: NodeConfig) -> Result<PeerId> {
        let name = self.node_name_counter.to_string();
        self.node_name_counter += 1;
        let fullnode_config = FullnodeNodeConfig::public_fullnode(
            name,
            self.dir.as_ref(),
            template,
            &self.genesis_waypoint,
            &self.genesis,
        )?;

        let version = self.versions.get(version).unwrap();
        let mut fullnode = LocalNode::new(
            version.to_owned(),
            fullnode_config.name,
            fullnode_config.dir,
        )?;

        let peer_id = fullnode.peer_id();
        fullnode.start()?;

        self.fullnodes.insert(peer_id, fullnode);

        Ok(peer_id)
    }

    pub fn root_key(&self) -> Ed25519PrivateKey {
        self.root_key.private_key()
    }

    pub fn chain_id(&self) -> ChainId {
        self.chain_id
    }

    pub fn validator(&self, peer_id: PeerId) -> Option<&LocalNode> {
        self.validators.get(&peer_id)
    }

    pub fn validator_mut(&mut self, peer_id: PeerId) -> Option<&mut LocalNode> {
        self.validators.get_mut(&peer_id)
    }

    pub fn validators(&self) -> impl Iterator<Item = &LocalNode> {
        self.validators.values()
    }

    pub fn validators_mut(&mut self) -> impl Iterator<Item = &mut LocalNode> {
        self.validators.values_mut()
    }

    pub fn fullnode(&self, peer_id: PeerId) -> Option<&LocalNode> {
        self.fullnodes.get(&peer_id)
    }

    pub fn fullnode_mut(&mut self, peer_id: PeerId) -> Option<&mut LocalNode> {
        self.fullnodes.get_mut(&peer_id)
    }

    pub fn dir(&self) -> &Path {
        self.dir.as_ref()
    }
}

impl Drop for LocalSwarm {
    fn drop(&mut self) {
        // If panicking, persist logs
        if std::thread::panicking() {
            eprintln!("Logs located at {}", self.logs_location());
        }
    }
}

#[async_trait::async_trait]
impl Swarm for LocalSwarm {
    async fn health_check(&mut self) -> Result<()> {
        Ok(())
    }

    fn validators<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn Validator> + 'a> {
        Box::new(self.validators.values().map(|v| v as &'a dyn Validator))
    }

    fn validators_mut<'a>(&'a mut self) -> Box<dyn Iterator<Item = &'a mut dyn Validator> + 'a> {
        Box::new(
            self.validators
                .values_mut()
                .map(|v| v as &'a mut dyn Validator),
        )
    }

    fn validator(&self, id: PeerId) -> Option<&dyn Validator> {
        self.validators.get(&id).map(|v| v as &dyn Validator)
    }

    fn validator_mut(&mut self, id: PeerId) -> Option<&mut dyn Validator> {
        self.validators
            .get_mut(&id)
            .map(|v| v as &mut dyn Validator)
    }

    fn upgrade_validator(&mut self, id: PeerId, version: &Version) -> Result<()> {
        let version = self
            .versions
            .get(version)
            .cloned()
            .ok_or_else(|| anyhow!("Invalid version: {:?}", version))?;
        let validator = self
            .validators
            .get_mut(&id)
            .ok_or_else(|| anyhow!("Invalid id: {}", id))?;
        validator.upgrade(version)
    }

    fn full_nodes<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn FullNode> + 'a> {
        Box::new(self.fullnodes.values().map(|v| v as &'a dyn FullNode))
    }

    fn full_nodes_mut<'a>(&'a mut self) -> Box<dyn Iterator<Item = &'a mut dyn FullNode> + 'a> {
        Box::new(
            self.fullnodes
                .values_mut()
                .map(|v| v as &'a mut dyn FullNode),
        )
    }

    fn full_node(&self, id: PeerId) -> Option<&dyn FullNode> {
        self.fullnodes.get(&id).map(|v| v as &dyn FullNode)
    }

    fn full_node_mut(&mut self, id: PeerId) -> Option<&mut dyn FullNode> {
        self.fullnodes.get_mut(&id).map(|v| v as &mut dyn FullNode)
    }

    fn add_validator(&mut self, _version: &Version, _template: NodeConfig) -> Result<PeerId> {
        todo!()
    }

    fn remove_validator(&mut self, _id: PeerId) -> Result<()> {
        todo!()
    }

    fn add_full_node(&mut self, version: &Version, template: NodeConfig) -> Result<PeerId> {
        self.add_fullnode(version, template)
    }

    fn remove_full_node(&mut self, id: PeerId) -> Result<()> {
        if let Some(mut fullnode) = self.fullnodes.remove(&id) {
            fullnode.stop();
        }

        Ok(())
    }

    fn versions<'a>(&'a self) -> Box<dyn Iterator<Item = Version> + 'a> {
        Box::new(self.versions.keys().cloned())
    }

    fn chain_info(&mut self) -> ChainInfo<'_> {
        ChainInfo::new(
            &mut self.root_account,
            self.validators
                .values()
                .map(|v| v.rest_api_endpoint().to_string())
                .next()
                .unwrap(),
            self.chain_id,
        )
    }

    fn logs_location(&mut self) -> String {
        self.dir.persist();
        self.dir.display().to_string()
    }
}
