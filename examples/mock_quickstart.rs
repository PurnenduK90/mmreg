//! Tour of `Device` on an in-memory register window: runs on any machine, no hardware needed.
//!
//! ```sh
//! cargo run --example mock_quickstart
//! ```
//!
//! On hardware, open the device with `Device::open_uio("<uio name>")` instead (see `uio_irq.rs`).

use mmreg::{Device, MemoryMapper, MmregError, Width};
use std::sync::{Arc, Mutex};

// Register map of the example peripheral, as a register generator would emit it.
const CTRL: u32 = 0x00;
const CTRL_EN: u32 = 1 << 0;
const CTRL_MODE_MASK: u32 = 0xF << 4;
const CTRL_MODE_SHIFT: u32 = 4;
const ISR: u32 = 0x0C;
const ISR_DONE: u32 = 1 << 0; // write-1-to-clear
const ISR_ERR: u32 = 1 << 1; // write-1-to-clear
const ISR_W1C_MASK: u32 = ISR_DONE | ISR_ERR;
const ISR_IRQ_EN: u32 = 1 << 8; // read-write bit in the same register
const SCRATCH: u32 = 0x10;

type Registers = Arc<Mutex<Vec<u8>>>;

fn load(regs: &Registers, at: usize, len: usize) -> u64 {
    let mem = regs.lock().unwrap_or_else(|e| e.into_inner());
    let mut buf = [0u8; 8];
    buf[..len].copy_from_slice(&mem[at..at + len]);
    u64::from_le_bytes(buf)
}

fn store(regs: &Registers, at: usize, len: usize, value: u64) {
    let mut mem = regs.lock().unwrap_or_else(|e| e.into_inner());
    mem[at..at + len].copy_from_slice(&value.to_le_bytes()[..len]);
}

/// Memory standing in for the peripheral; the `ISR` bits in `ISR_W1C_MASK` behave like hardware
/// write-1-to-clear bits (writing 1 clears, writing 0 leaves them as they are).
struct MockPeripheral {
    regs: Registers,
    mapped: bool,
}

impl MemoryMapper for MockPeripheral {
    fn map(&mut self, _address: u64, size: usize) -> Result<(), MmregError> {
        self.regs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .resize(size, 0);
        self.mapped = true;
        Ok(())
    }

    fn unmap(&mut self) {
        self.mapped = false;
    }

    fn is_mapped(&self) -> bool {
        self.mapped
    }

    fn read(&self, offset: u32, width: Width) -> u64 {
        load(&self.regs, offset as usize, width.bytes())
    }

    fn write(&self, offset: u32, width: Width, value: u64) {
        let mut value = value;
        if offset == ISR {
            let w1c = u64::from(ISR_W1C_MASK);
            let old = load(&self.regs, offset as usize, width.bytes());
            value = (value & !w1c) | (old & w1c & !value);
        }
        store(&self.regs, offset as usize, width.bytes(), value);
    }
}

fn main() -> Result<(), MmregError> {
    let regs = Registers::default();
    let mock = MockPeripheral {
        regs: Arc::clone(&regs),
        mapped: false,
    };
    // `with_force(true)` skips the blueprint and pre-flight checks, which only apply to hardware.
    let dev = Device::addr(0x4000_0000, 0x100)
        .with_custom_mapper(Box::new(mock))
        .with_force(true)
        .open()?;

    // 8, 16, 32 and 64-bit accesses (each naturally aligned).
    dev.write_u8(0x20, 0xAB)?;
    dev.write_u16(0x22, 0xCAFE)?;
    dev.write_u64(0x28, 0x0102_0304_0506_0708)?;
    println!(
        "u8 {:#04x}, u16 {:#06x}, u64 {:#018x}",
        dev.read_u8(0x20)?,
        dev.read_u16(0x22)?,
        dev.read_u64(0x28)?
    );

    // A field write is a locked read-modify-write of only that field.
    dev.update_u32(CTRL, CTRL_EN, CTRL_EN)?;
    let ctrl = dev.update_u32(CTRL, CTRL_MODE_MASK, 5 << CTRL_MODE_SHIFT)?;
    println!("CTRL = {ctrl:#010x} (EN = 1, MODE = 5)");
    assert_eq!(ctrl, 0x51);

    // A multi-register sequence under one lock; the accesses inside take it again (reentrant).
    {
        let _guard = dev.lock()?;
        let ctrl = dev.read_u32(CTRL)?;
        dev.write_u32(SCRATCH, ctrl)?;
    }

    // Software enables the interrupt; the "hardware" then raises DONE and ERR.
    dev.write_u32(ISR, ISR_IRQ_EN)?;
    let raised = load(&regs, ISR as usize, 4) | u64::from(ISR_W1C_MASK);
    store(&regs, ISR as usize, 4, raised);
    println!(
        "ISR = {:#010x} (IRQ_EN, DONE and ERR set)",
        dev.read_u32(ISR)?
    );

    // Clear DONE with a masked write: the mask holds every W1C bit, so DONE is written 1
    // (cleared), ERR is written 0 (stays pending) and the read-write IRQ_EN keeps its value.
    dev.update_u32(ISR, ISR_W1C_MASK, ISR_DONE)?;
    println!(
        "ISR = {:#010x} after update_u32(ISR, W1C_MASK, DONE): DONE cleared, ERR pending, IRQ_EN kept",
        dev.read_u32(ISR)?
    );
    assert_eq!(dev.read_u32(ISR)?, ISR_IRQ_EN | ISR_ERR);

    // The wrong way: a plain write also writes 0 to IRQ_EN and so disables the interrupt.
    dev.write_u32(ISR, ISR_ERR)?;
    println!(
        "ISR = {:#010x} after write_u32(ISR, ERR): ERR cleared, but IRQ_EN lost",
        dev.read_u32(ISR)?
    );
    assert_eq!(dev.read_u32(ISR)?, 0);

    // Errors carry a stable code.
    let err = dev.read_u32(0x02).unwrap_err();
    println!("read_u32(0x02) -> [{}]", err.code());
    Ok(())
}
