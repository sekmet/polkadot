// Copyright 2020 Parity Technologies (UK) Ltd.
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

//! The bitfield signing subsystem produces `SignedAvailabilityBitfield`s once per block.

use futures::{channel::oneshot, Future};
use polkadot_node_subsystem::{
	messages::{AllMessages, BitfieldSigningMessage},
	OverseerSignal, SubsystemResult,
};
use polkadot_node_subsystem::{FromOverseer, SpawnedSubsystem, Subsystem, SubsystemContext};
use polkadot_primitives::Hash;
use std::{collections::HashMap, pin::Pin};

struct BitfieldSigning;

impl BitfieldSigning {
	async fn run<Context>(mut ctx: Context)
	where
		Context: SubsystemContext<Message = BitfieldSigningMessage>,
	{
		let mut active_jobs = HashMap::new();

		loop {
			{
				use FromOverseer::*;
				use OverseerSignal::*;
				match ctx.recv().await {
					Ok(Communication { msg: _ }) => {
						unreachable!("BitfieldSigningMessage is uninstantiable; qed")
					}
					Ok(Signal(StartWork(hash))) => {
						active_jobs.insert(hash.clone(), bitfield_signing_job(hash));
					}
					Ok(Signal(StopWork(hash))) => {
						active_jobs.remove(&hash);
					}
					Ok(Signal(Conclude)) => break,
					Err(err) => {
						log::warn!("{:?}", err);
						break;
					}
				}
			}

			// prune the active jobs list, removing the completed ones.
			// this discards the results of ready futures, but all futures
			// created with ctx.spawn have Output=SubsystemResult<()>, so that's fine.
			active_jobs.retain(|_, future| future.poll().is_pending());
		}
	}
}

impl<C> Subsystem<C> for BitfieldSigning
where
	C: SubsystemContext<Message = BitfieldSigningMessage>,
{
	fn start(self, ctx: C) -> SpawnedSubsystem {
		SpawnedSubsystem(Box::pin(async move {
			Self::run(ctx).await;
		}))
	}
}

fn bitfield_signing_job(hash: Hash) -> Pin<Box<dyn Future<Output = SubsystemResult<()>> + Send>> {
	// let (tx, _) = oneshot::channel();

	// ctx.send_message(AllMessages::CandidateValidation(
	// 	CandidateValidationMessage::Validate(
	// 		Default::default(),
	// 		Default::default(),
	// 		PoVBlock {
	// 			block_data: BlockData(Vec::new()),
	// 		},
	// 		tx,
	// 	)
	// )).await.unwrap();
	unimplemented!()
}
