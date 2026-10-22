//! Prints the LSP's LSPS1 options (lsps1.get_info), raw and explained. Doesn't
//! start a node.

use megalith_lsps1::{channel_size_sats, fmt_sats, load_env, lsp_uri, lsps1};

fn main() {
	load_env();
	let client = lsps1::Client::from_env();
	println!("GET {}", client.url("get_info"));
	let info = client.get_info().unwrap_or_else(|e| {
		eprintln!("get_info failed: {e}");
		std::process::exit(1);
	});
	println!("{}", serde_json::to_string_pretty(&info.json).unwrap());
	println!();

	let (min, max) = info.lsp_balance_range();
	let size = channel_size_sats();
	println!("You can order a channel of {} to {} sats.", fmt_sats(min), fmt_sats(max));
	println!(
		"CHANNEL_SIZE_SATS in .env is {} sats: {}",
		fmt_sats(size),
		if (min..=max).contains(&size) { "OK" } else { "OUT OF RANGE" }
	);
	let pubkey = lsp_uri().split('@').next().unwrap_or_default().to_string();
	println!(
		"The LSP node in .env ({}) is {}",
		lsp_uri(),
		if info.uris.iter().any(|u| u.starts_with(&pubkey)) {
			"listed in uris: OK"
		} else {
			"NOT listed in uris; update LSP_PUBKEY and LSP_ADDRESS"
		}
	);
}
