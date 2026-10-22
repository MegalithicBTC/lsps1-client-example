//! Closes every channel, waits until the funds are back in the on-chain wallet,
//! then asks for a Bitcoin address and sweeps the whole balance to it.
//!
//! If the channels are already closed or closing, it goes straight to waiting
//! for the funds, so it is safe to stop with Ctrl-C and run again later.

use megalith_lsps1::{ask_yes_no, close, setup_node, stop};

fn main() {
	let (node, log_path) = setup_node("close_and_sweep");
	println!("Logs ➜ {log_path}\n");

	if !node.list_channels().is_empty() {
		close::print_close_warning(&node);
		println!();
		if !ask_yes_no("Close your channel?") {
			println!("Leaving the channel open.");
			stop(&node, 0);
		}
	}
	close::close_and_sweep(&node);
}
