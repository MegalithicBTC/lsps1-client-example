//! Pay BOLT11 invoices from the node, with enough diagnostics to explain a
//! failed send.

use crate::{cmd, fmt_sats, prompt, stop};
use ldk_node::bitcoin::secp256k1::PublicKey;
use ldk_node::lightning::routing::gossip::NodeId;
use ldk_node::lightning_invoice::Bolt11Invoice;
use ldk_node::payment::PaymentStatus;
use ldk_node::Node;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, thread, time::Duration};

// Waits until the node can pay: a usable channel and, with RGS, the public
// channel graph. Then prints the node's state.
pub fn wait_until_ready(node: &Node) {
	if node.list_channels().is_empty() {
		eprintln!("No channels yet; buy one with {}.", cmd(""));
		stop(node, 1);
	}

	// routes past the LSP need the public channel graph; RGS downloads it in
	// the background after start
	let use_rgs = env::var("RGS_SERVER_URL").is_ok();
	if use_rgs && node.status().latest_rgs_snapshot_timestamp.is_none() {
		println!("Downloading the public channel graph so your node can find routes.");
		println!("The first time takes a few seconds.");
	}

	let mut last_wait = String::new();
	loop {
		let chans = node.list_channels();
		let usable = chans.iter().filter(|c| c.is_usable).count();
		let peers_connected = node.list_peers().iter().filter(|p| p.is_connected).count();
		let graph_synced = !use_rgs || node.status().latest_rgs_snapshot_timestamp.is_some();

		// only print when something changes
		let line = format!(
			"[wait] peers_connected={} usable_chans={}/{} graph_synced={}",
			peers_connected,
			usable,
			chans.len(),
			graph_synced
		);
		if line != last_wait {
			println!("{line}");
			last_wait = line;
		}

		if usable > 0 && graph_synced {
			break;
		}
		thread::sleep(Duration::from_secs(2));
	}

	print_node_status(node);
}

// Asks for invoices and pays each one, until an empty line. An invoice
// without an amount asks for one.
pub fn pay_invoices(node: &Node, log_path: &str) {
	loop {
		println!();
		print_balance(node);
		let Some(invoice) = read_invoice("Paste a BOLT11 invoice (Enter when done): ") else {
			return;
		};
		let amount_sats = if invoice.amount_milli_satoshis().is_some() {
			None
		} else {
			match prompt("The invoice has no amount. Sats to pay: ").replace(',', "").parse() {
				Ok(sats) => Some(sats),
				Err(_) => {
					eprintln!("not a number of sats");
					continue;
				},
			}
		};
		pay(node, &invoice, amount_sats, log_path);
	}
}

// Asks for one invoice and pays it. An invoice without an amount is paid
// `default_sats`. Returns true if the payment succeeded.
pub fn pay_one(node: &Node, default_sats: u64, log_path: &str) -> bool {
	match read_invoice("Paste a BOLT11 invoice (Enter to skip): ") {
		Some(invoice) => pay(node, &invoice, Some(default_sats), log_path),
		None => false,
	}
}

// Asks until it gets a valid invoice, or None for an empty line.
fn read_invoice(question: &str) -> Option<Bolt11Invoice> {
	loop {
		let input = prompt(question).to_lowercase();
		let input = input.strip_prefix("lightning:").unwrap_or(&input);
		if input.is_empty() {
			return None;
		}
		match input.parse::<Bolt11Invoice>() {
			Ok(invoice) => return Some(invoice),
			Err(e) => eprintln!("invalid BOLT11 invoice: {e}"),
		}
	}
}

// Pays one invoice and waits until it succeeds or fails. `amount_sats` is
// only used when the invoice has no amount of its own.
fn pay(node: &Node, invoice: &Bolt11Invoice, amount_sats: Option<u64>, log_path: &str) -> bool {
	let amount_msat = match (invoice.amount_milli_satoshis(), amount_sats) {
		(Some(msat), _) => msat,
		(None, Some(sats)) => sats * 1000,
		(None, None) => {
			eprintln!("invoice has no amount; ask the payee for one with an amount in it");
			return false;
		},
	};
	print_invoice(node, invoice, amount_msat);
	if invoice.is_expired() {
		eprintln!("invoice has expired; ask the payee for a new one");
		return false;
	}

	// routing fees go on top of this; the router works them out and the send
	// fails if the channel can't cover them
	let sendable = sendable_msat(node);
	if amount_msat > sendable {
		eprintln!(
			"not enough: the invoice is {} sats, but you can send {} sats (routing fees included)",
			fmt_sats(amount_msat / 1000),
			fmt_sats(sendable / 1000)
		);
		let reserve = reserve_sats(node);
		if reserve > 0 {
			eprintln!(
				"Your side of the channel has to keep a {} sat reserve; only sats above it can be sent.",
				fmt_sats(reserve)
			);
		}
		return false;
	}

	let sent = if invoice.amount_milli_satoshis().is_some() {
		node.bolt11_payment().send(invoice, None)
	} else {
		node.bolt11_payment().send_using_amount(invoice, amount_msat, None)
	};
	let payment_id = match sent {
		Ok(id) => id,
		Err(e) => {
			eprintln!("send failed: {e} (reason in {log_path})");
			eprintln!(
				"If no route was found, routing fees on top of the amount may be more than you"
			);
			eprintln!("can send. Try a smaller amount.");
			return false;
		},
	};
	println!("sent {} sats; waiting for it to complete…", fmt_sats(amount_msat / 1000));

	loop {
		match node.payment(&payment_id).map(|p| (p.status, p.fee_paid_msat)) {
			Some((PaymentStatus::Succeeded, fee_msat)) => {
				let fee = fee_msat.map(|f| format!("{:.3}", f as f64 / 1000.0));
				println!(
					"SUCCEEDED: paid {} sats, routing fee {} sats",
					fmt_sats(amount_msat / 1000),
					fee.unwrap_or_else(|| "?".into())
				);
				return true;
			},
			Some((PaymentStatus::Failed, _)) => {
				eprintln!("FAILED (reason in {log_path})");
				return false;
			},
			_ => thread::sleep(Duration::from_secs(1)),
		}
	}
}

// What the channel can receive (the inbound liquidity you bought) and send
// (your own balance, minus the reserve).
pub fn print_balance(node: &Node) {
	let chans: Vec<_> = node.list_channels().into_iter().filter(|c| c.is_usable).collect();
	let capacity_sats: u64 = chans.iter().map(|c| c.channel_value_sats).sum();
	let receive_sats = chans.iter().map(|c| c.inbound_capacity_msat).sum::<u64>() / 1000;
	let reserve_sats = reserve_sats(node);
	let send_sats = sendable_msat(node) / 1000;

	let line = |label: &str, sats: u64, note: &str| {
		let text = format!("{label:<18}{:>10} sats  {note}", fmt_sats(sats));
		println!("{}", text.trim_end());
	};
	line("Channel capacity:", capacity_sats, "");
	line("  can receive:", receive_sats, "(inbound liquidity: the LSP's side)");
	line("  can send:", send_sats, "(your side; routing fees come out of this too)");
	if reserve_sats > 0 {
		line("  reserve:", reserve_sats, "(your side, can't be spent)");
	}

	// explain the numbers the first time only
	if BALANCE_EXPLAINED.swap(true, Ordering::Relaxed) {
		return;
	}
	println!();
	println!(
		"  - The LSP put all {} sats on its side of the channel, so you start",
		fmt_sats(capacity_sats)
	);
	println!("    with nothing to send and almost everything to receive. Each payment you");
	println!("    receive moves sats to your side; each one you send moves them back.");
	println!("  - \"Can receive\" is a little less than the capacity: the LSP keeps a reserve");
	println!("    on its side, so cheating (broadcasting an old channel state) always costs");
	println!("    it something.");
	if reserve_sats > 0 {
		println!(
			"  - Your side has a reserve too: {} sats. It's still yours and comes back",
			fmt_sats(reserve_sats)
		);
		println!("    when the channel closes, but you can't spend it in the channel.");
	}
}

static BALANCE_EXPLAINED: AtomicBool = AtomicBool::new(false);

// The reserve the LSP asks your side of the channel to keep.
pub fn reserve_sats(node: &Node) -> u64 {
	node.list_channels()
		.iter()
		.filter(|c| c.is_usable)
		.filter_map(|c| c.unspendable_punishment_reserve)
		.sum()
}

// What can leave the channels as payment plus fees: the balance minus the
// reserve (and anything in flight).
pub fn sendable_msat(node: &Node) -> u64 {
	node.list_channels()
		.iter()
		.filter(|c| c.is_usable)
		.map(|c| c.next_outbound_htlc_limit_msat)
		.sum()
}

// Node state once it's ready to pay, so a failed send is easier to explain.
fn print_node_status(node: &Node) {
	let status = node.status();
	println!(
		"[node] best_block={} lightning_sync={} onchain_sync={} fee_rates={} rgs_snapshot={}",
		status.current_best_block.height,
		ago(status.latest_lightning_wallet_sync_timestamp),
		ago(status.latest_onchain_wallet_sync_timestamp),
		ago(status.latest_fee_rate_cache_update_timestamp),
		ago(status.latest_rgs_snapshot_timestamp)
	);

	for p in node.list_peers() {
		println!("[peer] {}@{} connected={}", p.node_id, p.address, p.is_connected);
	}

	for c in node.list_channels() {
		let scid = c.short_channel_id.map(|v| v.to_string()).unwrap_or_else(|| "-".into());
		println!(
			"[chan] scid={} usable={} out={} sats in={} sats capacity={} sats max_htlc={} sats",
			scid,
			c.is_usable,
			fmt_sats(c.outbound_capacity_msat / 1000),
			fmt_sats(c.inbound_capacity_msat / 1000),
			fmt_sats(c.channel_value_sats),
			fmt_sats(c.next_outbound_htlc_limit_msat / 1000)
		);
	}

	let graph = node.network_graph();
	println!(
		"[graph] {} channels, {} nodes",
		fmt_sats(graph.list_channels().len() as u64),
		fmt_sats(graph.list_nodes().len() as u64)
	);
}

// The invoice, and whether the graph can reach it: the router needs a path
// from the LSP to the route hint's entry node (or to the payee, if the
// invoice has no route hints).
fn print_invoice(node: &Node, invoice: &Bolt11Invoice, amount_msat: u64) {
	println!(
		"[invoice] amount={} sats payee={} expires_in={}s",
		fmt_sats(amount_msat / 1000),
		invoice.recover_payee_pub_key(),
		invoice.duration_until_expiry().as_secs()
	);
	for hint in invoice.route_hints() {
		for hop in &hint.0 {
			println!(
				"[invoice] route hint: {} scid={} fee={} msat + {} ppm",
				hop.src_node_id,
				hop.short_channel_id,
				hop.fees.base_msat,
				hop.fees.proportional_millionths
			);
		}
	}

	let peers = node.list_peers();
	let graph = node.network_graph();
	let has_hints = !invoice.route_hints().is_empty();
	let describe = |label: &str, pk: PublicKey, ok_if_missing: bool| {
		if peers.iter().any(|p| p.node_id == pk) {
			println!("[graph] {label} {pk}: your peer");
		} else if let Some(n) = graph.node(&NodeId::from_pubkey(&pk)) {
			println!("[graph] {label} {pk}: {} channels", n.channels.len());
		} else if ok_if_missing {
			println!("[graph] {label} {pk}: not in graph (private; reached via route hint)");
		} else {
			println!("[graph] {label} {pk}: NOT in graph, so no route to it");
		}
	};
	describe("payee", invoice.recover_payee_pub_key(), has_hints);
	for hint in invoice.route_hints() {
		if let Some(hop) = hint.0.first() {
			describe("route hint entry", hop.src_node_id, false);
		}
	}
}

// unix timestamp → "12s ago" / "5m ago" / "3h ago"
fn ago(ts: Option<u64>) -> String {
	let Some(ts) = ts else { return "never".into() };
	let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(ts);
	match now.saturating_sub(ts) {
		s if s < 120 => format!("{s}s ago"),
		s if s < 7200 => format!("{}m ago", s / 60),
		s => format!("{}h ago", s / 3600),
	}
}
