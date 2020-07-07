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

use futures::{channel::oneshot, future::abortable, Future};
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
			use FromOverseer::*;
			use OverseerSignal::*;
			match ctx.recv().await {
				Ok(Communication { msg: _ }) => {
					unreachable!("BitfieldSigningMessage is uninstantiable; qed")
				}
				Ok(Signal(StartWork(hash))) => {
					let (future, abort_handle) = abortable(bitfield_signing_job(hash.clone()));
					// future currently returns a Result based on whether or not it was aborted;
					// let's ignore all that and return () unconditionally, to fit the interface.
					let future = async move {
						let _ = future.await;
					};
					active_jobs.insert(hash.clone(), abort_handle);
					ctx.spawn(Box::pin(future));
				}
				Ok(Signal(StopWork(hash))) => {
					if let Some(abort_handle) = active_jobs.remove(&hash) {
						abort_handle.abort();
					}
				}
				Ok(Signal(Conclude)) => break,
				Err(err) => {
					log::warn!("{:?}", err);
					break;
				}
			}
		}
	}
}

impl<Context> Subsystem<Context> for BitfieldSigning
where
	Context: SubsystemContext<Message = BitfieldSigningMessage>,
{
	fn start(self, ctx: Context) -> SpawnedSubsystem {
		SpawnedSubsystem(Box::pin(async move {
			Self::run(ctx).await;
		}))
	}
}

async fn bitfield_signing_job(hash: Hash) {
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
