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
| `m4h-ring` | Lock-free SPSC rings with 64-byte, cache-line-aligned slots | in progress |
| `m4h-platform` | The `Platform` trait and its Linux backend | in progress |
| `m4h-kernel` | The multikernel | placeholder |
| `m4h-beam` | The BEAM-compatible VM | placeholder |
| `m4h-net` | The shared network stack | placeholder |
| `m4h-lb` | The load balancer | placeholder |
| `m4h-bench` | Benchmarks | placeholder |

## Building

```sh
cargo build --workspace
cargo test --workspace
```

The toolchain is pinned in `rust-toolchain.toml`.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
