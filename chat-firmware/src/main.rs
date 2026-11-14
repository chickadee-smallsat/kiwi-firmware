#![no_std]
#![no_main]
//! Simplified chat firmware: defmt logging over one USB CDC-ACM interface,
//! and an interactive login + chat terminal over a second one. After typing
//! a name at the login prompt, anything typed is broadcast as a chat
//! message to every other node on the CAN bus (via `mcp25625`'s p2p
//! protocol), and incoming chat messages from other nodes are printed as
//! they arrive.
//!
//! Deliberately single-core and without the main `kiwi-firmware-base`
//! firmware's sensors/Wi-Fi/SD card/watchdog — this exists to demonstrate
//! the USB + CAN stack on their own, as a separate, standalone binary.

use core::fmt::Write as _;

use defmt::info;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_time::Timer;
use heapless::String;
use panic_probe as _;

mod canbus_chat;
mod resources;
mod usb_chat;

use resources::{AssignedResources, CanDev, UsbConfDev};
use usb_chat::{ChatReceiver, ChatSender};

/// Longest name a user can log in with.
const NAME_MAX: usize = 16;
/// Longest chat message a user can type in one line.
const MSG_MAX: usize = 200;
/// `"<name>: <message>"`, comfortably within `mcp25625::MAX_P2P_MSG_LEN` (256).
const BROADCAST_MAX: usize = NAME_MAX + MSG_MAX + 4;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let resources = split_resources!(p);

    // Give an attached debug probe time to parse the defmt RTT header before
    // any flash access — reading that header might touch flash memory,
    // which interferes with flash reads. Same reasoning as the main
    // firmware's `main`. https://github.com/knurling-rs/defmt/pull/683
    Timer::after_millis(10).await;

    let (usb_tx, usb_rx) = usb_chat::build(spawner, resources.usbconfig);

    // Add a delay to ensure USB has enumerated before relying on it below,
    // mirroring the main firmware's `main`.
    Timer::after_millis(1000).await;

    info!(
        "Kiwi Chat Firmware v{} starting up...",
        env!("CARGO_PKG_VERSION")
    );

    let (can_tx, can_rx) = canbus_chat::init(spawner, resources.can).await;

    spawner.spawn(defmt::unwrap!(chat_task(usb_tx, usb_rx, can_tx, can_rx)));
}

#[embassy_executor::task]
async fn chat_task(
    mut tx: ChatSender,
    mut rx: ChatReceiver,
    can_tx: canbus_chat::CanSender,
    mut can_rx: canbus_chat::CanReceiver,
) -> ! {
    loop {
        rx.wait_connection().await;
        write_message(
            &mut tx,
            b"\r\nWelcome to Kiwi Chat!\r\nLogin - enter your name: ",
        )
        .await;

        let mut name = String::<NAME_MAX>::new();
        if read_line(&mut rx, &mut tx, &mut name).await.is_none() {
            continue; // disconnected before logging in; wait for reconnect
        }

        write_message(&mut tx, b"Hi, ").await;
        write_message(&mut tx, name.as_bytes()).await;
        write_message(
            &mut tx,
            b"! Type a message and press enter to send it to the bus.\r\n> ",
        )
        .await;

        let mut msg = String::<MSG_MAX>::new();
        let mut data = [0u8; 64];
        loop {
            match select(rx.read_packet(&mut data), can_rx.receive()).await {
                Either::First(Ok(n)) => {
                    let Ok(s) = core::str::from_utf8(&data[..n]) else {
                        continue;
                    };
                    if feed_line(&mut tx, &mut msg, s).await {
                        let mut line = String::<BROADCAST_MAX>::new();
                        let _ = write!(line, "{}: {}", name, msg);
                        if can_tx.broadcast(line.as_bytes()).await.is_err() {
                            write_message(&mut tx, b"Error: message too long, not sent.\r\n").await;
                        }
                        msg.clear();
                        write_message(&mut tx, b"> ").await;
                    }
                }
                Either::First(Err(_)) => break, // disconnected; back to login
                Either::Second((_src, _channel, payload)) => {
                    if let Ok(text) = core::str::from_utf8(&payload) {
                        write_message(&mut tx, b"\r\n").await;
                        write_message(&mut tx, text.as_bytes()).await;
                        write_message(&mut tx, b"\r\n> ").await;
                        write_message(&mut tx, msg.as_bytes()).await; // redraw in-progress input
                    }
                }
            }
        }
    }
}

/// Writes `s` to the chat terminal, chunked to the same packet size the
/// main firmware's USB comm task uses.
async fn write_message(tx: &mut ChatSender, s: &[u8]) {
    for chunk in s.chunks(32) {
        let _ = tx.write_packet(chunk).await;
    }
}

/// Feeds one just-read, UTF-8-decoded USB packet's worth of characters
/// (`input`) into `msg`, echoing keystrokes back (with backspace handling
/// and a rudimentary ANSI escape skip) via `tx`. Returns `true` once `msg`
/// holds a complete non-empty line ready for the caller to consume (on
/// `\r`/`\n`), `false` if still accumulating.
async fn feed_line<const N: usize>(tx: &mut ChatSender, msg: &mut String<N>, input: &str) -> bool {
    let mut skip = 0u8;
    for c in input.chars() {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        if c.is_control() {
            if c == '\x08' || c == '\x7f' {
                // backspace or delete
                if msg.pop().is_some() {
                    write_message(tx, b"\x08 \x08").await;
                }
            } else if c == '\n' || c == '\r' {
                if !msg.is_empty() {
                    write_message(tx, b"\r\n").await;
                    return true;
                }
            } else if c == '\x1b' {
                skip = 2; // rudimentary way to skip ANSI escape sequences
            }
        } else if msg.push(c).is_err() {
            write_message(tx, b"\r\nError: message too long, discarding.\r\n").await;
            msg.clear();
        } else {
            let mut buf = [0u8; 4];
            write_message(tx, c.encode_utf8(&mut buf).as_bytes()).await;
        }
    }
    false
}

/// Reads and echoes characters into `msg` until a complete line is entered,
/// then returns `Some(())`. Returns `None` if the connection drops first.
async fn read_line<const N: usize>(
    rx: &mut ChatReceiver,
    tx: &mut ChatSender,
    msg: &mut String<N>,
) -> Option<()> {
    let mut data = [0u8; 64];
    loop {
        let n = rx.read_packet(&mut data).await.ok()?;
        let Ok(s) = core::str::from_utf8(&data[..n]) else {
            continue;
        };
        if feed_line(tx, msg, s).await {
            return Some(());
        }
    }
}
