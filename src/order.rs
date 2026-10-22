//! Buy an inbound channel from the LSP with LSPS1, following the five steps
//! of Megalith's guide: read the LSP's options (get_info), connect to its
//! node, place an order (create_order), pay the order's Lightning invoice from
//! any wallet, and poll the order (get_order) until the channel is open.
//!
//! The order is saved to `<STORAGE_DIR>/lsps1_order.json`. If you stop while
//! waiting, running this again picks the same order back up.

use crate::lsps1::{self, GetInfo, Order};
use crate::{channel_size_sats, fmt_sats, lsp_node, lsp_uri, stop, storage_dir};
use ldk_node::lightning_invoice::Bolt11Invoice;
use ldk_node::{ChannelDetails, Node};
use serde_json::Value;
use std::{env, fs, thread, time::Duration};

// bLIP-51: poll get_order every few seconds.
const POLL_SECS: u64 = 5;

const DOCS: &str =
	"https://docs.megalithic.me/lightning-services/lsp1-get-inbound-liquidity-for-mobile-clients/";

// Returns once the node has a usable channel with the LSP. If it already has
// one, nothing is ordered.
pub fn get_channel(node: &Node, log_path: &str) -> ChannelDetails {
	if lsp_channel(node).is_some() {
		println!("You already have a channel with the LSP, so there's nothing to order.");
		println!("Reconnecting to it...");
		return wait_until_usable(node);
	}

	let client = lsps1::Client::from_env();
	println!("This is the main LSPS1 flow: buy an inbound channel from the LSP.");
	println!();
	println!("How it works:");
	println!(
		"  .env           LSPS1_API_URL, LSP_PUBKEY, LSP_ADDRESS, LSP_TOKEN, CHANNEL_SIZE_SATS"
	);
	println!("  src/lsps1.rs   the HTTPS calls: get_info, create_order, get_order");
	println!("  src/order.rs   this flow");
	println!("  Docs           {DOCS}");
	println!();
	println!("  LSPS1 API:  {}", client.base_url());
	println!("  LSP node:   {}", lsp_uri());
	println!("  Your node:  {}", node.node_id());
	println!();
	println!("Logs ➜ {log_path}");

	// ── step 1: get_info ───────────────────────────────────────────────────
	println!();
	println!("Step 1: ask the LSP what it offers");
	println!("  GET {}", client.url("get_info"));
	let info = client.get_info().unwrap_or_else(|e| {
		eprintln!("get_info failed: {e}");
		eprintln!("Check LSPS1_API_URL in .env and your internet connection.");
		stop(node, 1);
	});
	print_info(&info);
	let channel_sats = check_channel_size(node, &info);
	check_lsp_listed(node, &info);

	// ── connect ────────────────────────────────────────────────────────────
	// The LSP only takes orders from nodes connected to it. persist=true
	// makes ldk-node reconnect after restarts, so the channel stays usable.
	println!();
	println!("Connecting to the LSP's node...");
	let (lsp_pubkey, lsp_address) = lsp_node();
	if let Err(e) = node.connect(lsp_pubkey, lsp_address, true) {
		eprintln!("Could not connect to {} ({e}). Details: {log_path}", lsp_uri());
		stop(node, 1);
	}
	println!("  connected");

	// ── step 2: create_order (or pick up the saved one) ────────────────────
	let order = match resume_saved_order(&client) {
		Some(order) => order,
		None => create_order(node, &client, &info, channel_sats),
	};

	// ── steps 3 and 4: show the invoice for the user to pay ────────────────
	let bolt11 = order.bolt11().unwrap_or_else(|e| {
		eprintln!("{e}");
		stop(node, 1);
	});
	if bolt11.state == "EXPECT_PAYMENT" {
		print_invoice(&order);
	}

	// ── step 5: poll get_order until the channel is open ───────────────────
	wait_for_channel(node, &client, order, log_path)
}

// The node's channel with the LSP, if it has one.
pub fn lsp_channel(node: &Node) -> Option<ChannelDetails> {
	let (lsp_pubkey, _) = lsp_node();
	node.list_channels().into_iter().find(|c| c.counterparty_node_id == lsp_pubkey)
}

fn print_info(info: &GetInfo) {
	let (min, max) = info.lsp_balance_range();
	println!("  channel size:     {} to {} sats", fmt_sats(min), fmt_sats(max));
	println!(
		"  longest lease:    {} blocks (~{})",
		fmt_sats(info.max_channel_expiry_blocks as u64),
		blocks_to_time(info.max_channel_expiry_blocks)
	);
	println!(
		"  zero-conf:        {}",
		if info.min_required_channel_confirmations == 0 { "yes" } else { "no" }
	);
	println!(
		"  pushed to you:    {} to {} sats (client_balance_sat)",
		fmt_sats(info.min_initial_client_balance_sat),
		fmt_sats(info.max_initial_client_balance_sat)
	);
	for uri in &info.uris {
		println!("  node:             {uri}");
	}
}

// CHANNEL_SIZE_SATS, checked against get_info's bounds before we POST.
fn check_channel_size(node: &Node, info: &GetInfo) -> u64 {
	let sats = channel_size_sats();
	let (min, max) = info.lsp_balance_range();
	if sats < min || sats > max {
		eprintln!(
			"CHANNEL_SIZE_SATS is {} sats, but the LSP sells {} to {} sats. Change it in .env.",
			fmt_sats(sats),
			fmt_sats(min),
			fmt_sats(max)
		);
		stop(node, 1);
	}
	sats
}

// The node in .env must be one of get_info's `uris`: that's the node the
// order is placed with, and the one trusted for zero-conf in setup_node().
fn check_lsp_listed(node: &Node, info: &GetInfo) {
	let (lsp_pubkey, _) = lsp_node();
	let pubkey = lsp_pubkey.to_string();
	if info.uris.is_empty() || info.uris.iter().any(|u| u.starts_with(&pubkey)) {
		return;
	}
	eprintln!();
	eprintln!("The LSP node in .env ({}) is not in get_info's uris:", lsp_uri());
	for uri in &info.uris {
		eprintln!("  {uri}");
	}
	eprintln!("Update LSP_PUBKEY and LSP_ADDRESS in .env to one of those.");
	stop(node, 1);
}

// ── step 2 ───────────────────────────────────────────────────────────────────

fn create_order(node: &Node, client: &lsps1::Client, info: &GetInfo, channel_sats: u64) -> Order {
	let request = lsps1::CreateOrder {
		lsp_balance_sat: channel_sats.to_string(),
		client_balance_sat: "0".to_string(),
		required_channel_confirmations: info.min_required_channel_confirmations,
		funding_confirms_within_blocks: info.min_funding_confirms_within_blocks,
		channel_expiry_blocks: info.max_channel_expiry_blocks,
		token: env::var("LSP_TOKEN").ok().filter(|t| !t.is_empty()),
		// A private channel: an end-user node doesn't route for others.
		announce_channel: false,
		public_key: node.node_id().to_string(),
	};

	println!();
	println!("Step 2: order a {} sat channel", fmt_sats(channel_sats));
	println!("  POST {}", client.url("create_order"));
	for line in serde_json::to_string_pretty(&request).unwrap().lines() {
		println!("  {line}");
	}

	let order = client.create_order(&request).unwrap_or_else(|e| {
		eprintln!("create_order failed: {e}");
		stop(node, 1);
	});
	save_order(&order);
	println!();
	println!("Step 3: the LSP replies with an order and an invoice");
	print_order(&order);
	check_order(node, &order);
	order
}

// The saved order, if it can still lead to a channel: unpaid with an invoice
// that hasn't expired, or paid and being opened. Otherwise a new one is made.
fn resume_saved_order(client: &lsps1::Client) -> Option<Order> {
	let order_id = saved_order_id()?;
	println!();
	println!("Found your earlier order {order_id}; checking on it.");
	println!("  GET {}?order_id={order_id}", client.url("get_order"));
	let order = match client.get_order(&order_id) {
		Ok(order) => order,
		Err(e) => {
			println!("  couldn't look it up ({e}); ordering a new channel.");
			return None;
		},
	};
	save_order(&order);
	let state = order.bolt11().map(|b| b.state.as_str()).unwrap_or("");
	let resumable = order.order_state == "CREATED"
		&& (state == "HOLD" || (state == "EXPECT_PAYMENT" && !invoice_expired(&order)));
	if !resumable {
		// COMPLETED with no channel on this node means that channel was
		// closed since.
		println!(
			"  it's {} (invoice {}), so it can't give you a new channel; ordering a new one.",
			order.order_state,
			if state == "EXPECT_PAYMENT" { "expired" } else { state }
		);
		return None;
	}
	println!("  picking it up.");
	print_order(&order);
	Some(order)
}

fn print_order(order: &Order) {
	let Ok(b) = order.bolt11() else { return };
	println!("  order_id:       {}", order.order_id);
	println!("  order_state:    {}", order.order_state);
	println!("  channel:        {} sats of inbound liquidity", fmt_sats(order.lsp_balance_sat));
	println!(
		"  lease:          {} blocks (~{}); the LSP won't close it sooner",
		fmt_sats(order.channel_expiry_blocks as u64),
		blocks_to_time(order.channel_expiry_blocks)
	);
	println!("  price:          {} sats (fee_total_sat)", fmt_sats(b.fee_total_sat));
	println!("  to pay:         {} sats (order_total_sat)", fmt_sats(b.order_total_sat));
	println!("  pay by:         {}", b.expires_at);
	println!("  saved to:       {}", saved_order_path());
}

// The checks bLIP-51 says a client SHOULD make before paying.
fn check_order(node: &Node, order: &Order) {
	let b = order.bolt11().unwrap_or_else(|e| {
		eprintln!("{e}");
		stop(node, 1);
	});
	let mut problems = Vec::new();
	if b.fee_total_sat + order.client_balance_sat != b.order_total_sat {
		problems.push("fee_total_sat + client_balance_sat != order_total_sat".to_string());
	}
	match b.invoice.parse::<Bolt11Invoice>() {
		Ok(inv) => {
			if inv.amount_milli_satoshis() != Some(b.order_total_sat * 1000) {
				problems.push(format!(
					"the invoice is for {:?} msat, not order_total_sat",
					inv.amount_milli_satoshis()
				));
			}
			if inv.network() != node.config().network {
				problems.push(format!("the invoice is for {}", inv.network()));
			}
		},
		Err(e) => problems.push(format!("the invoice doesn't parse: {e}")),
	}
	if problems.is_empty() {
		println!("  ✓ checked: price adds up, invoice is for that amount and this network");
		return;
	}
	eprintln!("The order doesn't add up, so don't pay it:");
	for p in problems {
		eprintln!("  - {p}");
	}
	stop(node, 1);
}

fn print_invoice(order: &Order) {
	let Ok(b) = order.bolt11() else { return };
	println!();
	println!("Step 4: pay the invoice");
	println!("  Pay it from any Lightning wallet; it doesn't have to be this node. It's a");
	println!("  hold invoice: the LSP holds your payment until your channel is open, then");
	println!("  settles it. If the channel doesn't open, the payment is refunded, and your");
	println!("  wallet may show it as pending meanwhile. Megalith usually opens within");
	println!("  seconds.");
	println!();
	println!("  Not happy with the price? Don't pay. The invoice expires and nothing happens.");
	println!();
	println!("Keep this running. Once the channel opens, it walks you through receiving and");
	println!("sending a test payment and closing the channel. Ctrl-C stops; running it again");
	println!("picks this order back up.");

	// the invoice goes last so it's still on screen while this waits
	let rule = "─".repeat(78);
	println!();
	println!("{rule}");
	println!(
		"PAY THIS INVOICE from any Lightning wallet ({} sats, pay by {}):",
		fmt_sats(b.order_total_sat),
		b.expires_at
	);
	println!();
	println!("{}", b.invoice);
	println!("{rule}");
	println!();
}

// ── step 5 ───────────────────────────────────────────────────────────────────

fn wait_for_channel(
	node: &Node, client: &lsps1::Client, mut order: Order, log_path: &str,
) -> ChannelDetails {
	println!("Step 5: poll get_order every {POLL_SECS}s until the channel is open");
	println!("  GET {}?order_id={}", client.url("get_order"), order.order_id);
	let mut last_line = String::new();
	let mut last_error = String::new();
	loop {
		match client.get_order(&order.order_id) {
			Ok(o) => {
				if o.json != order.json {
					save_order(&o);
				}
				order = o;
			},
			Err(e) => {
				let e = format!("[wait] couldn't check the order ({e}); retrying");
				if e != last_error {
					eprintln!("{e}");
					last_error = e;
				}
			},
		}
		let state = order.bolt11().map(|b| b.state.clone()).unwrap_or_default();
		let chan = lsp_channel(node);
		let channel = match &chan {
			None => "not yet",
			Some(c) if c.is_usable => "open",
			Some(c) if c.is_channel_ready => "ready, reconnecting",
			Some(_) => "opening",
		};

		// only print when something changes
		let line = format!(
			"[wait] order: {} | invoice: {state}{} | your node's channel: {channel}",
			order.order_state,
			describe_payment(&state)
		);
		if line != last_line {
			println!("{line}");
			last_line = line;
		}

		if let Some(c) = chan.filter(|c| c.is_usable) {
			print_channel(&order, &c);
			return c;
		}
		if order.order_state == "FAILED" || matches!(state.as_str(), "REFUNDED" | "CANCELLED") {
			eprintln!();
			eprintln!(
				"The order {} didn't go through: order_state {}, invoice {state}.",
				order.order_id, order.order_state
			);
			eprintln!("If you paid, the hold invoice is cancelled and your wallet gets the sats");
			eprintln!("back. Run this again to place a new order. Details: {log_path}");
			stop(node, 1);
		}
		if state == "EXPECT_PAYMENT" && invoice_expired(&order) {
			eprintln!("The invoice expired before it was paid. Run this again for a new order.");
			stop(node, 1);
		}
		thread::sleep(Duration::from_secs(POLL_SECS));
	}
}

fn describe_payment(state: &str) -> &'static str {
	match state {
		"EXPECT_PAYMENT" => " (waiting for your payment)",
		"HOLD" => " (payment arrived; the LSP is opening your channel)",
		"PAID" => " (settled)",
		_ => "",
	}
}

fn print_channel(order: &Order, c: &ChannelDetails) {
	println!();
	println!("Your channel is open.");
	if let Some(ch) = &order.channel {
		println!("  funding_outpoint: {}", ch.funding_outpoint);
		println!("  funded_at:        {}", ch.funded_at);
		println!("  lease ends:       {} (the LSP won't close it before then)", ch.expires_at);
	}
	let confs = c.confirmations.unwrap_or(0);
	println!(
		"  capacity:         {} sats{}",
		fmt_sats(c.channel_value_sats),
		if c.channel_value_sats >= order.lsp_balance_sat {
			" (✓ at least what you ordered)"
		} else {
			""
		}
	);
	println!(
		"  confirmations:    {confs} (zero-conf: usable now, the funding tx confirms in the background)"
	);
}

fn wait_until_usable(node: &Node) -> ChannelDetails {
	loop {
		if let Some(c) = lsp_channel(node).filter(|c| c.is_usable) {
			return c;
		}
		thread::sleep(Duration::from_secs(2));
	}
}

fn invoice_expired(order: &Order) -> bool {
	order
		.bolt11()
		.ok()
		.and_then(|b| b.invoice.parse::<Bolt11Invoice>().ok())
		.is_some_and(|inv| inv.is_expired())
}

// 13140 → "3 months" (blocks are ~10 minutes apart)
fn blocks_to_time(blocks: u32) -> String {
	let days = blocks as u64 * 10 / 60 / 24;
	match days {
		0 => format!("{} hours", blocks as u64 * 10 / 60),
		1..=59 => format!("{days} days"),
		_ => format!("{} months", days / 30),
	}
}

// ── saved order ──────────────────────────────────────────────────────────────

pub fn saved_order_path() -> String {
	format!("{}/lsps1_order.json", storage_dir())
}

// The LSP's latest response for the order, as received.
fn save_order(order: &Order) {
	let _ = fs::create_dir_all(storage_dir());
	if let Err(e) =
		fs::write(saved_order_path(), serde_json::to_string_pretty(&order.json).unwrap() + "\n")
	{
		eprintln!("Couldn't save the order to {} ({e}).", saved_order_path());
	}
}

pub fn saved_order_id() -> Option<String> {
	let text = fs::read_to_string(saved_order_path()).ok()?;
	let json: Value = serde_json::from_str(&text).ok()?;
	json.get("order_id")?.as_str().map(str::to_string)
}
