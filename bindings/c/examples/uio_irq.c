/*
 * Interrupt loop on a real UIO device (needs the hardware; see quickstart.c otherwise).
 *
 *   ./uio_irq <uio name> [count]      e.g. ./uio_irq axi_timer 10
 *
 * The peripheral is assumed to have an ISR register at 0x0C with write-1-to-clear DONE (bit 0)
 * and ERR (bit 1) bits; adapt the constants to your IP.
 */
#include "mmreg.h"

#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>

#define ISR          0x0Cu
#define ISR_DONE     0x00000001u
#define ISR_ERR      0x00000002u
#define ISR_W1C_MASK (ISR_DONE | ISR_ERR)

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <uio name> [count]\n", argv[0]);
        return 2;
    }
    unsigned long count = argc > 2 ? strtoul(argv[2], NULL, 0) : 5;

    mmreg_dev *dev = mmreg_open_uio(argv[1], 0);
    if (!dev) {
        fprintf(stderr, "open failed [%s]: %s\n", mmreg_last_error_code(), mmreg_last_error());
        return 1;
    }
    for (size_t i = 0; i < mmreg_warning_count(dev); i++) {
        fprintf(stderr, "%s\n", mmreg_warning(dev, i));
    }
    printf("%s at 0x%" PRIx64 " (%zu bytes): waiting for %lu interrupts\n", argv[1],
           mmreg_base_address(dev), mmreg_size(dev), count);

    int rc = mmreg_irq_unmask(dev);
    for (unsigned long seen = 0; rc == 0 && seen < count;) {
        uint32_t total = 0;
        int fired = mmreg_irq_wait(dev, 5000, &total); /* MMREG_WAIT_FOREVER: no timeout */
        if (fired < 0) {
            rc = -1;
            break;
        }
        if (fired == 0) {
            printf("no interrupt in 5 s\n");
            continue;
        }
        seen++;
        uint32_t isr = 0;
        rc = mmreg_rd32(dev, ISR, &isr);
        printf("interrupt %lu (kernel count %" PRIu32 "): ISR = 0x%08" PRIx32 "\n", seen, total, isr);

        /* Level-triggered sources: clear the cause in the device before unmasking the line.
         * Masked write: clears exactly the pending W1C bits read above, keeps read-write bits. */
        if (rc == 0) {
            rc = mmreg_update32(dev, ISR, ISR_W1C_MASK, isr & ISR_W1C_MASK, NULL);
        }
        if (rc == 0) {
            rc = mmreg_irq_unmask(dev);
        }
    }
    if (rc != 0) {
        fprintf(stderr, "failed [%s]: %s\n", mmreg_last_error_code(), mmreg_last_error());
    }
    mmreg_close(dev);
    return rc == 0 ? 0 : 1;
}
