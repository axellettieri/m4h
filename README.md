# M4H — Multi4Hyper

*Ad astra per aspera.*

M4H is a multikernel written in Rust, built for hyper-scale message workloads:
tens of millions of WebSocket connections on a single server, one lightweight
BEAM process per connection.

- **M4H** — the multikernel: per-core state, NUMA-aware memory, cores that
  coordinate only through message rings. No global locks.
- **M4H-BEAM** — a BEAM-compatible VM in Rust. ERTS is rewritten; standard
  `.beam` files (Erlang, Elixir, OTP) run unchanged.
- **m4h-net** — a per-core, zero-copy network stack shared by M4H-BEAM and M4H-LB.
- **M4H-LB** — TLS-terminating load balancer with Virtual IP failover.

## Crates

| Crate | Purpose | Status |
|---|---|---|
| `m4h` | Umbrella crate | 0.0.1 |
| `m4h-ring` | Lock-free SPSC rings with 64-byte, cache-line-aligned slots | usable, verified with loom and Miri |
| `m4h-platform` | The `Platform` trait and its Linux backend | first draft |
| `m4h-bench` | Ring throughput and latency benchmarks | usable |
| `m4h-kernel` | The multikernel | placeholder |
| `m4h-beam` | The BEAM-compatible VM | placeholder |
| `m4h-net` | The shared network stack | placeholder |
| `m4h-lb` | The load balancer | placeholder |

## Building and testing

```sh
cargo build --workspace
cargo test --workspace

# model checking of the ring protocol
RUSTFLAGS="--cfg loom" cargo test -p m4h-ring --test loom --release

# undefined-behaviour checks (nightly)
cargo +nightly miri test -p m4h-ring
```

The toolchain is pinned in `rust-toolchain.toml`; the minimum supported Rust
version is 1.85, checked in CI. The memory-ordering argument of the ring is in
the [`m4h-ring` documentation](crates/m4h-ring/src/lib.rs).

## Benchmarks

```sh
cargo run --release -p m4h-bench -- topology
cargo run --release -p m4h-bench -- suite
cargo run --release -p m4h-bench -- throughput --cores 0,8 --impl m4h,m4h-batch,rtrb
cargo run --release -p m4h-bench -- latency --cores 0,8
```

`suite` reads the topology and runs every implementation on SMT siblings, on
two cores of the same NUMA node, and on two cores of different NUMA nodes.
`rtrb` and crossbeam's `ArrayQueue` are included as baselines.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
