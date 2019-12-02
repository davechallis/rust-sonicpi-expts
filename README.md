# Sonic Pi Rust Backend

Experiments on writing a Sonic Pi OSC forwarding backend in Rust. Initial aim
is to replicate the same functionality as available via the
[Erlang server](https://github.com/samaaron/sonic-pi/tree/master/app/server/erlang).

## Current design notes

Testing new tokio async/await for dealing with this. Depending on timer
latency/precision, alternate approaches might be needed (e.g. high precision
timers, dedicated threads for specific subtasks, etc.).

Uses the `env_logger` crate for logging, consider moving to tokio trace if
the tokio approach looks promising longer term.

## Running

Binds to `127.0.0.1:4560` by default, or pass in bind parameter as first
positional argument. E.g. to run with debug logging and given host/port, use:

    RUST_LOG=rust_sonicpi_expts=debug cargo run 127.0.0.1:4560

Or e.g. for a release build:

    cargo build --release
    RUST_LOG=rust_sonicpi_expts=debug ./target/release/rust-sonicpi-expts localhost:1234

## Questions

* how to best set up testing environment? (for e.g. heavy load)
* how to measure latency?
* get estimate of latency/load requirements
* desired failure modes (e.g. drop messages vs. late delivery)
* desired configuration options
