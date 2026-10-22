//! A small LSPS1 (bLIP-51) client that talks to the LSP over HTTPS.
//!
//! bLIP-51 is written for BOLT8 peer messages, and that is what ldk-node's
//! `node.lsps1_liquidity()` speaks. Megalith takes the same requests and
//! returns the same responses as plain JSON over HTTPS instead, so this module
//! makes the three calls itself:
//!
//!   GET  {LSPS1_API_URL}/get_info
//!   POST {LSPS1_API_URL}/create_order
//!   GET  {LSPS1_API_URL}/get_order?order_id=…
//!
//! Over HTTP the LSP can't tell which node is asking, so Megalith adds two
//! fields to the spec: `uris` in get_info (where to connect) and `public_key`
//! in create_order (which node to open the channel to).
//!
//! Megalith takes Lightning (BOLT11) payments only, so there is no
//! `refund_onchain_address` in the order and the `onchain` payment option is
//! ignored.
//!
//! Spec: https://github.com/lightning/blips/blob/master/blip-0051.md

use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::{env, fmt, time::Duration};

pub struct Client {
	base_url: String,
	http: reqwest::blocking::Client,
}

impl Client {
	pub fn from_env() -> Self {
		let base_url =
			env::var("LSPS1_API_URL").expect("LSPS1_API_URL must be set in .env or environment");
		let http = reqwest::blocking::Client::builder()
			.timeout(Duration::from_secs(30))
			.user_agent(concat!("megalith-lsps1-client-example/", env!("CARGO_PKG_VERSION")))
			.build()
			.expect("could not build the HTTP client");
		Self { base_url: base_url.trim_end_matches('/').to_string(), http }
	}

	pub fn base_url(&self) -> &str {
		&self.base_url
	}

	pub fn url(&self, method: &str) -> String {
		format!("{}/{method}", self.base_url)
	}

	// lsps1.get_info: the LSP's limits. bLIP-51 says to call it first.
	pub fn get_info(&self) -> Result<GetInfo, Error> {
		let json = send(self.http.get(self.url("get_info")))?;
		Ok(GetInfo { json: json.clone(), ..parse(json)? })
	}

	// lsps1.create_order: not idempotent; every call makes a new order.
	pub fn create_order(&self, request: &CreateOrder) -> Result<Order, Error> {
		let json = send(self.http.post(self.url("create_order")).json(request))?;
		Ok(Order { json: json.clone(), ..parse(json)? })
	}

	// lsps1.get_order: the order's current state; poll this while you wait.
	pub fn get_order(&self, order_id: &str) -> Result<Order, Error> {
		let req = self.http.get(self.url("get_order")).query(&[("order_id", order_id)]);
		let json = send(req)?;
		Ok(Order { json: json.clone(), ..parse(json)? })
	}
}

// ── get_info ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct GetInfo {
	// 0 means the LSP can open zero-conf channels.
	pub min_required_channel_confirmations: u16,
	pub min_funding_confirms_within_blocks: u16,
	#[serde(default)]
	pub supports_zero_channel_reserve: bool,
	// Longest lease the LSP will sell, in blocks.
	pub max_channel_expiry_blocks: u32,
	#[serde(deserialize_with = "sat")]
	pub min_initial_client_balance_sat: u64,
	#[serde(deserialize_with = "sat")]
	pub max_initial_client_balance_sat: u64,
	#[serde(deserialize_with = "sat")]
	pub min_initial_lsp_balance_sat: u64,
	#[serde(deserialize_with = "sat")]
	pub max_initial_lsp_balance_sat: u64,
	#[serde(deserialize_with = "sat")]
	pub min_channel_balance_sat: u64,
	#[serde(deserialize_with = "sat")]
	pub max_channel_balance_sat: u64,
	// Megalith extension: the LSP's node, as "pubkey@host:port".
	#[serde(default)]
	pub uris: Vec<String>,
	// The response as received.
	#[serde(skip)]
	pub json: Value,
}

impl GetInfo {
	// The smallest and largest `lsp_balance_sat` we can order. With no client
	// balance, the channel size is the LSP balance, so both pairs of bounds
	// apply.
	pub fn lsp_balance_range(&self) -> (u64, u64) {
		(
			self.min_initial_lsp_balance_sat.max(self.min_channel_balance_sat),
			self.max_initial_lsp_balance_sat.min(self.max_channel_balance_sat),
		)
	}
}

// ── create_order ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct CreateOrder {
	// Channel size: what the LSP puts on its side, which is your inbound
	// liquidity. LSPS0.sat values go over the wire as strings.
	pub lsp_balance_sat: String,
	// What the LSP pushes to your side. Megalith only allows "0".
	pub client_balance_sat: String,
	// 0 asks for a zero-conf channel, usable before the funding tx confirms.
	pub required_channel_confirmations: u16,
	pub funding_confirms_within_blocks: u16,
	// How long the LSP promises to keep the channel open.
	pub channel_expiry_blocks: u32,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub token: Option<String>,
	pub announce_channel: bool,
	// Megalith extension: the node to open the channel to. It must already be
	// connected to the LSP's node.
	pub public_key: String,
}

// ── orders (create_order and get_order responses) ───────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Order {
	pub order_id: String,
	#[serde(deserialize_with = "sat")]
	pub lsp_balance_sat: u64,
	#[serde(deserialize_with = "sat")]
	pub client_balance_sat: u64,
	pub channel_expiry_blocks: u32,
	#[serde(default)]
	pub created_at: String,
	// CREATED, then COMPLETED once the LSP publishes the funding transaction,
	// or FAILED.
	pub order_state: String,
	pub payment: Payment,
	// Null until the funding transaction is published.
	pub channel: Option<OrderChannel>,
	// The response as received.
	#[serde(skip)]
	pub json: Value,
}

// bLIP-51 says to ignore payment options you don't use: `onchain` (Megalith
// doesn't take on-chain payments) and `bolt12` are left out here.
#[derive(Debug, Clone, Deserialize)]
pub struct Payment {
	pub bolt11: Option<Bolt11Payment>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Bolt11Payment {
	// EXPECT_PAYMENT → HOLD (payment arrived, held until the channel opens)
	// → PAID. REFUNDED (or the older CANCELLED) if the channel didn't open.
	pub state: String,
	pub expires_at: String,
	#[serde(deserialize_with = "sat")]
	pub fee_total_sat: u64,
	#[serde(deserialize_with = "sat")]
	pub order_total_sat: u64,
	// A hold invoice for order_total_sat.
	pub invoice: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderChannel {
	pub funded_at: String,
	pub funding_outpoint: String,
	// Earliest time the LSP may close the channel.
	pub expires_at: String,
}

impl Order {
	pub fn bolt11(&self) -> Result<&Bolt11Payment, Error> {
		self.payment.bolt11.as_ref().ok_or_else(|| {
			Error::Parse("the order has no Lightning (bolt11) payment option".to_string())
		})
	}
}

// ── errors ───────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum Error {
	// Couldn't reach the API, or the reply wasn't JSON.
	Http(String),
	// The LSP answered with a JSON-RPC error object (bLIP-50 error handling),
	// e.g. code 100 "Option mismatch" with data.property = "lsp_balance_sat".
	Lsp { code: i64, message: String, data: Value },
	// The JSON didn't have the shape bLIP-51 describes.
	Parse(String),
}

impl fmt::Display for Error {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		match self {
			Error::Http(e) => write!(f, "{e}"),
			Error::Parse(e) => write!(f, "unexpected response: {e}"),
			Error::Lsp { code, message, data } => {
				write!(f, "{message} (code {code})")?;
				if let Some(property) = data.get("property").and_then(Value::as_str) {
					write!(f, ", property {property}")?;
				}
				if let Some(detail) = data.get("message").and_then(Value::as_str) {
					write!(f, ": {detail}")?;
				}
				Ok(())
			},
		}
	}
}

// Sends the request and returns the JSON body. An `error` object in the body
// is an error whatever the HTTP status (Megalith answers "order not found"
// with 200).
fn send(req: reqwest::blocking::RequestBuilder) -> Result<Value, Error> {
	let resp = req.send().map_err(|e| Error::Http(e.to_string()))?;
	let status = resp.status();
	let body = resp.text().map_err(|e| Error::Http(e.to_string()))?;
	let json: Value = serde_json::from_str(&body)
		.map_err(|_| Error::Http(format!("HTTP {status}, not JSON: {}", body.trim())))?;
	if let Some(err) = json.get("error") {
		return Err(Error::Lsp {
			code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
			message: err.get("message").and_then(Value::as_str).unwrap_or("error").to_string(),
			data: err.get("data").cloned().unwrap_or(Value::Null),
		});
	}
	if !status.is_success() {
		return Err(Error::Http(format!("HTTP {status}: {}", body.trim())));
	}
	Ok(json)
}

fn parse<T: DeserializeOwned>(json: Value) -> Result<T, Error> {
	serde_json::from_value(json).map_err(|e| Error::Parse(e.to_string()))
}

// LSPS0.sat amounts are JSON strings ("250000"); accept plain numbers too.
fn sat<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
	#[derive(Deserialize)]
	#[serde(untagged)]
	enum Sat {
		Str(String),
		Num(u64),
	}
	match Sat::deserialize(d)? {
		Sat::Str(s) => s.parse().map_err(|_| D::Error::custom(format!("bad sat amount {s:?}"))),
		Sat::Num(n) => Ok(n),
	}
}
