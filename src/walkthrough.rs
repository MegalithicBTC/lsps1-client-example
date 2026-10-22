//! The guided flow behind `cargo run`: buy an inbound channel with LSPS1, then
//! optionally receive a test payment into it, send part of it back out, and
//! close the channel.

use crate::{ask_yes_no, close, cmd, fmt_sats, order, pay, receive, setup_node, stop};

// Small test amounts: the receive shows the inbound liquidity working, the
// send shows the channel can pay out too.
const RECEIVE_TEST_SATS: u64 = 100;
const SEND_TEST_SATS: u64 = 50;

pub fn run() -> ! {
	let (node, log_path) = setup_node("ldk_node");

	// ── 1. buy a channel ───────────────────────────────────────────────────
	order::get_channel(&node, &log_path);
	println!();
	pay::print_balance(&node);
	println!();
	println!("Don't quit yet: that takes your node offline, and it can't send or receive");
	println!("payments while it's offline.");
	println!();

	// ── 2. receive a test payment ──────────────────────────────────────────
	let received = ask_yes_no(&format!(
		"Would you like to receive a {} sat test payment into the channel? We recommend it.",
		fmt_sats(RECEIVE_TEST_SATS)
	)) && receive::receive(&node, RECEIVE_TEST_SATS, &log_path);

	// ── 3. send a test payment ─────────────────────────────────────────────
	// offered after a receive, or on a later run if there's already enough
	if received || pay::sendable_msat(&node) > SEND_TEST_SATS * 1000 {
		println!();
		pay::print_balance(&node);
		println!();
		let reserve = pay::reserve_sats(&node);
		if pay::sendable_msat(&node) <= SEND_TEST_SATS * 1000 {
			println!(
				"You can't send {} sats yet: the LSP asks your side of the channel to keep a {} sat",
				fmt_sats(SEND_TEST_SATS),
				fmt_sats(reserve)
			);
			println!("reserve, and only sats above it can be sent. Receive more first (try");
			println!("{}), then send with {}.", cmd("make_invoice"), cmd("pay_invoice"));
		} else if ask_yes_no(&format!(
			"Would you like to send {} sats back out of the channel?",
			fmt_sats(SEND_TEST_SATS)
		)) {
			pay::wait_until_ready(&node);
			println!();
			println!(
				"Make an invoice for {} sats in another Lightning wallet and paste it here.",
				fmt_sats(SEND_TEST_SATS)
			);
			println!(
				"An invoice with no amount works too; it's paid {} sats.",
				fmt_sats(SEND_TEST_SATS)
			);
			if pay::pay_one(&node, SEND_TEST_SATS, &log_path) {
				println!();
				pay::print_balance(&node);
			}
		}
	}

	// ── 4. close ───────────────────────────────────────────────────────────
	println!();
	close::print_close_warning(&node);
	println!();
	if ask_yes_no("Would you like to close this channel?") {
		close::close_and_sweep(&node);
	}
	println!("Leaving the channel open. Check on it with {}, and", cmd("node_info"));
	println!("close it later with {}.", cmd("close_and_sweep"));
	stop(&node, 0);
}
