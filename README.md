# Megalith LSPS1 client example

A small [LDK Node](https://github.com/lightningdevkit/ldk-node) client that buys an inbound
Lightning channel from Megalith's [LSPS1](https://github.com/lightning/blips/blob/master/blip-0051.md)
service. It then walks you through receiving a payment into the channel, sending part of it back
out, and closing it.

Full guide: [LSPS1: Get a Lightning channel opened to your node](https://docs.megalithic.me/lightning-services/lsp1-get-inbound-liquidity-for-mobile-clients/)

## Buy a channel

**This is the command to run:**

```bash
cargo run
```

It follows the steps from Megalith's guide and prints each request as it makes it:

1. **Ask the LSP what it offers.** `GET /get_info` returns the channel sizes and lease lengths
   Megalith sells, and the node URI to connect to.
2. **Connect to the LSP's node.** Megalith only takes orders from nodes that are already
   connected to it.
3. **Order a 250,000 sat channel.** `POST /create_order` returns an order with a price and a
   Lightning invoice. The program checks the price adds up and the invoice is for that amount
   (bLIP-51 says clients should).
4. **You pay the invoice** from any Lightning wallet; it doesn't have to be this node.
5. **Wait for the channel.** The program polls `GET /get_order` every 5 seconds. Megalith opens
   the channel within seconds of the payment arriving.

The whole 250,000 sats is **inbound liquidity**: it sits on Megalith's side of the channel, which
is what lets your node receive payments. You start with nothing on your side.

The price depends on the channel size and on-chain fees, and the program shows it before you pay.
If you don't like it, don't pay: the invoice expires and nothing happens.

The invoice is a **hold invoice**. Megalith holds your payment until the channel is open and
only then settles it. If the channel doesn't open, the payment is cancelled and the sats go back
to your wallet. Your wallet may show the payment as pending for those few seconds.

- Leave the process running while it waits. Press Ctrl-C to stop, and Ctrl-C again to quit
  without waiting for a clean shutdown.
- The order is saved to `tmp/lsps1_order.json`. If you stop before the channel opens, running
  `cargo run` again picks the same order back up instead of placing a new one.

### Try it on Mutinynet first

[Mutinynet](https://github.com/MutinyWallet/mutiny-net) is a test network with 30-second blocks
and free coins. Megalith runs a separate LSPS1 service and node there.

```bash
cargo run -- --mutinynet
```

`--mutinynet` loads `.env.mutinynet` on top of `.env` and keeps node state in `tmp_mutinynet/`,
so mainnet state in `tmp/` is never touched. Every command below takes the flag too, e.g.
`cargo run --bin node_info -- --mutinynet`. You'll need a Mutinynet Lightning wallet to pay the
invoice. The [Mutinynet faucet](https://faucet.mutinynet.com) can pay Lightning invoices once
you sign in.

## After the channel opens

The same process keeps going. **Don't quit it yet:** quitting takes your node offline, and an
offline node can't send or receive payments. It prints your balance and asks three questions.

**1. Would you like to receive a 100 sat test payment into the channel?** We recommend it: this
is what you bought the channel for. It prints an invoice; pay it from another Lightning wallet.
Press Enter to skip.

**2. Would you like to send 50 sats back out of the channel?** Only asked once you have sats on
your side. Make an invoice for **50 sats** in another Lightning wallet and paste it in (an invoice
with no amount works too, and is paid 50 sats). First the program downloads the public channel
graph from a Rapid Gossip Sync server (a few seconds the first time). Without the graph your node
can't find a route past the LSP.

**3. Would you like to close this channel?** Before asking, it explains what closing costs you:

- The channel fee isn't refunded, and you lose the inbound liquidity you paid for. You'd
  normally keep the channel open for the whole lease (13,140 blocks, about 3 months).
- After the test payments you have a little under 50 sats on your side (100 in, 50 out, minus
  the routing fee). That's below the 354 sat dust limit, so the closing transaction has no
  output for you and those sats don't come back on-chain.

If you say yes, it closes the channel cooperatively. If the LSP hasn't agreed within 60 seconds,
it offers a force close, which locks your funds behind a timelock for up to about 2 weeks. If
your side held more than dust, it then waits for the funds to land on-chain (about 10 minutes)
and asks for a Bitcoin address to sweep them to. You can press Ctrl-C while it waits and finish
later with `cargo run --bin close_and_sweep`.

If you say no, the channel stays open and the program exits. Running `cargo run` again while you
have a channel skips the order and goes straight to these questions.

## Commands

| Command                                                   | What it does                                                         |
| --------------------------------------------------------- | -------------------------------------------------------------------- |
| `cargo run`                                               | The guided flow above: buy a channel, receive, send, close           |
| `cargo run --bin get_info`                                | Prints the LSP's `get_info` response and checks `.env` against it (no node needed) |
| `cargo run --bin order_status [order_id]`                 | Prints the saved order (or the one given) from `get_order` (no node needed) |
| `cargo run --bin node_info`                               | Public key, balances, peers, channels, recent payments, saved order  |
| `cargo run --bin make_invoice [sats]`                     | Makes an invoice to receive into the channel (default 100 sats) and waits for it |
| `cargo run --bin pay_invoice`                             | Shows your balance, then pays invoices you paste in                  |
| `cargo run --bin sweep_onchain [address] [--no-reserves]` | Sweeps on-chain funds to an address (asks for one if not given)      |
| `cargo run --bin close_and_sweep`                         | Closes the channel, waits for the funds to land on-chain, then sweeps them to an address you give |

Add `-- --mutinynet` to any of them for Mutinynet, for example
`cargo run --bin make_invoice -- --mutinynet 1000`. Node state, logs and the saved order are in
`tmp/` (`tmp_mutinynet/` on Mutinynet). Run one node command at a time; they share that
directory.

## How it works

### LSPS1 over HTTPS

bLIP-51 is written for [BOLT8](https://github.com/lightning/bolts/blob/master/08-transport.md)
peer messages, and that is what ldk-node's built-in client (`node.lsps1_liquidity()`) speaks.
Megalith takes the same requests and returns the same JSON over plain HTTPS for now. So this
example doesn't use ldk-node's LSPS1 client. `src/lsps1.rs` makes the three calls itself with
`reqwest`:

```bash
# 1. What the LSP sells
curl https://megalithic.me/api/lsps1/v1/get_info

# 2. Place an order (your node must already be connected to the LSP's node)
curl -X POST https://megalithic.me/api/lsps1/v1/create_order \
  -H 'Content-Type: application/json' \
  -d '{
    "lsp_balance_sat": "250000",
    "client_balance_sat": "0",
    "required_channel_confirmations": 0,
    "funding_confirms_within_blocks": 6,
    "channel_expiry_blocks": 13140,
    "token": "lsps1-client-example",
    "announce_channel": false,
    "public_key": "<your node id>"
  }'

# 3. Check on it
curl 'https://megalithic.me/api/lsps1/v1/get_order?order_id=<order_id>'
```

Megalith's API differs from bLIP-51 in a few ways:

- **`uris` in `get_info` and `public_key` in `create_order`.** Over BOLT8 the LSP knows who's
  asking from the connection. Over HTTP it doesn't, so `uris` tells you where to connect and
  `public_key` tells Megalith which node to open the channel to.
- **Lightning payments only.** There is no `refund_onchain_address` in the order, and the
  client only uses `payment.bolt11`.
- **Errors are JSON-RPC error objects** in the response body, e.g.
  `{"error":{"code":100,"message":"Option mismatch","data":{"property":"lsp_balance_sat",...}}}`.
  Check the body for `error` whatever the HTTP status: "order not found" comes back with HTTP
  200.

Amounts (`*_sat`) are JSON strings, as LSPS0 specifies.

### Order states

| `order_state` | `payment.bolt11.state` | Meaning                                                         |
| ------------- | ---------------------- | --------------------------------------------------------------- |
| `CREATED`     | `EXPECT_PAYMENT`       | Waiting for you to pay the invoice                              |
| `CREATED`     | `HOLD`                 | Payment arrived; Megalith holds it while it opens your channel  |
| `COMPLETED`   | `PAID`                 | Funding transaction published, payment settled                  |
| `FAILED`      | `REFUNDED`             | The channel didn't open; the held payment was cancelled         |

`channel` in the order stays `null` until the funding transaction is published. Then it gives
`funding_outpoint`, `funded_at`, and `expires_at`, the earliest time Megalith may close the
channel.

### What ldk-node needs

ldk-node's `set_liquidity_source_lsps1()` would configure two things for you. Since this example
talks to the LSP over HTTPS instead, `setup_node()` in `src/lib.rs` sets them itself:

```rust
let lsp: PublicKey = "038a9e56512ec98da2b5789761f7af8f280baf98a09282360cd6ff1381b5e889bf".parse().unwrap();

// We order a zero-conf channel (required_channel_confirmations: 0), so the LSP
// sends channel_ready before the funding transaction confirms. LDK only
// accepts that from peers it trusts.
cfg.trusted_peers_0conf.push(lsp);

// Megalith's node supports anchor channels. For an inbound anchor channel LDK
// wants an on-chain reserve (25,000 sats by default) to bump fees with, and a
// new node's on-chain wallet is empty, so it would reject the channel.
let mut anchor_cfg = AnchorChannelsConfig::default();
anchor_cfg.trusted_peers_no_reserve.push(lsp);
cfg.anchor_channels_config = Some(anchor_cfg);
```

Then, before `create_order`, connect with `persist = true`, so ldk-node reconnects after
restarts and the channel stays usable:

```rust
node.connect(lsp, "64.23.162.51:9735".parse().unwrap(), true)?;
```

That's all LDK needs. When Megalith opens the channel, ldk-node accepts it as zero-conf and it's
usable at once.

### Code map

| File                  | What's in it                                                               |
| --------------------- | -------------------------------------------------------------------------- |
| `src/lsps1.rs`        | The LSPS1 HTTPS client: `get_info`, `create_order`, `get_order`, and the bLIP-51 types |
| `src/order.rs`        | Steps 1–5: buy the channel, save and resume the order                      |
| `src/lib.rs`          | `setup_node()` and config loading                                          |
| `src/receive.rs`      | Make an invoice and wait for it to be paid                                 |
| `src/pay.rs`          | Pay invoices, with diagnostics for failed sends                            |
| `src/close.rs`        | Close the channel and sweep the funds on-chain                             |
| `src/walkthrough.rs`  | The guided flow behind `cargo run`                                         |
| `src/bin/*.rs`        | The commands above                                                         |

## Configuration

Settings live in the committed `.env` (nothing in it is secret). `.env.mutinynet` overrides it
when you pass `--mutinynet`, and real environment variables override both:

| Variable            | Description                                                     | Mainnet value                    |
| ------------------- | --------------------------------------------------------------- | -------------------------------- |
| `LSPS1_API_URL`     | Megalith's LSPS1 API                                            | `https://megalithic.me/api/lsps1/v1` |
| `LSP_PUBKEY`        | LSP node ID, from `get_info`'s `uris`                           | `038a9e56…5e889bf`               |
| `LSP_ADDRESS`       | LSP host and port, from `get_info`'s `uris`                     | `64.23.162.51:9735`              |
| `LSP_TOKEN`         | Sent as the order's `token` (any non-empty string works)        | `lsps1-client-example`           |
| `CHANNEL_SIZE_SATS` | Channel to buy, within `get_info`'s bounds                      | `250000`                         |
| `NETWORK`           | Must be `bitcoin` for the mainnet LSP                           | `bitcoin`                        |
| `ESPLORA_API_URL`   | Esplora API endpoint                                            | `https://blockstream.info/api`   |
| `RGS_SERVER_URL`    | Rapid Gossip Sync server for the channel graph (needed to pay)  | `https://rapidsync.lightningdevkit.org/snapshot` |
| `STORAGE_DIR`       | Node state, logs and the saved order                            | `tmp`                            |
| `LOG_LEVEL`         | Trace, Debug, Info, Warn, Error                                 | `Debug`                          |

The program checks that the node in `.env` is still listed in `get_info`'s `uris` before it
orders, and that `CHANNEL_SIZE_SATS` is within the sizes Megalith sells. If Megalith moves its
node, it tells you what to change. `cargo run --bin get_info` does the same checks without
starting a node.

## Troubleshooting

| Message                                                    | What to do                                                   |
| ---------------------------------------------------------- | ------------------------------------------------------------ |
| `Client rejected (code 1): Client node not connected to LSP node` | The connect step failed or dropped. Check `LSP_PUBKEY`/`LSP_ADDRESS` against `cargo run --bin get_info`. |
| `Option mismatch (code 100), property lsp_balance_sat`     | `CHANNEL_SIZE_SATS` is outside what Megalith sells.          |
| `The invoice expired before it was paid`                   | Run `cargo run` again for a new order.                       |
| `The order … didn't go through`                            | Megalith couldn't open the channel. A held payment is cancelled automatically. Run again to place a new order. |
| A send fails with no route                                 | Routing fees go on top of the amount. Try a smaller amount, and check the `[graph]` lines it prints. |

Debug logs are in `tmp/ldk_node.log` (each command writes its own log there).
