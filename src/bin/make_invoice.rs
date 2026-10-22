//! Makes an invoice to receive into your channel and waits for it to be paid.
//! The amount in sats is the first argument (default 100).

use megalith_lsps1::{cmd, order, pay, receive, setup_node, stop};
use std::env;

fn main() {
	let amount_sats: u64 = env::args()
		.skip(1)
		.find(|a| !a.starts_with("--"))
		.map(|a| a.replace(',', "").parse().expect("the amount must be a number of sats"))
		.unwrap_or(100);

	let (node, log_path) = setup_node("make_invoice");
	println!("Logs ➜ {log_path}");

	if order::lsp_channel(&node).is_none() {
		eprintln!("No channel with the LSP yet; buy one with {}.", cmd(""));
		stop(&node, 1);
	}
	println!("Waiting for the channel to reconnect...");
	while !order::lsp_channel(&node).is_some_and(|c| c.is_usable) {
		std::thread::sleep(std::time::Duration::from_secs(2));
	}
	pay::print_balance(&node);

	let received = receive::receive(&node, amount_sats, &log_path);
	if received {
		println!();
		pay::print_balance(&node);
	}
	stop(&node, if received { 0 } else { 1 });
}
