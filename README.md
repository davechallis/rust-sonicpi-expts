# Sonic Pi Rust Backend

Experiments on writing a [Sonic Pi](https://sonic-pi.net/)
[OSC](http://opensoundcontrol.org/introduction-osc) forwarding backend in Rust.
Initial aim is to replicate the same functionality as available via the
[Erlang server](https://github.com/samaaron/sonic-pi/tree/master/app/server/erlang).

## Commands

### Bundle commands

These are sent as OSC bundles, where the timetag indicates the time that the bundle command
should be sent on to the OSC synth.

---

`/send_after` - send given message at bundle's timetag, with "default" tag

OSC bundle should consist of:

    OscBundle {
        timetag: (<seconds>, <fractional_seconds>),
        content: [
            OscMessage {
                addr: "/send_after",
                args: [<host>, <port>, <addr>, <cmd>...]
            }
        ]
    }
    
This will queue an OSC message of the form `OscMessage {addr: <addr>, args: [<cmd>...]}` to
be sent to UDP address `<host>:<port>` at the time specified by the timetag.

It will be tagged with `"default"` (tags can be used to cancel pending commands).
    
---

`/send_after_tagged` - send given message at bundle's timetag, with assigned tag

OSC bundle should consist of:

    OscBundle {
        timetag: (<seconds>, <fractional_seconds>),
        content: [
            OscMessage {
                addr: "/send_after_tagged",
                args: [<tag>, <host>, <port>, <addr>, <cmd>...]
            }
        ]
    }
    
This will queue an OSC message of the form `OscMessage {addr: <addr>, args: [<cmd>...]}` to
be sent to UDP address `<host>:<port>` at the time specified by the timetag.

It will be tagged with `<tag>` (tags can be used to cancel pending commands).

### Message commands

`/flush` - cancel all pending messages with a given tag

OSC message should consist of:

    OscMessage {
        addr: "/flush",
        args: [<tag>]
    }
    
All pending messages previously queued via `/send_after` or `/send_after_tagged` with the
given `<tag>` will be cancelled.

---

`/clock/sync`: TODO? (find out if this is needed, and what it should do)

---

## Running

Binds to `127.0.0.1:4560` by default, or pass in bind parameter as first
positional argument. E.g. to run with debug logging and given host/port, use:

    RUST_LOG=rust_sonicpi_expts=debug cargo run 127.0.0.1:4560

Or e.g. for a release build:

    cargo build --release
    RUST_LOG=rust_sonicpi_expts=debug ./target/release/rust-sonicpi-expts localhost:1234
    
## Current design notes

Testing new tokio async/await for dealing with this. Depending on timer
latency/precision, alternate approaches might be needed (e.g. high precision
timers, dedicated threads for specific subtasks, etc.).

Uses the `env_logger` crate for logging, consider moving to tokio trace if
the tokio approach looks promising longer term.

## Questions

* how to best set up testing environment? (for e.g. heavy load)
* how to measure latency?
* get estimate of latency/load requirements
* desired failure modes (e.g. drop messages vs. late delivery)
* desired configuration options
* functionality of `/clock/sync` command?
