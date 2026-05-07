# device-blob

A `no_std` crate for loading device configuration and firmware blobs from flash memory on embedded targets.

## Overview

This crate provides two main capabilities:

1. **Device configuration** — Serialize/deserialize `DeviceConfig` (WiFi credentials + device identifier) to/from a 4 KB flash page, with CRC16 integrity verification.
2. **Firmware blob loading** — Load arbitrary firmware blobs from flash at a given address, verified by a CRC16 checksum.

## Flash Layout

### Device Configuration (`DeviceConfig`)

The configuration occupies exactly one 4 KB (`0x1000`) flash page:

| Offset | Size | Contents |
|--------|------|----------|
| `0x000` | `0x100` | `[u32 SSID length]` + SSID bytes (UTF-8) |
| `0x100` | `0x100` | `[u32 password length]` + password bytes (UTF-8); length `0` = no password |
| `0x200` | `0x100` | `[u32 WiFi mode]` XOR `0xFFFFFFFF` (`0` = AP, `1` = WPA2 Personal) |
| `0x300` | `0x100` | `[u32 ident length]` + device identifier bytes (UTF-8, max 12 chars) |
| `0xFF0` | `8` | CRC16 (XMODEM) of bytes `0x000–0xFEF`, stored as `u64` little-endian |
| `0xFF8` | `8` | Magic bytes: `"CHICKADE"` |

Unused bytes in each block are padded with `0xFF` (erased flash state).

The WiFi mode value is XOR'd with `0xFFFFFFFF` before writing to avoid all-zero bit patterns in flash.

### Firmware Blobs

Firmware blobs are loaded from an arbitrary base address with a caller-supplied size and expected CRC16. The base address **must** be 4-byte aligned.

## API

```rust
// Load device configuration from flash
pub fn load_device_config(base: usize) -> Result<DeviceConfig, &'static str>;

// Serialize configuration to a 4 KB byte array for writing to flash
impl DeviceConfig {
    pub fn to_bytes(&self) -> [u8; BLOCK_SIZE];
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, &'static str>;
}

// Load a firmware blob from flash, verifying its CRC16
pub unsafe fn load_firmware_blob(base: usize, size: usize, crc: u16)
    -> Result<&'static Aligned<A4, [u8]>, u16>;
```

## Features

| Feature | Description |
|---------|-------------|
| `defmt` | Enables [`defmt`](https://defmt.rs/) logging and formatting support |

## Constraints

| Field | Max length |
|-------|-----------|
| SSID | 32 bytes |
| Password | 64 bytes |
| Device identifier (`ident`) | 12 bytes |
