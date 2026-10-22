//! Sweeps the on-chain wallet to an address without touching channels.
//! The address is asked for if not given. --no-reserves also spends the
//! anchor-channel reserve.

use megalith_lsps1::{close, setup_node, stop, sync_wallets};
use std::env;

fn main() {
	// ── args ───────────────────────────────────────────────────────────────
	let address_arg = env::args().skip(1).find(|a| !a.starts_with("--"));
	let retain_reserves = !env::args().any(|a| a == "--no-reserves");

	// ── setup node ─────────────────────────────────────────────────────────
	let (node, log_path) = setup_node("sweep_onchain");
	println!("Logs ➜ {log_path}");

	let address = address_arg.map(|a| {
		close::parse_address(&a, node.config().network).unwrap_or_else(|e| {
			eprintln!("{e}");
			stop(&node, 2);
		})
	});

	sync_wallets(&node);
	if node.list_balances().spendable_onchain_balance_sats == 0 {
		eprintln!("No spendable on-chain balance to sweep.");
		stop(&node, 1);
	}

	close::sweep(&node, address, retain_reserves);
}
