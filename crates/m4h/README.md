# m4h

*Ad astra per aspera.*

Umbrella crate of **M4H (Multi4Hyper)**, a multikernel written in Rust for
hyper-scale message workloads: tens of millions of WebSocket connections on a
single server, one lightweight BEAM process per connection.

The project is in early development. The components live in their own crates:

- [`m4h-ring`](https://crates.io/crates/m4h-ring): lock-free SPSC rings with
  64-byte, cache-line-aligned slots, the core inter-core primitive;
- `m4h-platform`: the `Platform` trait and its Linux backend;
- `m4h-kernel`, `m4h-beam`, `m4h-net`, `m4h-lb`: the multikernel, the
  BEAM-compatible VM, the network stack and the load balancer (in progress).

Source, architecture and roadmap: <https://github.com/axellettieri/m4h>.

Licensed under either of Apache License 2.0 or MIT license, at your option.
