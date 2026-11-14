# mcp25625

Platform-agnostic, `no_std`, async Rust driver for the [Microchip MCP25625](https://www.microchip.com/en-us/product/mcp25625) CAN 2.0B controller with integrated transceiver.

The MCP25625 combines an MCP2515-compatible CAN controller core with an on-die
high-speed CAN transceiver, communicating with the host over SPI. It supports
classic CAN 2.0B (11-bit and 29-bit identifiers, up to 8 data bytes, up to 1 Mbps).

## Design

- `Mcp25625` is the low-level driver: reset, bit-timing/mode configuration,
  filter/mask setup, and single-shot `transmit`/`receive` operations. It owns
  the SPI device, the reset pin, the interrupt pin, and a delay provider, and
  makes no assumption about an async executor.
- `Mcp25625::split` builds a queued front end on top of it using
  `embassy-sync` channels: a cheaply-clonable `Mcp25625Handle` for callers to
  submit frames, await received ones, and request a live filter update
  (`update_filter`, applied without a full reset), and a `Mcp25625Runner`
  whose `run()` method is meant to be spawned as an `embassy-executor` task
  (the same pattern used by `cyw43::Runner` and `embassy_net::Runner`
  elsewhere in this workspace).

## Bit timing

`BitTiming::from_bit_rate(osc_hz, bitrate_bps)` derives CNF1/CNF2/CNF3 register
values for a target bit rate given the oscillator frequency driving OSC1,
targeting an ~87.5% sample point. `BitTiming::new` is available for fully
manual control when a specific sample point or SJW is required.

## Scope

This driver assumes a single owner for the whole peripheral (SPI + IRQ + RST),
matching the dedicated CAN resource set in this workspace's firmware — it is
not designed to share the interrupt line or SPI device across independently
scheduled tasks outside of the provided `Runner`.

## Examples

`examples/` runs this crate's async protocols on a host, against a mock
SPI/GPIO "chip" (`examples/common`) and a virtual CAN bus, with no real
hardware needed:

- `p2p_chat` — two peers exchanging datagrams over `new_p2p`.
- `isotp_transfer` — a payload bigger than one CAN frame over `IsoTpChannel`.
- `timesync` — `TimeSyncMaster`/`TimeSyncFollower`, including a Delay
  Request/Response path-delay measurement.
- `combined_network` — `new_p2p`, `BusArbiter`/`BusArbitrationPeer`, and
  `TimeSyncMaster`/`TimeSyncFollower` all in use at once across a `master`
  and a `listener` node.

Run any of them with (the `defmt` feature needs an embedded logger backend
these std examples don't have, so it must stay off):

```sh
cargo run -p mcp25625 --no-default-features --example p2p_chat
```
