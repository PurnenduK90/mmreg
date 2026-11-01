/*
 * test_capi.c — Example and verification test for the mmreg C ABI (`bindings/c/include/mmreg.h`).
 *
 * Build against the static library on Linux, from the repository root:
 *   cargo build --release -p mmreg-capi
 *   cc -O2 -Wall -Wextra -Werror -Ibindings/c/include bindings/c/examples/test_capi.c \
 *      target/release/libmmreg.a $(pkg-config --libs --static mmreg) -o test_capi
 *
 * Modes:
 *   ./test_capi                 — runs self-tests on an off-target mock device
 *   ./test_capi lock <hex-addr> — acquires the page lock at <hex-addr> for C <-> Rust interop,
 *                                 printing "ready" before and "acquired" after mmreg_lock()
 */

#include "mmreg.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Always evaluates `cond`, unlike assert(), so the calls run under -DNDEBUG too. */
#define CHECK(cond)                                                                     \
    do {                                                                                \
        if (!(cond)) {                                                                  \
            fprintf(stderr, "%s:%d: CHECK(%s) failed: %s\n", __FILE__, __LINE__, #cond, \
                    mmreg_last_error());                                                \
            exit(1);                                                                    \
        }                                                                               \
    } while (0)

#define CTRL_REG            0x00u
#define CTRL_EN_MASK        0x00000001u
#define CTRL_EN_SHIFT       0u
#define CTRL_MODE_MASK      0x000000F0u
#define CTRL_MODE_SHIFT     4u
#define CTRL_TOP_MASK       0x80000000u
#define CTRL_TOP_SHIFT      31u

static int lock_mode(const char *addr_arg) {
    uint64_t addr = strtoull(addr_arg, NULL, 0);
    mmreg_dev *dev = mmreg_open_addr(addr, 4096, MMREG_O_FORCE | MMREG_O_MOCK);
    CHECK(dev != NULL);
    printf("ready\n");
    fflush(stdout);
    CHECK(mmreg_lock(dev) == 0);
    printf("acquired\n");
    fflush(stdout);
    CHECK(mmreg_unlock(dev) == 0);
    mmreg_close(dev);
    return 0;
}

int main(int argc, char **argv) {
    /* Optional mode for two-process C <-> Rust lock interop testing */
    if (argc == 3 && strcmp(argv[1], "lock") == 0) {
        return lock_mode(argv[2]);
    }

    /* The header and the loaded library agree on the version */
    char expected[32];
    snprintf(expected, sizeof expected, "%d.%d.%d", MMREG_VERSION_MAJOR, MMREG_VERSION_MINOR,
             MMREG_VERSION_PATCH);
    CHECK(strcmp(mmreg_version(), expected) == 0);

    mmreg_clear_error();
    CHECK(strcmp(mmreg_last_error(), "") == 0);
    CHECK(strcmp(mmreg_last_error_code(), "") == 0);

    /* Open a 64-byte mock register window at 0x4000_0000 */
    mmreg_dev *dev = mmreg_open_addr(0x40000000ULL, 64, MMREG_O_FORCE | MMREG_O_MOCK);
    CHECK(dev != NULL);
    CHECK(mmreg_base_address(dev) == 0x40000000ULL);
    CHECK(mmreg_size(dev) == 64);

    /* 8-, 16-, 32-, and 64-bit register reads and writes */
    CHECK(mmreg_wr8(dev, 0x00, 0xAB) == 0);
    uint8_t v8 = 0;
    CHECK(mmreg_rd8(dev, 0x00, &v8) == 0);
    CHECK(v8 == 0xAB);

    CHECK(mmreg_wr16(dev, 0x02, 0xCD12) == 0);
    uint16_t v16 = 0;
    CHECK(mmreg_rd16(dev, 0x02, &v16) == 0);
    CHECK(v16 == 0xCD12);

    CHECK(mmreg_wr32(dev, CTRL_REG, 0x00000000u) == 0);
    uint32_t ctrl = 0;
    CHECK(mmreg_update32(
        dev,
        CTRL_REG,
        CTRL_EN_MASK | CTRL_MODE_MASK | CTRL_TOP_MASK,
        MMREG_PREP(1, CTRL_EN_MASK, CTRL_EN_SHIFT) | MMREG_PREP(5, CTRL_MODE_MASK, CTRL_MODE_SHIFT) |
            MMREG_PREP(1, CTRL_TOP_MASK, CTRL_TOP_SHIFT),
        &ctrl
    ) == 0);
    CHECK(MMREG_FIELD(ctrl, CTRL_EN_MASK, CTRL_EN_SHIFT) == 1u);
    CHECK(MMREG_FIELD(ctrl, CTRL_MODE_MASK, CTRL_MODE_SHIFT) == 5u);
    CHECK(MMREG_FIELD(ctrl, CTRL_TOP_MASK, CTRL_TOP_SHIFT) == 1u);

    CHECK(mmreg_wr64(dev, 0x08, 0x0102030405060708ULL) == 0);
    uint64_t v64 = 0;
    CHECK(mmreg_rd64(dev, 0x08, &v64) == 0);
    CHECK(v64 == 0x0102030405060708ULL);

    /* Reentrant sequence lock, usable through a const handle */
    const mmreg_dev *cdev = dev;
    CHECK(mmreg_lock(cdev) == 0);
    CHECK(mmreg_lock(cdev) == 0);
    CHECK(mmreg_wr32(cdev, 0x10, 0xCAFEBABEu) == 0);
    CHECK(mmreg_unlock(cdev) == 0);
    CHECK(mmreg_unlock(cdev) == 0);

    /* Alignment error check */
    uint32_t bad = 0;
    CHECK(mmreg_rd32(dev, 0x02, &bad) == -1);
    CHECK(strcmp(mmreg_last_error_code(), "E_UNALIGNED_ACCESS") == 0);

    mmreg_close(dev);
    printf("C ABI test passed.\n");
    return 0;
}
