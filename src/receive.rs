//! Receive a payment into the channel: make an invoice and wait for it to be
//! paid. This is what the inbound liquidity you bought is for.

use crate::{enter_pressed, fmt_sats, order::lsp_channel};
use ldk_node::bitcoin::hashes::Hash;
use ldk_node::lightning::ln::channelmanager::PaymentId;
use ldk_node::lightning_invoice::{Bolt11InvoiceDescription, Description};
use ldk_node::payment::PaymentStatus;
use ldk_node::Node;
use std::thread;
use std::time::{Duration, Instant};

const INVOICE_EXPIRY_SECS: u32 = 3_600;

// Makes an invoice for `amount_sats`, prints it, and waits until it's paid
// (true), or it expires, fails, or Enter is pressed (false).
pub fn receive(node: &Node, amount_sats: u64, log_path: &str) -> bool {
	wait_for_route_hint(node);

	let desc =
		Bolt11InvoiceDescription::Direct(Description::new("megalith-lsps1-test".into()).unwrap());
	let invoice =
		match node.bolt11_payment().receive(amount_sats * 1_000, &desc, INVOICE_EXPIRY_SECS) {
			Ok(invoice) => invoice,
			Err(e) => {
				eprintln!("Couldn't make an invoice: {e}. Details: {log_path}");
				return false;
			},
		};
	let payment_id = PaymentId(invoice.payment_hash().to_byte_array());

	let rule = "─".repeat(78);
	println!();
	println!("{rule}");
	println!(
		"PAY THIS INVOICE from another Lightning wallet ({} sats, expires in 1 hour):",
		fmt_sats(amount_sats)
	);
	println!();
	println!("{invoice}");
	println!("{rule}");
	println!();
	println!("Waiting for the payment. Press Enter to skip.");

	enter_pressed(); // throw away anything typed earlier
	loop {
		match node.payment(&payment_id).map(|p| p.status) {
			Some(PaymentStatus::Succeeded) => {
				println!("RECEIVED {} sats.", fmt_sats(amount_sats));
				return true;
			},
			Some(PaymentStatus::Failed) => {
				eprintln!("The payment failed. Details: {log_path}");
				return false;
			},
			_ => {},
		}
		if invoice.is_expired() {
			eprintln!("The invoice expired before it was paid.");
			return false;
		}
		if enter_pressed() {
			println!("Skipped.");
			return false;
		}
		thread::sleep(Duration::from_secs(1));
	}
}

// The channel is private, so payers find your node through a route hint in
// the invoice. LDK can only add one once the LSP has sent its channel_update
// (its fees for forwarding to you), which comes just after the channel opens.
fn wait_for_route_hint(node: &Node) {
	let start = Instant::now();
	while start.elapsed() < Duration::from_secs(30) {
		let ready = lsp_channel(node)
			.is_some_and(|c| c.is_usable && c.counterparty_forwarding_info_fee_base_msat.is_some());
		if ready {
			return;
		}
		thread::sleep(Duration::from_secs(1));
	}
	eprintln!("The LSP hasn't sent its channel_update yet, so the invoice may have no route hint");
	eprintln!("and payers may not find your node. If the payment doesn't arrive, try again.");
}
