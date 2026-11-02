/*
 * mmreg.h — C ABI for mmreg (safe, concurrent 8/16/32/64-bit MMIO register access on Linux)
 *
 * SPDX-License-Identifier: MIT
 */

#ifndef MMREG_H
#define MMREG_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/** Version of the API this header describes; compare with `mmreg_version()` at run time. */
#define MMREG_VERSION_MAJOR 1
#define MMREG_VERSION_MINOR 4
#define MMREG_VERSION_PATCH 1

/**
 * Opaque handle to a mapped peripheral register window (UIO or /dev/mem).
 * Thread-safe: one `mmreg_dev *` may be shared across threads.
 */
typedef struct mmreg_dev mmreg_dev;

/** Open the device read-only (`O_RDONLY` + `PROT_READ`). */
#define MMREG_O_RDONLY  (1u << 0)

/** Skip blueprint and hardware pre-flight validation (`--force`). */
#define MMREG_O_FORCE   (1u << 1)

/**
 * Back the device with an in-memory buffer for off-target unit tests (requires `MMREG_O_FORCE`).
 * Windows over 16 MiB are rejected with `E_INVALID_ARG` by every open function.
 */
#define MMREG_O_MOCK    (1u << 2)

/** Encode UIO region index `n` (`maps/map<n>`, default 0) into `flags`. */
#define MMREG_REGION(n) (((uint32_t)((n) & 0xFFu)) << 8)

/** Extract a bitfield from a 32-bit register value `v` given its `mask` and bit `shift`. */
#define MMREG_FIELD(v, mask, shift) ((((uint32_t)(v)) & ((uint32_t)(mask))) >> (shift))

/**
 * Shift and mask a field value `v` into position for `mmreg_update32` or `mmreg_wr32`.
 * `v` is converted to `uint32_t` first, so shifting into bit 31 is well defined.
 */
#define MMREG_PREP(v, mask, shift)  ((((uint32_t)(v)) << (shift)) & ((uint32_t)(mask)))

/* --- Thread-local error reporting --- */

/**
 * Returns the calling thread's last error message (`"[E_CODE] ..."`), or `""` if no error has
 * occurred on this thread. Never returns NULL. Valid until the next `mmreg_*` call on the thread.
 */
const char *mmreg_last_error(void);

/**
 * Returns the calling thread's last machine-readable error code (`"E_UIO_NOT_FOUND"`, etc.), or
 * `""` if no error has occurred on this thread. Never returns NULL.
 */
const char *mmreg_last_error_code(void);

/** Clears the calling thread's last error state. */
void mmreg_clear_error(void);

/** Returns the loaded library's version (`"MAJOR.MINOR.PATCH"`). Never returns NULL. */
const char *mmreg_version(void);

/* --- UIO discovery & device lifecycle --- */

/**
 * Looks up a UIO device by its `/sys/class/uio/uio*\/name` attribute and writes its
 * NUL-terminated `/dev/uioN` path into `buf` (of capacity `len` bytes).
 *
 * Returns 0 on success, or -1 on error (inspect with `mmreg_last_error()`).
 */
int mmreg_find_uio(const char *name, char *buf, size_t len);

/**
 * Opens region `MMREG_REGION(n)` (default 0) of the UIO device matching `name`.
 * Returns a non-NULL `mmreg_dev *` on success, or NULL on error.
 */
mmreg_dev *mmreg_open_uio(const char *name, uint32_t flags);

/**
 * Opens region `MMREG_REGION(n)` (default 0) of the UIO device node at `path` (e.g. `"/dev/uio0"`).
 * Returns a non-NULL `mmreg_dev *` on success, or NULL on error.
 */
mmreg_dev *mmreg_open_uio_path(const char *path, uint32_t flags);

/**
 * Opens the physical address window `[base, base + size)` via a covering UIO region or `/dev/mem`
 * (when built with the `debug` feature). `MMREG_REGION(n)` with `n != 0` is rejected with
 * `E_INVALID_ARG`: the address selects the region.
 * Returns a non-NULL `mmreg_dev *` on success, or NULL on error.
 */
mmreg_dev *mmreg_open_addr(uint64_t base, size_t size, uint32_t flags);

/**
 * Unmaps and closes `dev`, releasing any held locks. Passing NULL is a safe no-op.
 */
void mmreg_close(mmreg_dev *dev);

/* --- Locked register reads, writes, and read-modify-write --- */

int mmreg_rd8(const mmreg_dev *dev, uint32_t offset, uint8_t *out);
int mmreg_rd16(const mmreg_dev *dev, uint32_t offset, uint16_t *out);
int mmreg_rd32(const mmreg_dev *dev, uint32_t offset, uint32_t *out);
int mmreg_rd64(const mmreg_dev *dev, uint32_t offset, uint64_t *out);

int mmreg_wr8(const mmreg_dev *dev, uint32_t offset, uint8_t value);
int mmreg_wr16(const mmreg_dev *dev, uint32_t offset, uint16_t value);
int mmreg_wr32(const mmreg_dev *dev, uint32_t offset, uint32_t value);
int mmreg_wr64(const mmreg_dev *dev, uint32_t offset, uint64_t value);

/**
 * Atomically updates bits in `mask` at 4-byte-aligned `offset` (`(old & ~mask) | (value & mask)`)
 * under one lock. If `out_new` is non-NULL, stores the new 32-bit register value in `*out_new`.
 * Returns 0 on success, or -1 on error.
 */
int mmreg_update32(
    const mmreg_dev *dev,
    uint32_t offset,
    uint32_t mask,
    uint32_t value,
    uint32_t *out_new
);

/* --- Reentrant sequence locking --- */

/**
 * Acquires a reentrant sequence lock on `dev` (held across subsequent reads/writes/updates on the
 * calling thread until a matching `mmreg_unlock(dev)`).
 * Returns 0 on success, or -1 on error.
 */
int mmreg_lock(const mmreg_dev *dev);

/**
 * Releases one sequence lock acquired by `mmreg_lock(dev)` on the calling thread.
 * Returns 0 on success, or -1 on error.
 */
int mmreg_unlock(const mmreg_dev *dev);

/* --- UIO interrupts --- */

/**
 * Unmasks (re-enables) the UIO interrupt line by writing `(uint32_t)1` to `/dev/uioN`.
 * For level-triggered interrupts, clear the interrupt cause in the device's registers first.
 * Returns 0 on success, or -1 on error.
 */
int mmreg_irq_unmask(const mmreg_dev *dev);

/** `timeout_ms` for `mmreg_irq_wait` that waits for an interrupt without a timeout. */
#define MMREG_WAIT_FOREVER UINT64_MAX

/**
 * Waits up to `timeout_ms` milliseconds (`MMREG_WAIT_FOREVER`: no timeout) for a UIO interrupt
 * on `dev`.
 * Returns:
 * -  1 if an interrupt fired (writes the cumulative kernel IRQ count to `*out_count` if non-NULL),
 * -  0 if the timeout elapsed with no interrupt,
 * - -1 on error (inspect with `mmreg_last_error()`).
 */
int mmreg_irq_wait(const mmreg_dev *dev, uint64_t timeout_ms, uint32_t *out_count);

/* --- Window metadata & pre-flight warnings --- */

uint64_t mmreg_base_address(const mmreg_dev *dev);
size_t mmreg_size(const mmreg_dev *dev);
size_t mmreg_warning_count(const mmreg_dev *dev);
const char *mmreg_warning(const mmreg_dev *dev, size_t index);

#ifdef __cplusplus
}
#endif

#endif /* MMREG_H */
