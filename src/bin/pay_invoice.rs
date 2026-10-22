//! Shows your balance, then pays BOLT11 invoices you paste in.

use megalith_lsps1::{pay, setup_node, stop};

fn main() {
	let (node, log_path) = setup_node("pay_invoice");
	println!("Logs ➜ {log_path}");

	pay::wait_until_ready(&node);
	pay::pay_invoices(&node, &log_path);
	stop(&node, 0);
}
