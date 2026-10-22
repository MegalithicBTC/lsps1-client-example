use ldk_node::bitcoin::{secp256k1::PublicKey, Network};
use ldk_node::config::{AnchorChannelsConfig, BackgroundSyncConfig, Config, EsploraSyncConfig};
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::logger::LogLevel;
use ldk_node::{Builder, Node};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use std::{env, process, str::FromStr, sync::Arc, thread};

pub mod close;
pub mod lsps1;
pub mod order;
pub mod pay;
pub mod receive;
pub mod walkthrough;

// Loads `.env`, with `.env.mutinynet` on top when `--mutinynet` is passed.
// dotenvy never overrides a variable that is already set, so the Mutinynet
// file goes first, and real environment variables beat both files.
pub fn load_env() {
	if env::args().any(|a| a == "--mutinynet") {
		dotenvy::from_filename(".env.mutinynet")
			.expect("--mutinynet needs .env.mutinynet; run this from the repo root");
	}
	let _ = dotenvy::dotenv();
}

// Node state, logs and the saved LSPS1 order live here.
pub fn storage_dir() -> String {
	env::var("STORAGE_DIR").unwrap_or_else(|_| "tmp".to_string())
}

// The LSP's Lightning node, from .env.
pub fn lsp_node() -> (PublicKey, SocketAddress) {
	let pubkey = env::var("LSP_PUBKEY").expect("LSP_PUBKEY must be set in .env or environment");
	let address = env::var("LSP_ADDRESS").expect("LSP_ADDRESS must be set in .env or environment");
	(
		PublicKey::from_str(&pubkey).expect("Invalid LSP_PUBKEY format"),
		SocketAddress::from_str(&address).expect("Invalid LSP_ADDRESS format"),
	)
}

// "pubkey@host:port", the form get_info's `uris` use.
pub fn lsp_uri() -> String {
	let (pubkey, address) = lsp_node();
	format!("{pubkey}@{address}")
}

// A command to suggest, e.g. cmd("node_info") → "`cargo run --bin node_info`",
// keeping --mutinynet when this run has it. cmd("") is the main flow.
pub fn cmd(bin: &str) -> String {
	let mut cmd = "cargo run".to_string();
	if !bin.is_empty() {
		cmd += &format!(" --bin {bin}");
	}
	if env::args().any(|a| a == "--mutinynet") {
		cmd += " -- --mutinynet";
	}
	format!("`{cmd}`")
}

// Size of the channel to buy, from .env.
pub fn channel_size_sats() -> u64 {
	env::var("CHANNEL_SIZE_SATS")
		.expect("CHANNEL_SIZE_SATS must be set in .env or environment")
		.parse()
		.expect("Invalid CHANNEL_SIZE_SATS format")
}

// 1234567 → "1,234,567"
pub fn fmt_sats(sats: u64) -> String {
	let digits = sats.to_string();
	let mut out = String::new();
	for (i, c) in digits.chars().enumerate() {
		if i > 0 && (digits.len() - i) % 3 == 0 {
			out.push(',');
		}
		out.push(c);
	}
	out
}

// Builds and starts the node. Logs go to `<STORAGE_DIR>/<log_name>.log`, and
// that path is returned alongside the node.
pub fn setup_node(log_name: &str) -> (Arc<Node>, String) {
	load_env();
	let storage_dir = storage_dir();
	let log_path = format!("{storage_dir}/{log_name}.log");

	// ── base config ────────────────────────────────────────────────────────
	let mut cfg = Config::default();

	// Network configuration from environment
	let network_str = env::var("NETWORK").unwrap_or_else(|_| "bitcoin".to_string());
	cfg.network = match network_str.to_lowercase().as_str() {
		"bitcoin" => Network::Bitcoin,
		"testnet" => Network::Testnet,
		"regtest" => Network::Regtest,
		"signet" => Network::Signet,
		_ => {
			eprintln!("Warning: Unknown network '{}', defaulting to Bitcoin", network_str);
			Network::Bitcoin
		},
	};

	// We speak LSPS1 to the LSP over HTTPS (src/lsps1.rs), not through
	// ldk-node's set_liquidity_source_lsps1(), so we set up the two things
	// that would otherwise do for us:
	let (lsp_pubkey, _) = lsp_node();
	// 1. We order a zero-conf channel (required_channel_confirmations: 0), so
	//    the LSP sends channel_ready before the funding transaction confirms.
	//    LDK only accepts that from peers it trusts.
	cfg.trusted_peers_0conf.push(lsp_pubkey);
	// 2. If the LSP opens an anchor channel, don't require us to hold an
	//    on-chain reserve for it (we start with an empty on-chain wallet).
	let mut anchor_cfg = AnchorChannelsConfig::default();
	anchor_cfg.trusted_peers_no_reserve.push(lsp_pubkey);
	cfg.anchor_channels_config = Some(anchor_cfg);

	// ── builder ────────────────────────────────────────────────────────────
	let mut builder = Builder::from_config(cfg);

	// Log level configuration from environment
	let log_level_str = env::var("LOG_LEVEL").unwrap_or_else(|_| "Debug".to_string());
	let log_level = match log_level_str.to_lowercase().as_str() {
		"trace" => LogLevel::Trace,
		"debug" => LogLevel::Debug,
		"info" => LogLevel::Info,
		"warn" => LogLevel::Warn,
		"error" => LogLevel::Error,
		_ => {
			eprintln!("Warning: Unknown log level '{}', defaulting to Debug", log_level_str);
			LogLevel::Debug
		},
	};

	// Esplora API URL from environment (required)
	let esplora_url =
		env::var("ESPLORA_API_URL").expect("ESPLORA_API_URL must be set in .env or environment");

	// Configure Esplora sync intervals to reduce API rate limiting
	let mut sync_config = EsploraSyncConfig::default();
	sync_config.background_sync_config = Some(BackgroundSyncConfig {
		onchain_wallet_sync_interval_secs: 120, // 2 minutes (default: 60)
		lightning_wallet_sync_interval_secs: 60, // 1 minute (default: 30)
		fee_rate_cache_update_interval_secs: 300, // 5 minutes (default: 60)
	});

	builder
		.set_storage_dir_path(storage_dir)
		.set_filesystem_logger(Some(log_path.clone()), Some(log_level))
		.set_chain_source_esplora(esplora_url, Some(sync_config));

	// Public channel graph, needed to route payments past the LSP. Without it
	// ldk-node falls back to P2P gossip, which takes a long time to fill in.
	if let Ok(rgs_url) = env::var("RGS_SERVER_URL") {
		builder.set_gossip_source_rgs(rgs_url);
	}

	// ── build & start ──────────────────────────────────────────────────────
	let node = Arc::new(builder.build().unwrap());

	// Start node with better error handling for fee estimation timeouts
	if let Err(e) = node.start() {
		eprintln!("WARNING: Node startup encountered an issue: {}", e);
		eprintln!("This is often due to fee estimation timeout from the Esplora API.");
		eprintln!("The node may still be functional for some operations.");
		eprintln!("Consider using a different ESPLORA_API_URL or try again later.");
		eprintln!();
		// Continue anyway - many operations can still work
	}

	// Ctrl-C → clean shutdown (LDK's final persist can take ~15s);
	// a second Ctrl-C quits immediately
	{
		let n = Arc::clone(&node);
		let stopping = AtomicBool::new(false);
		ctrlc::set_handler(move || {
			if stopping.swap(true, Ordering::SeqCst) {
				eprintln!("Force quitting.");
				std::process::exit(130);
			}
			eprintln!("\nShutting down, this can take ~15s. Press Ctrl-C again to force quit.");
			// Stop on another thread so this handler can catch a second Ctrl-C.
			let n = Arc::clone(&n);
			std::thread::spawn(move || {
				let _ = n.stop();
				std::process::exit(0);
			});
		})
		.unwrap();
	}

	(node, log_path)
}

// One manual sync so we start from the current chain state; after that the
// node's background sync keeps balances fresh. The chain source can need a
// few tries right after start.
pub fn sync_wallets(node: &Node) {
	println!("Syncing with the blockchain, please wait...");
	for attempt in 1..=5 {
		match node.sync_wallets() {
			Ok(()) => return,
			Err(_) if attempt < 5 => thread::sleep(Duration::from_secs(2)),
			Err(e) => eprintln!(
				"Couldn't sync with the blockchain ({e}); continuing, but balances may be out of date."
			),
		}
	}
}

// All terminal input comes through one reader thread, so a wait loop can
// notice Enter (see `enter_pressed`) without swallowing the next answer.
fn stdin_lines() -> &'static Mutex<Receiver<String>> {
	static LINES: OnceLock<Mutex<Receiver<String>>> = OnceLock::new();
	LINES.get_or_init(|| {
		let (tx, rx) = mpsc::channel();
		thread::spawn(move || {
			for line in io::stdin().lines() {
				let Ok(line) = line else { break };
				if tx.send(line).is_err() {
					break;
				}
			}
		});
		Mutex::new(rx)
	})
}

// Asks a question and returns the trimmed answer. Returns "" once stdin is
// closed, which callers treat as "no" or "done".
pub fn prompt(question: &str) -> String {
	print!("{question}");
	let _ = io::stdout().flush();
	match stdin_lines().lock().unwrap().recv() {
		Ok(line) => line.trim().to_string(),
		Err(_) => {
			println!();
			String::new()
		},
	}
}

pub fn ask_yes_no(question: &str) -> bool {
	matches!(prompt(&format!("{question} [y/N] ")).to_lowercase().as_str(), "y" | "yes")
}

// True if Enter was pressed since the last call. Call it once before a wait
// to throw away anything typed earlier.
pub fn enter_pressed() -> bool {
	let lines = stdin_lines().lock().unwrap();
	let mut pressed = false;
	while lines.try_recv().is_ok() {
		pressed = true;
	}
	pressed
}

// Stops the node cleanly (LDK's final persist can take ~15s) and exits.
pub fn stop(node: &Node, code: i32) -> ! {
	println!("Shutting down, this can take ~15s.");
	let _ = node.stop();
	process::exit(code);
}
