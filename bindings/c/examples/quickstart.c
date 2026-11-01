/*
 * Tour of the mmreg C API on an in-memory register window: runs without hardware.
 *
 *   cargo build --release -p mmreg-capi
 *   cc -Ibindings/c/include bindings/c/examples/quickstart.c target/release/libmmreg.a \
 *      -lgcc_s -lutil -lrt -lpthread -lm -ldl -lc -o quickstart && ./quickstart
 *
 * MMREG_O_MOCK backs the device with plain memory. On hardware, open the device with
 * mmreg_open_uio("<uio name>", 0) instead (see uio_irq.c).
 */
#include "mmreg.h"

#include <inttypes.h>
#include <stdio.h>

/* Register map of the example peripheral, as a register generator would emit it. */
#define CTRL            0x00u
#define CTRL_EN_MASK    0x00000001u
#define CTRL_EN_SHIFT   0u
#define CTRL_MODE_MASK  0x000000F0u
#define CTRL_MODE_SHIFT 4u
#define ISR             0x0Cu
#define ISR_DONE        0x00000001u /* write-1-to-clear */
#define ISR_ERR         0x00000002u /* write-1-to-clear */
#define ISR_W1C_MASK    (ISR_DONE | ISR_ERR)
#define ISR_IRQ_EN      0x00000100u /* read-write bit in the same register */
#define SCRATCH         0x10u

/* Prints the failed call and the thread's last error; returns 1 for `main`. */
static int fail(const char *what) {
    fprintf(stderr, "%s failed [%s]: %s\n", what, mmreg_last_error_code(), mmreg_last_error());
    return 1;
}

int main(void) {
    mmreg_dev *dev = mmreg_open_addr(0x40000000u, 0x100, MMREG_O_FORCE | MMREG_O_MOCK);
    if (!dev) {
        return fail("mmreg_open_addr");
    }
    int rc = 1;

    /* 8, 16, 32 and 64-bit accesses (each naturally aligned). */
    uint8_t u8 = 0;
    uint16_t u16 = 0;
    uint64_t u64 = 0;
    if (mmreg_wr8(dev, 0x20u, 0xABu) || mmreg_wr16(dev, 0x22u, 0xCAFEu) ||
        mmreg_wr64(dev, 0x28u, 0x0102030405060708ull) || mmreg_rd8(dev, 0x20u, &u8) ||
        mmreg_rd16(dev, 0x22u, &u16) || mmreg_rd64(dev, 0x28u, &u64)) {
        rc = fail("8/16/64-bit access");
        goto out;
    }
    printf("u8 0x%02x, u16 0x%04x, u64 0x%016" PRIx64 "\n", u8, u16, u64);

    /* A field write is a locked read-modify-write of only that field. */
    uint32_t ctrl = 0;
    if (mmreg_update32(dev, CTRL, CTRL_EN_MASK, MMREG_PREP(1u, CTRL_EN_MASK, CTRL_EN_SHIFT), NULL) ||
        mmreg_update32(dev, CTRL, CTRL_MODE_MASK, MMREG_PREP(5u, CTRL_MODE_MASK, CTRL_MODE_SHIFT), &ctrl)) {
        rc = fail("mmreg_update32");
        goto out;
    }
    printf("CTRL = 0x%08" PRIx32 ": EN = %" PRIu32 ", MODE = %" PRIu32 "\n", ctrl,
           MMREG_FIELD(ctrl, CTRL_EN_MASK, CTRL_EN_SHIFT), MMREG_FIELD(ctrl, CTRL_MODE_MASK, CTRL_MODE_SHIFT));

    /* A multi-register sequence under one lock (reentrant: the accesses inside lock again). */
    if (mmreg_lock(dev)) {
        rc = fail("mmreg_lock");
        goto out;
    }
    uint32_t snapshot = 0;
    int seq = mmreg_rd32(dev, CTRL, &snapshot) || mmreg_wr32(dev, SCRATCH, snapshot);
    if (mmreg_unlock(dev) || seq) {
        rc = fail("locked sequence");
        goto out;
    }

    /* Clear a write-1-to-clear bit with a masked write: the mask holds every W1C bit, so DONE is
     * written 1, ERR 0 (a pending ERR stays pending) and the read-write IRQ_EN keeps its value. */
    uint32_t isr = 0;
    if (mmreg_wr32(dev, ISR, ISR_IRQ_EN) || mmreg_update32(dev, ISR, ISR_W1C_MASK, ISR_DONE, &isr)) {
        rc = fail("clear ISR.DONE");
        goto out;
    }
    printf("clearing ISR.DONE wrote 0x%08" PRIx32 " (IRQ_EN kept, ERR written as 0)\n", isr);

    /* Errors: -1 plus a stable code. */
    uint32_t value = 0;
    if (mmreg_rd32(dev, 0x02u, &value) != 0) {
        printf("mmreg_rd32(0x02) -> [%s]\n", mmreg_last_error_code());
    }
    rc = (isr == (ISR_IRQ_EN | ISR_DONE) && ctrl == 0x51u) ? 0 : 1;

out:
    mmreg_close(dev);
    return rc;
}
