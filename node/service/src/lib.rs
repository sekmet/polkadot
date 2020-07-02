// Copyright 2017-2020 Parity Technologies (UK) Ltd.
// This file is part of Polkadot.

// Polkadot is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Polkadot is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Polkadot.  If not, see <http://www.gnu.org/licenses/>.

//! Polkadot service. Specialized wrapper over substrate service.

pub mod chain_spec;
mod grandpa_support;
mod client;

use std::sync::Arc;
use std::time::Duration;
use polkadot_primitives::{parachain, AccountId, Nonce, Balance};
#[cfg(feature = "full-node")]
use service::error::Error as ServiceError;
use grandpa::{self, FinalityProofProvider as GrandpaFinalityProofProvider};
use sc_executor::native_executor_instance;
use log::info;
use sp_blockchain::HeaderBackend;
use polkadot_overseer::{self as overseer, BlockInfo, Overseer, OverseerHandler};
use polkadot_subsystem::{
	Subsystem, SubsystemContext, SpawnedSubsystem,
	messages::{CandidateValidationMessage, CandidateBackingMessage},
};
use sp_trie::PrefixedMemoryDB;
pub use service::{
	Role, PruningMode, TransactionPoolOptions, Error, RuntimeGenesis,
	TFullClient, TLightClient, TFullBackend, TLightBackend, TFullCallExecutor, TLightCallExecutor,
	Configuration, ChainSpec, ServiceComponents, TaskManager,
};
pub use service::config::{DatabaseConfig, PrometheusConfig};
pub use sc_executor::NativeExecutionDispatch;
pub use sc_client_api::{Backend, ExecutionStrategy, CallExecutor};
pub use sc_consensus::LongestChain;
pub use sp_api::{ApiRef, Core as CoreApi, ConstructRuntimeApi, ProvideRuntimeApi, StateBackend};
pub use sp_runtime::traits::{DigestFor, HashFor, NumberFor};
pub use consensus_common::{Proposal, SelectChain, BlockImport, RecordProof, block_validation::Chain};
pub use polkadot_primitives::parachain::{CollatorId, ParachainHost};
pub use polkadot_primitives::{Block, BlockId};
pub use sp_runtime::traits::{Block as BlockT, self as runtime_traits, BlakeTwo256};
pub use chain_spec::{PolkadotChainSpec, KusamaChainSpec, WestendChainSpec};
#[cfg(feature = "full-node")]
pub use codec::Codec;
pub use polkadot_runtime;
pub use kusama_runtime;
pub use westend_runtime;
use prometheus_endpoint::Registry;
pub use self::client::PolkadotClient;

native_executor_instance!(
	pub PolkadotExecutor,
	polkadot_runtime::api::dispatch,
	polkadot_runtime::native_version,
	frame_benchmarking::benchmarking::HostFunctions,
);

native_executor_instance!(
	pub KusamaExecutor,
	kusama_runtime::api::dispatch,
	kusama_runtime::native_version,
	frame_benchmarking::benchmarking::HostFunctions,
);

native_executor_instance!(
	pub WestendExecutor,
	westend_runtime::api::dispatch,
	westend_runtime::native_version,
	frame_benchmarking::benchmarking::HostFunctions,
);

/// A set of APIs that polkadot-like runtimes must implement.
pub trait RuntimeApiCollection<Extrinsic: codec::Codec + Send + Sync + 'static>:
	sp_transaction_pool::runtime_api::TaggedTransactionQueue<Block>
	+ sp_api::ApiExt<Block, Error = sp_blockchain::Error>
	+ babe_primitives::BabeApi<Block>
	+ grandpa_primitives::GrandpaApi<Block>
	+ ParachainHost<Block>
	+ sp_block_builder::BlockBuilder<Block>
	+ system_rpc_runtime_api::AccountNonceApi<Block, AccountId, Nonce>
	+ pallet_transaction_payment_rpc_runtime_api::TransactionPaymentApi<Block, Balance, Extrinsic>
	+ sp_api::Metadata<Block>
	+ sp_offchain::OffchainWorkerApi<Block>
	+ sp_session::SessionKeys<Block>
	+ authority_discovery_primitives::AuthorityDiscoveryApi<Block>
where
	Extrinsic: RuntimeExtrinsic,
	<Self as sp_api::ApiExt<Block>>::StateBackend: sp_api::StateBackend<BlakeTwo256>,
{}

impl<Api, Extrinsic> RuntimeApiCollection<Extrinsic> for Api
where
	Api:
	sp_transaction_pool::runtime_api::TaggedTransactionQueue<Block>
	+ sp_api::ApiExt<Block, Error = sp_blockchain::Error>
	+ babe_primitives::BabeApi<Block>
	+ grandpa_primitives::GrandpaApi<Block>
	+ ParachainHost<Block>
	+ sp_block_builder::BlockBuilder<Block>
	+ system_rpc_runtime_api::AccountNonceApi<Block, AccountId, Nonce>
	+ pallet_transaction_payment_rpc_runtime_api::TransactionPaymentApi<Block, Balance, Extrinsic>
	+ sp_api::Metadata<Block>
	+ sp_offchain::OffchainWorkerApi<Block>
	+ sp_session::SessionKeys<Block>
	+ authority_discovery_primitives::AuthorityDiscoveryApi<Block>,
	Extrinsic: RuntimeExtrinsic,
	<Self as sp_api::ApiExt<Block>>::StateBackend: sp_api::StateBackend<BlakeTwo256>,
{}

pub trait RuntimeExtrinsic: codec::Codec + Send + Sync + 'static {}

impl<E> RuntimeExtrinsic for E where E: codec::Codec + Send + Sync + 'static {}

/// Can be called for a `Configuration` to check if it is a configuration for the `Kusama` network.
pub trait IdentifyVariant {
	/// Returns if this is a configuration for the `Kusama` network.
	fn is_kusama(&self) -> bool;

	/// Returns if this is a configuration for the `Westend` network.
	fn is_westend(&self) -> bool;
}

impl IdentifyVariant for Box<dyn ChainSpec> {
	fn is_kusama(&self) -> bool {
		self.id().starts_with("kusama") || self.id().starts_with("ksm")
	}
	fn is_westend(&self) -> bool {
		self.id().starts_with("westend") || self.id().starts_with("wnd")
	}
}

// If we're using prometheus, use a registry with a prefix of `polkadot`.
fn set_prometheus_registry(config: &mut Configuration) -> Result<(), ServiceError> {
	if let Some(PrometheusConfig { registry, .. }) = config.prometheus_config.as_mut() {
		*registry = Registry::new_custom(Some("polkadot".into()), None)?;
	}

	Ok(())
}

/// Starts a `ServiceBuilder` for a full service.
///
/// Use this macro if you don't actually need the full service, but just the builder in order to
/// be able to perform chain operations.
macro_rules! new_full_up_to_import_queue {
	($config:expr, $runtime:ty, $executor:ty) => {{
		set_prometheus_registry(&mut $config)?;

		let inherent_data_providers = inherents::InherentDataProviders::new();

		let (client, backend, keystore, task_manager) = service::new_full_parts::<
			Block, $runtime, $executor
		>(&$config)?;
		let client = Arc::new(client);

		let select_chain = sc_consensus::LongestChain::new(backend.clone());

		let pool_api = Arc::new(sc_transaction_pool::FullChainApi::new(client.clone()));
		let transaction_pool = sc_transaction_pool::BasicPool::new_full(
			$config.transaction_pool.clone(),
			pool_api,
			$config.prometheus_registry(),
			task_manager.spawn_handle(),
			client.clone(),
		);

		let grandpa_hard_forks = if $config.chain_spec.is_kusama() {
			grandpa_support::kusama_hard_forks()
		} else {
			Vec::new()
		};

		let (grandpa_block_import, grandpa_link) =
			grandpa::block_import_with_authority_set_hard_forks(
				client.clone(),
				select_chain.clone(),
				grandpa_hard_forks,
			)?;

		let justification_import = grandpa_block_import.clone();

		let (block_import, babe_link) = babe::block_import(
			babe::Config::get_or_compute(&*client)?,
			grandpa_block_import,
			client.clone(),
		)?;

		let import_queue = babe::import_queue(
			babe_link.clone(),
			block_import.clone(),
			Some(Box::new(justification_import)),
			None,
			client.clone(),
			inherent_data_providers.clone(),
			&task_manager.spawn_handle(),
			$config.prometheus_registry(),
		)?;

		let import_setup = (block_import, grandpa_link, babe_link);

		(
			client, backend, keystore, task_manager, inherent_data_providers, select_chain,
			transaction_pool, import_setup, import_queue,
		)
	}}
}

struct CandidateValidationSubsystem;

impl<C> Subsystem<C> for CandidateValidationSubsystem
	where C: SubsystemContext<Message = CandidateValidationMessage>
{
	fn start(self, mut ctx: C) -> SpawnedSubsystem {
		SpawnedSubsystem(Box::pin(async move {
			while let Ok(_) = ctx.recv().await {}
		}))
	}
}

struct CandidateBackingSubsystem;

impl<C> Subsystem<C> for CandidateBackingSubsystem
	where C: SubsystemContext<Message = CandidateBackingMessage>
{
	fn start(self, mut ctx: C) -> SpawnedSubsystem {
		SpawnedSubsystem(Box::pin(async move {
			while let Ok(_) = ctx.recv().await {}
		}))
	}
}

fn real_overseer<S: futures::task::Spawn>(
	leaves: impl IntoIterator<Item = BlockInfo>,
	s: S,
) -> Result<(Overseer<S>, OverseerHandler), ServiceError> {
	let validation = CandidateValidationSubsystem;
	let candidate_backing = CandidateBackingSubsystem;
	Overseer::new(leaves, validation, candidate_backing, s)
		.map_err(|e| ServiceError::Other(format!("Failed to create an Overseer: {:?}", e)))
}

/// Builds a new service for a full client.
#[macro_export]
macro_rules! new_full {
	(
		$config:expr,
		$collating_for:expr,
		$authority_discovery_enabled:expr,
		$grandpa_pause:expr,
		$runtime:ty,
		$dispatch:ty,
	) => {{
		use sc_client_api::ExecutorProvider;
		use sp_core::traits::BareCryptoStorePtr;
		use grandpa::StorageAndProofProvider;

		let is_collator = $collating_for.is_some();
		let role = $config.role.clone();
		let is_authority = role.is_authority() && !is_collator;
		let force_authoring = $config.force_authoring;
		let disable_grandpa = $config.disable_grandpa;
		let name = $config.network.node_name.clone();
		let prometheus_registry = $config.prometheus_registry().cloned();

		let (
			client, backend, keystore, mut task_manager, inherent_data_providers, select_chain,
			transaction_pool, import_setup, import_queue, 
		) = new_full_up_to_import_queue!($config, $runtime, $dispatch);

		let (_, grandpa_link, babe_link) = &import_setup;

		let shared_authority_set = grandpa_link.shared_authority_set().clone();
		let shared_voter_state = grandpa::SharedVoterState::empty();

		let rpc_setup = shared_voter_state.clone();

		let babe_config = babe_link.config().clone();
		let shared_epoch_changes = babe_link.epoch_changes().clone();

		let rpc_extensions_builder = {
			let client = client.clone();
			let transaction_pool = transaction_pool.clone();
			let select_chain = select_chain.clone();
			let keystore = keystore.clone();

			move |deny_unsafe| -> polkadot_rpc::RpcExtension {
				let deps = polkadot_rpc::FullDeps {
					client: client.clone(),
					pool: transaction_pool.clone(),
					select_chain: select_chain.clone(),
					deny_unsafe,
					babe: polkadot_rpc::BabeDeps {
						babe_config: babe_config.clone(),
						shared_epoch_changes: shared_epoch_changes.clone(),
						keystore: keystore.clone(),
					},
					grandpa: polkadot_rpc::GrandpaDeps {
						shared_voter_state: shared_voter_state.clone(),
						shared_authority_set: shared_authority_set.clone(),
					},
				};

				polkadot_rpc::create_full(deps)
			}
		};
			
		let provider = client.clone() as Arc<dyn StorageAndProofProvider<_, _>>;
		let finality_proof_provider = Arc::new(GrandpaFinalityProofProvider::new(backend.clone(), provider));

		let ServiceComponents {
			network, telemetry_on_connect_sinks, ..
		} = service::build(service::ServiceParams {
			config: $config,
			backend: backend.clone(),
			client: client.clone(),
			block_announce_validator_builder: None,
			finality_proof_request_builder: None,
			finality_proof_provider: Some(finality_proof_provider),
			on_demand: None,
			import_queue,
			keystore: keystore.clone(),
			task_manager: &mut task_manager,
			remote_blockchain: None,
			rpc_extensions_builder: Box::new(rpc_extensions_builder),
			transaction_pool: transaction_pool.clone()
		})?;

		let (block_import, link_half, babe_link) = import_setup;

		let shared_voter_state = rpc_setup;

		let overseer_client = client.clone();
		let spawner = task_manager.spawn_handle();
		let leaves: Vec<_> = select_chain.clone()
			.leaves()
			.unwrap_or_else(|_| vec![])
			.into_iter()
			.filter_map(|hash| {
				let number = client.number(hash).ok()??;
				let parent_hash = client.header(&BlockId::Hash(hash)).ok()??.parent_hash;

				Some(BlockInfo {
					hash,
					parent_hash,
					number,
				})
			})
			.collect();

		let (overseer, handler) = real_overseer(leaves, spawner)?;

		task_manager.spawn_essential_handle().spawn_blocking("overseer", Box::pin(async move {
			use futures::{pin_mut, select, FutureExt};

			let forward = overseer::forward_events(overseer_client, handler);

			let forward = forward.fuse();
			let overseer_fut = overseer.run().fuse();

			pin_mut!(overseer_fut);
			pin_mut!(forward);

			loop {
				select! {
					_ = forward => break,
					_ = overseer_fut => break,
					complete => break,
				}
			}
		}));

		if role.is_authority() {
			let can_author_with =
				consensus_common::CanAuthorWithNativeVersion::new(client.executor().clone());

			// TODO: custom proposer (https://github.com/paritytech/polkadot/issues/1248)
			let proposer = sc_basic_authorship::ProposerFactory::new(
				client.clone(),
				transaction_pool,
				None,
			);

			let babe_config = babe::BabeParams {
				keystore: keystore.clone(),
				client: client.clone(),
				select_chain,
				block_import,
				env: proposer,
				sync_oracle: network.clone(),
				inherent_data_providers: inherent_data_providers.clone(),
				force_authoring,
				babe_link,
				can_author_with,
			};

			let babe = babe::start_babe(babe_config)?;
			task_manager.spawn_essential_handle().spawn_blocking("babe", babe);
		}

		// if the node isn't actively participating in consensus then it doesn't
		// need a keystore, regardless of which protocol we use below.
		let keystore = if is_authority {
			Some(keystore.clone() as BareCryptoStorePtr)
		} else {
			None
		};

		let config = grandpa::Config {
			// FIXME substrate#1578 make this available through chainspec
			gossip_duration: Duration::from_millis(1000),
			justification_period: 512,
			name: Some(name),
			observer_enabled: false,
			keystore,
			is_authority: role.is_network_authority(),
		};

		let enable_grandpa = !disable_grandpa;
		if enable_grandpa {
			// start the full GRANDPA voter
			// NOTE: unlike in substrate we are currently running the full
			// GRANDPA voter protocol for all full nodes (regardless of whether
			// they're validators or not). at this point the full voter should
			// provide better guarantees of block and vote data availability than
			// the observer.

			// add a custom voting rule to temporarily stop voting for new blocks
			// after the given pause block is finalized and restarting after the
			// given delay.
			let voting_rule = match $grandpa_pause {
				Some((block, delay)) => {
					info!("GRANDPA scheduled voting pause set for block #{} with a duration of {} blocks.",
						block,
						delay,
					);

					grandpa::VotingRulesBuilder::default()
						.add(grandpa_support::PauseAfterBlockFor(block, delay))
						.build()
				},
				None =>
					grandpa::VotingRulesBuilder::default()
						.build(),
			};

			let grandpa_config = grandpa::GrandpaParams {
				config,
				link: link_half,
				network: network.clone(),
				inherent_data_providers: inherent_data_providers.clone(),
				telemetry_on_connect: Some(telemetry_on_connect_sinks.on_connect_stream()),
				voting_rule,
				prometheus_registry: prometheus_registry,
				shared_voter_state,
			};

			task_manager.spawn_essential_handle().spawn_blocking(
				"grandpa-voter",
				grandpa::run_grandpa_voter(grandpa_config)?
			);
		} else {
			grandpa::setup_disabled_grandpa(
				client.clone(),
				&inherent_data_providers,
				network.clone(),
			)?;
		}

		(task_manager, client)
	}}
}

pub struct FullNodeHandles;

/// Builds a new service for a light client.
#[macro_export]
macro_rules! new_light {
	($config:expr, $runtime:ty, $dispatch:ty) => {{
		use sc_client_api::RemoteBackend;

		crate::set_prometheus_registry(&mut $config)?;
		let inherent_data_providers = inherents::InherentDataProviders::new();

		let (client, backend, keystore, mut task_manager, on_demand) =
			service::new_light_parts::<Block, $runtime, $dispatch>(&$config)?;

		let transaction_pool_api = Arc::new(sc_transaction_pool::LightChainApi::new(
			client.clone(), on_demand.clone(),
		));
		let transaction_pool = Arc::new(sc_transaction_pool::BasicPool::new_light(
			$config.transaction_pool.clone(),
			transaction_pool_api,
			$config.prometheus_registry(),
			task_manager.spawn_handle(),
		));

		let grandpa_block_import = grandpa::light_block_import(
			client.clone(), backend.clone(),
			Arc::new(on_demand.checker().clone()) as Arc<_>,
		)?;

		let finality_proof_import = grandpa_block_import.clone();
		let finality_proof_request_builder =
			finality_proof_import.create_finality_proof_request_builder();

		let (babe_block_import, babe_link) = babe::block_import(
			babe::Config::get_or_compute(&*client)?,
			grandpa_block_import,
			client.clone(),
		)?;

		// FIXME: pruning task isn't started since light client doesn't do `AuthoritySetup`.
		let import_queue = babe::import_queue(
			babe_link,
			babe_block_import,
			None,
			Some(Box::new(finality_proof_import)),
			client.clone(),
			inherent_data_providers.clone(),
			&task_manager.spawn_handle(),
			$config.prometheus_registry(),
		)?;		

		let provider = client.clone() as Arc<dyn grandpa::StorageAndProofProvider<_, _>>;
		let finality_proof_provider =
			Arc::new(grandpa::FinalityProofProvider::new(backend.clone(), provider)) as _;

		let rpc_extensions = polkadot_rpc::create_light(polkadot_rpc::LightDeps {
			remote_blockchain: backend.remote_blockchain(),
			fetcher: on_demand.clone(),
			client: client.clone(),
			pool: transaction_pool.clone(),
		});
		
		service::build(service::ServiceParams {
			config: $config,
			block_announce_validator_builder: None,
			finality_proof_request_builder: Some(finality_proof_request_builder),
			finality_proof_provider: Some(finality_proof_provider),
			on_demand: Some(on_demand),
			task_manager: &mut task_manager,
			remote_blockchain: Some(backend.remote_blockchain()),
			rpc_extensions_builder: Box::new(service::NoopRpcExtensionBuilder(rpc_extensions)),
			client, transaction_pool, import_queue, keystore, backend,
		})?;

		Ok(task_manager)
	}}
}

/// Builds a new object suitable for chain operations.
pub fn new_chain_ops<Runtime, Dispatch, Extrinsic>(mut config: Configuration) -> Result<
	(
		Arc<service::TFullClient<Block, Runtime, Dispatch>>, 
		Arc<TFullBackend<Block>>,
		consensus_common::import_queue::BasicQueue<Block, PrefixedMemoryDB<BlakeTwo256>>,
		TaskManager,
	),
	ServiceError
>
where
	Runtime: ConstructRuntimeApi<Block, service::TFullClient<Block, Runtime, Dispatch>> + Send + Sync + 'static,
	Runtime::RuntimeApi:
	RuntimeApiCollection<Extrinsic, StateBackend = sc_client_api::StateBackendFor<TFullBackend<Block>, Block>>,
	Dispatch: NativeExecutionDispatch + 'static,
	Extrinsic: RuntimeExtrinsic,
{
	config.keystore = service::config::KeystoreConfig::InMemory;
	let (client, backend, _, task_manager, .., import_queue)
		= new_full_up_to_import_queue!(config, Runtime, Dispatch);
	Ok((client, backend, import_queue, task_manager))
}

/// Create a new Polkadot service for a full node.
#[cfg(feature = "full-node")]
pub fn polkadot_new_full(
	mut config: Configuration,
	collating_for: Option<(CollatorId, parachain::Id)>,
	_max_block_data_size: Option<u64>,
	_authority_discovery_enabled: bool,
	_slot_duration: u64,
	grandpa_pause: Option<(u32, u32)>,
)
	-> Result<(
		TaskManager,
		Arc<impl PolkadotClient<
			Block,
			TFullBackend<Block>,
			polkadot_runtime::RuntimeApi
		>>,
		FullNodeHandles,
	), ServiceError>
{
	let (components, client) = new_full!(
		config,
		collating_for,
		authority_discovery_enabled,
		grandpa_pause,
		polkadot_runtime::RuntimeApi,
		PolkadotExecutor,
	);

	Ok((components, client, FullNodeHandles))
}

/// Create a new Kusama service for a full node.
#[cfg(feature = "full-node")]
pub fn kusama_new_full(
	mut config: Configuration,
	collating_for: Option<(CollatorId, parachain::Id)>,
	_max_block_data_size: Option<u64>,
	_authority_discovery_enabled: bool,
	_slot_duration: u64,
	grandpa_pause: Option<(u32, u32)>,
) -> Result<(
		TaskManager,
		Arc<impl PolkadotClient<
			Block,
			TFullBackend<Block>,
			kusama_runtime::RuntimeApi
			>
		>,
		FullNodeHandles,
	), ServiceError>
{
	let (components, client) = new_full!(
		config,
		collating_for,
		authority_discovery_enabled,
		grandpa_pause,
		kusama_runtime::RuntimeApi,
		KusamaExecutor,
	);

	Ok((components, client, FullNodeHandles))
}

/// Create a new Kusama service for a full node.
#[cfg(feature = "full-node")]
pub fn westend_new_full(
	mut config: Configuration,
	collating_for: Option<(CollatorId, parachain::Id)>,
	_max_block_data_size: Option<u64>,
	_authority_discovery_enabled: bool,
	_slot_duration: u64,
	grandpa_pause: Option<(u32, u32)>,
)
	-> Result<(
		TaskManager,
		Arc<impl PolkadotClient<
			Block,
			TFullBackend<Block>,
			westend_runtime::RuntimeApi
		>>,
		FullNodeHandles,
	), ServiceError>
{
	let (components, client) = new_full!(
		config,
		collating_for,
		authority_discovery_enabled,
		grandpa_pause,
		westend_runtime::RuntimeApi,
		WestendExecutor,
	);

	Ok((components, client, FullNodeHandles))
}

/// Create a new Polkadot service for a light client.
pub fn polkadot_new_light(mut config: Configuration) -> Result<TaskManager, ServiceError>
{
	new_light!(config, polkadot_runtime::RuntimeApi, PolkadotExecutor)
}

/// Create a new Kusama service for a light client.
pub fn kusama_new_light(mut config: Configuration) -> Result<TaskManager, ServiceError>
{
	new_light!(config, kusama_runtime::RuntimeApi, KusamaExecutor)
}

/// Create a new Westend service for a light client.
pub fn westend_new_light(mut config: Configuration, ) -> Result<TaskManager, ServiceError>
{
	new_light!(config, westend_runtime::RuntimeApi, KusamaExecutor)
}
