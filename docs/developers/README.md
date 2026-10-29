# `mmreg` Developer Guide

Architecture and development documentation for contributors.

1. **[Architecture & Execution Flow](01-architecture.md)**: modules and their single
   responsibilities, the access flow, per-page locking and RAII cleanup.
2. **[Memory Mapper Backends](02-memory-mappers.md)**: the `MemoryMapper` trait, `/dev/mem` and
   UIO window arithmetic, and custom/mock mappers.
3. **[Blueprint & Pre-Flight Internals](03-preflight-internals.md)**: `resolve`, Device Tree `reg`
   parsing, FPGA detection, clock checks, the runtime PM guard and safety profiles.
4. **[Build, Test & Release](04-build-test-release.md)**: `just` tasks, features and binary size,
   cross compilation and CI.
