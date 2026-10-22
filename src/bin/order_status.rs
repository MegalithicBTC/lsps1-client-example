//! Prints an LSPS1 order (lsps1.get_order): the saved one, or the order id
//! given as an argument. Doesn't start a node.

use megalith_lsps1::{cmd, load_env, lsps1, order};
use std::env;

fn main() {
	load_env();
	let order_id =
		env::args().skip(1).find(|a| !a.starts_with("--")).or_else(order::saved_order_id);
	let Some(order_id) = order_id else {
		eprintln!(
			"No saved order in {}. Pass an order id, or run {} to place one.",
			order::saved_order_path(),
			cmd("")
		);
		std::process::exit(1);
	};

	let client = lsps1::Client::from_env();
	println!("GET {}?order_id={order_id}", client.url("get_order"));
	let order = client.get_order(&order_id).unwrap_or_else(|e| {
		eprintln!("get_order failed: {e}");
		std::process::exit(1);
	});
	println!("{}", serde_json::to_string_pretty(&order.json).unwrap());
	println!();

	let payment = order.bolt11().map(|b| b.state.as_str()).unwrap_or("-");
	println!("order_state: {} | bolt11 payment: {payment}", order.order_state);
	match &order.channel {
		Some(c) => println!("channel: {} (lease ends {})", c.funding_outpoint, c.expires_at),
		None => println!("channel: not opened yet"),
	}
}
