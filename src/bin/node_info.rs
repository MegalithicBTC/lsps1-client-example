//! Public key, balances, peers, channels, recent payments, and the saved
//! LSPS1 order.

use megalith_lsps1::{cmd, order, setup_node, stop, sync_wallets};

fn main() {
	let (node, log_path) = setup_node("node_info");

	sync_wallets(&node);

	let config = node.config();
	let status = node.status();
	let balances = node.list_balances();
	let peers = node.list_peers();
	let channels = node.list_channels();
	let mut payments = node.list_payments();
	payments.sort_by(|a, b| b.latest_update_timestamp.cmp(&a.latest_update_timestamp));

	println!("Node");
	println!("  Public key:        {}", node.node_id());
	println!("  Network:           {:?}", config.network);
	println!("  Running:           {}", status.is_running);
	println!(
		"  Best block:        {} at height {}",
		status.current_best_block.block_hash, status.current_best_block.height
	);
	println!("  Lightning sync:    {}", fmt_opt(status.latest_lightning_wallet_sync_timestamp));
	println!("  On-chain sync:     {}", fmt_opt(status.latest_onchain_wallet_sync_timestamp));
	println!("  Fee cache update:  {}", fmt_opt(status.latest_fee_rate_cache_update_timestamp));
	println!();

	println!("Balances");
	println!("  On-chain total:    {} sats", balances.total_onchain_balance_sats);
	println!("  On-chain spendable:{} sats", balances.spendable_onchain_balance_sats);
	println!("  Anchor reserve:    {} sats", balances.total_anchor_channels_reserve_sats);
	println!("  Lightning total:   {} sats", balances.total_lightning_balance_sats);
	println!("  Lightning entries: {}", balances.lightning_balances.len());
	println!("  Pending sweeps:    {}", balances.pending_balances_from_channel_closures.len());
	println!();

	println!("Peers ({})", peers.len());
	if peers.is_empty() {
		println!("  none");
	} else {
		for peer in peers {
			println!(
				"  {} addr={:?} connected={} persisted={}",
				peer.node_id, peer.address, peer.is_connected, peer.is_persisted
			);
		}
	}
	println!();

	let total_outbound_msat = channels.iter().map(|c| c.outbound_capacity_msat).sum::<u64>();
	let total_inbound_msat = channels.iter().map(|c| c.inbound_capacity_msat).sum::<u64>();
	let ready_channels = channels.iter().filter(|c| c.is_channel_ready).count();
	let usable_channels = channels.iter().filter(|c| c.is_usable).count();

	println!("Channels ({})", channels.len());
	println!("  Ready:             {ready_channels}");
	println!("  Usable:            {usable_channels}");
	println!("  Total outbound:    {total_outbound_msat} msat");
	println!("  Total inbound:     {total_inbound_msat} msat");

	if channels.is_empty() {
		println!("  none");
	} else {
		for channel in channels {
			println!("  - channel_id={:?}", channel.channel_id);
			println!("    counterparty={}", channel.counterparty_node_id);
			println!("    funding_txo={}", fmt_opt(channel.funding_txo));
			println!(
				"    scid={} inbound_alias={} outbound_alias={}",
				fmt_opt(channel.short_channel_id),
				fmt_opt(channel.inbound_scid_alias),
				fmt_opt(channel.outbound_scid_alias)
			);
			println!(
				"    ready={} usable={} announced={} outbound={}",
				channel.is_channel_ready,
				channel.is_usable,
				channel.is_announced,
				channel.is_outbound
			);
			println!(
				"    capacity={} sats outbound={} msat inbound={} msat",
				channel.channel_value_sats,
				channel.outbound_capacity_msat,
				channel.inbound_capacity_msat
			);
			println!(
				"    next_outbound_htlc_min={} msat next_outbound_htlc_limit={} msat",
				channel.next_outbound_htlc_minimum_msat, channel.next_outbound_htlc_limit_msat
			);
			println!(
				"    confirmations={}/{} feerate={} sat/kw",
				fmt_opt(channel.confirmations),
				fmt_opt(channel.confirmations_required),
				channel.feerate_sat_per_1000_weight
			);
		}
	}
	println!();

	println!("Recent payments ({}, showing up to 10)", payments.len());
	if payments.is_empty() {
		println!("  none");
	} else {
		for payment in payments.iter().take(10) {
			println!(
				"  id={:?} direction={:?} status={:?} amount={} fee={} updated={} kind={:?}",
				payment.id,
				payment.direction,
				payment.status,
				fmt_opt(payment.amount_msat),
				fmt_opt(payment.fee_paid_msat),
				payment.latest_update_timestamp,
				payment.kind
			);
		}
	}
	println!();

	println!("LSPS1 order");
	match order::saved_order_id() {
		Some(id) => {
			println!("  {id}");
			println!(
				"  saved in {} (check it with {})",
				order::saved_order_path(),
				cmd("order_status")
			);
		},
		None => println!("  none saved"),
	}
	println!();
	println!("Logs -> {log_path}");

	stop(&node, 0);
}

fn fmt_opt<T: std::fmt::Display>(value: Option<T>) -> String {
	value.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string())
}
