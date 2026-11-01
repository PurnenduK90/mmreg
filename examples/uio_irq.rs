//! Interrupt loop on a real UIO device (needs the hardware; see `mock_quickstart.rs` otherwise).
//!
//! ```sh
//! cargo run --example uio_irq -- <uio name> [count]    # e.g. `axi_timer 10`
//! ```
//!
//! The peripheral is assumed to have an `ISR` register at 0x0C with write-1-to-clear `DONE`
//! (bit 0) and `ERR` (bit 1) bits; adapt the constants to your IP.

use mmreg::{Device, MmregError};
use std::time::Duration;

const ISR: u32 = 0x0C;
const ISR_DONE: u32 = 1 << 0;
const ISR_ERR: u32 = 1 << 1;
const ISR_W1C_MASK: u32 = ISR_DONE | ISR_ERR;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(name) = args.next() else {
        eprintln!("usage: uio_irq <uio name> [count]");
        std::process::exit(2);
    };
    let count = args.next().and_then(|c| c.parse().ok()).unwrap_or(5u32);
    if let Err(err) = run(&name, count) {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run(name: &str, count: u32) -> Result<(), MmregError> {
    let dev = Device::open_uio(name)?;
    for warning in dev.warnings() {
        eprintln!("{warning}");
    }
    println!(
        "{} at {:#x} ({} bytes): waiting for {count} interrupts",
        dev.name(),
        dev.base_address(),
        dev.size()
    );

    dev.irq_unmask()?;
    let mut seen = 0;
    while seen < count {
        // `Duration::MAX` waits without a timeout.
        let Some(total) = dev.irq_wait(Duration::from_secs(5))? else {
            println!("no interrupt in 5 s");
            continue;
        };
        seen += 1;
        let isr = dev.read_u32(ISR)?;
        println!("interrupt {seen} (kernel count {total}): ISR = {isr:#010x}");

        // Level-triggered sources: clear the cause in the device before unmasking the line.
        // Masked write: clears exactly the pending W1C bits read above, keeps read-write bits.
        dev.update_u32(ISR, ISR_W1C_MASK, isr & ISR_W1C_MASK)?;
        dev.irq_unmask()?;
    }
    Ok(())
}
