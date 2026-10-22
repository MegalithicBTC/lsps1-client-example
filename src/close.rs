//! Close every channel, wait until the funds are back in the on-chain wallet,
//! then sweep the whole balance to an address.
//!
//! If the channels are already closed or closing, it goes straight to waiting
//! for the funds, so it is safe to stop with Ctrl-C and run again later.

use crate::{ask_yes_no, cmd, fmt_sats, lsp_node, prompt, stop, sync_wallets};
use ldk_node::bitcoin::{Address, Network};
use ldk_node::lightning::chain::channelmonitor::BalanceSource;
use ldk_node::{BalanceDetails, ChannelDetails, LightningBalance, Node, PendingSweepBalance};
use std::thread;
use std::time::{Duration, Instant};

const COOP_CLOSE_TIMEOUT_SECS: u64 = 60;
const POLL_SECS: u64 = 60;

// A closing transaction leaves out any output at or below the dust limit, and
// LDK's dust limit is 354 sats. A balance that small doesn't come back.
const DUST_LIMIT_SATS: u64 = 354;

// What closing means for an LSPS1 channel, shown before asking to close.
pub fn print_close_warning(node: &Node) {
	let channels = node.list_channels();
	if channels.is_empty() {
		return;
	}
	let ours = our_balance_sats(&channels);
	println!("Closing gives up the inbound liquidity you paid for: the LSP's fee isn't");
	println!("refunded, and receiving again means buying a new channel. You'd normally keep");
	println!("it open for the whole lease.");
	if ours == 0 {
		println!();
		println!("Your side of the channel is empty, so nothing comes back on-chain.");
	} else if ours <= DUST_LIMIT_SATS {
		println!();
		println!(
			"Your side of the channel holds {} sats. That's at or below the {} sat dust",
			fmt_sats(ours),
			DUST_LIMIT_SATS
		);
		println!("limit, so the closing transaction has no output for you: those sats go to");
		println!("the closing fee and don't come back on-chain.");
	}
}

pub fn close_and_sweep(node: &Node) -> ! {
	// ── close channels, or note that they are already closed/closing ───────
	let channels = node.list_channels();
	if !channels.is_empty() {
		let ours = our_balance_sats(&channels);
		close_all(node, &channels);
		println!("All channels are closed.\n");
		if ours <= DUST_LIMIT_SATS {
			// nothing is coming back from the channel, so there's nothing to
			// wait for
			if ours > 0 {
				println!("Your {} sats were dust, so nothing comes back on-chain.", fmt_sats(ours));
			}
			sync_wallets(node);
			if node.list_balances().spendable_onchain_balance_sats == 0 {
				println!("Your on-chain wallet is empty, so there is nothing to sweep.");
				stop(node, 0);
			}
			sweep(node, None, false);
		}
	} else {
		// sync first so the balances show whether an earlier close is still
		// settling
		sync_wallets(node);
		let balances = node.list_balances();
		if settling(&balances).0 > 0 {
			println!("No open channels: your channel is already closed or closing.\n");
		} else if balances.total_onchain_balance_sats == 0 {
			println!("No channels and no on-chain balance. Nothing to sweep.");
			stop(node, 0);
		}
	}

	// ── wait for the funds, then sweep ─────────────────────────────────────
	wait_for_onchain_funds(node);
	// Every channel is closed and settled, so no anchor reserve is needed.
	sweep(node, None, false);
}

fn close_all(node: &Node, channels: &[ChannelDetails]) {
	// A cooperative close needs the LSP online and connected.
	let (lsp_pubkey, lsp_address) = lsp_node();
	if let Err(e) = node.connect(lsp_pubkey, lsp_address, true) {
		eprintln!("Could not connect to the LSP ({e}); a cooperative close may not complete.");
	}

	println!("Closing your channel with the LSP, please wait...");
	for c in channels {
		match node.close_channel(&c.user_channel_id, c.counterparty_node_id) {
			Ok(()) => println!(
				"  channel {} ({} sats) with {}",
				c.channel_id,
				fmt_sats(c.channel_value_sats),
				c.counterparty_node_id
			),
			Err(e) => eprintln!("  could not start closing channel {}: {e}", c.channel_id),
		}
	}

	println!("Waiting up to {COOP_CLOSE_TIMEOUT_SECS}s for the other side to agree...");
	if wait_until_no_channels(node, COOP_CLOSE_TIMEOUT_SECS) {
		return;
	}

	let remaining = node.list_channels();
	println!();
	println!("{} channel(s) did not close cooperatively.", remaining.len());
	println!("You can force-close instead. That works without the LSP, but your funds stay");
	println!("locked by a timelock (up to ~2 weeks) and the closing fees are higher.");
	if !ask_yes_no("Force-close now?") {
		println!("Not force-closing. Run {} later to retry the", cmd("close_and_sweep"));
		println!("cooperative close.");
		stop(node, 0);
	}
	for c in &remaining {
		match node.force_close_channel(&c.user_channel_id, c.counterparty_node_id, None) {
			Ok(()) => println!("  force-closed channel {}", c.channel_id),
			Err(e) => eprintln!("  could not force-close channel {}: {e}", c.channel_id),
		}
	}
	if !wait_until_no_channels(node, 60) {
		eprintln!("Some channels are still open; see the logs.");
		stop(node, 1);
	}
}

// Our side of the channels, reserve included: what a close would pay us
// before fees.
fn our_balance_sats(channels: &[ChannelDetails]) -> u64 {
	channels
		.iter()
		.map(|c| c.outbound_capacity_msat / 1000 + c.unspendable_punishment_reserve.unwrap_or(0))
		.sum()
}

fn wait_until_no_channels(node: &Node, timeout_secs: u64) -> bool {
	let start = Instant::now();
	while start.elapsed().as_secs() < timeout_secs {
		if node.list_channels().is_empty() {
			return true;
		}
		thread::sleep(Duration::from_secs(2));
	}
	node.list_channels().is_empty()
}

// Sats from closed channels that are not yet spendable in the on-chain wallet,
// and the block height by which the last of them should be (when known).
fn settling(b: &BalanceDetails) -> (u64, Option<u32>) {
	let mut sats = 0;
	let mut done_height: Option<u32> = None;
	let mut add = |amount: u64, height: Option<u32>| {
		sats += amount;
		done_height = done_height.max(height);
	};

	for lb in &b.lightning_balances {
		match lb {
			// A confirmed cooperative close pays straight to our on-chain wallet
			// and already shows up in its balance.
			LightningBalance::ClaimableAwaitingConfirmations {
				source: BalanceSource::CoopClose,
				..
			} => {},
			LightningBalance::ClaimableAwaitingConfirmations {
				amount_satoshis,
				confirmation_height,
				..
			} => add(*amount_satoshis, Some(*confirmation_height)),
			LightningBalance::ContentiousClaimable { amount_satoshis, timeout_height, .. } => {
				add(*amount_satoshis, Some(*timeout_height))
			},
			LightningBalance::MaybeTimeoutClaimableHTLC {
				amount_satoshis,
				claimable_height,
				..
			} => add(*amount_satoshis, Some(*claimable_height)),
			LightningBalance::ClaimableOnChannelClose { amount_satoshis, .. }
			| LightningBalance::MaybePreimageClaimableHTLC { amount_satoshis, .. }
			| LightningBalance::CounterpartyRevokedOutputClaimable { amount_satoshis, .. } => {
				add(*amount_satoshis, None)
			},
		}
	}
	for pb in &b.pending_balances_from_channel_closures {
		match pb {
			PendingSweepBalance::AwaitingThresholdConfirmations {
				amount_satoshis,
				confirmation_height,
				..
			} => add(*amount_satoshis, Some(*confirmation_height)),
			PendingSweepBalance::PendingBroadcast { amount_satoshis, .. }
			| PendingSweepBalance::BroadcastAwaitingConfirmation { amount_satoshis, .. } => {
				add(*amount_satoshis, None)
			},
		}
	}
	(sats, done_height)
}

fn wait_for_onchain_funds(node: &Node) {
	let mut explained = false;
	let mut last_line = String::new();
	loop {
		let b = node.list_balances();
		let (settling_sats, done_height) = settling(&b);
		if settling_sats == 0 && b.spendable_onchain_balance_sats > 0 {
			return;
		}

		if !explained {
			println!("The funds can only be swept once they are back in your on-chain wallet:");
			println!("  - cooperative close: when the closing transaction confirms (~10 minutes)");
			println!("  - force close: after a timelock as well (up to ~2 weeks)");
			println!("Checking your on-chain balance every {POLL_SECS}s. You can press Ctrl-C and");
			println!("run {} later; it picks up where it left off.\n", cmd("close_and_sweep"));
			explained = true;
		}

		let height = node.status().current_best_block.height;
		let mut line = format!(
			"[block {height}] on-chain spendable: {} sats",
			fmt_sats(b.spendable_onchain_balance_sats)
		);
		if settling_sats > 0 {
			line += &format!(", still settling: {} sats", fmt_sats(settling_sats));
			if let Some(h) = done_height.filter(|h| *h > height) {
				line += &format!(" (until about block {h}, {} blocks to go)", h - height);
			}
		} else {
			line += ", waiting for the closing transaction to confirm";
		}
		if line != last_line {
			println!("{line}");
			last_line = line;
		}

		thread::sleep(Duration::from_secs(POLL_SECS));
	}
}

// Sends the whole on-chain balance to `address` (asked for when None), after
// a confirmation. `retain_reserves` keeps the anchor-channel reserve back.
pub fn sweep(node: &Node, address: Option<Address>, retain_reserves: bool) -> ! {
	let b = node.list_balances();
	println!();
	println!("{} sats are in your on-chain wallet.", fmt_sats(b.spendable_onchain_balance_sats));

	let network = node.config().network;
	let address = match address {
		Some(address) => address,
		None => loop {
			let input = prompt(&format!("Bitcoin address to sweep to ({network}): "));
			if input.is_empty() {
				println!("Not sweeping.");
				stop(node, 0);
			}
			match parse_address(&input, network) {
				Ok(address) => break address,
				Err(e) => eprintln!("{e}"),
			}
		},
	};

	if !ask_yes_no(&format!("Send everything (minus the network fee) to {address}?")) {
		println!("Not sweeping.");
		stop(node, 0);
	}

	match node.onchain_payment().send_all_to_address(&address, retain_reserves, None) {
		Ok(txid) => {
			println!("Sweep transaction: {txid}");
			println!("Keeping the node online for 30s so the transaction propagates...");
			thread::sleep(Duration::from_secs(30));
			stop(node, 0);
		},
		Err(e) => {
			eprintln!("Sweep failed: {e}");
			stop(node, 1);
		},
	}
}

pub fn parse_address(input: &str, network: Network) -> Result<Address, String> {
	input
		.parse::<Address<_>>()
		.map_err(|e| format!("Invalid address: {e}"))?
		.require_network(network)
		.map_err(|e| format!("Wrong network: {e}"))
}
