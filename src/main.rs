use std::{env, io, fmt};
use std::time::Duration;
use std::error::Error;
use tokio::sync;
use tokio::net::UdpSocket;
use chrono::offset::TimeZone;
use log::{debug, info, warn};
use futures::select;
use futures::future::FutureExt;
use std::collections::HashMap;


// Delta between NTP epoch (1900-01-01 00:00:00) and Unix epoch (1970-01-01 00:00:00).
// Contains 53 non-leap years, and 17 leap years, in seconds, this is:
// (53 * 365 + 17 * 366) * 86400 = 2208988800.
const EPOCH_DELTA: u64 = 2_208_988_800;

// Tag name to use for messages without an explicit tag (i.e. currently those sent via
// `/send_after`).
const DEFAULT_TAG: &str = "default";

// Convert an OSC timetag into unix timestamp seconds and microseconds.
//
// [OSC timetags](http://opensoundcontrol.org/spec-1_0) use NTP timestamps
// (https://en.wikipedia.org/wiki/Network_Time_Protocol#Timestamps).
//
// TODO: verify time conversions are actually correct, check against other implementations
fn timetag_to_unix(ntp_secs: u32, ntp_frac_secs: u32) -> (u64, u32) {
    let unix_secs = ntp_secs as u64 - EPOCH_DELTA;
    let unix_micros = ((ntp_frac_secs as u64) * 1_000_000) >> 32;
    (unix_secs, unix_micros as u32)
}

// TODO: verify time conversions are actually correct, check roundtrips
fn timetag_to_duration(ntp_secs: u32, ntp_frac_secs: u32) -> Duration {
    let (unix_secs, unix_micros) = timetag_to_unix(ntp_secs, ntp_frac_secs);

    // TODO: can this be done with SystemTime to avoid `chrono` dependency?
    let dur = chrono::Utc.timestamp(unix_secs as i64, unix_micros * 1000) - chrono::Utc::now();

    // TODO: error handling, can occur on negative timestamps, or on overflow
    dur.to_std().expect("failed to convert duration")
}

/*
fn unix_to_timetag(unix_secs: u64, unix_micros: u32) -> (u32, u32) {
    let ntp_secs = unix_secs + EPOCH_DELTA;
    let ntp_frac_secs = ((unix_micros as u64 + 1) << 32) / 1_000_000;
    (ntp_secs as u32, ntp_frac_secs as u32)
}
*/

struct Server {
    /// Server's listening UDP socket.
    socket: UdpSocket,

    /// Internal buffer used for reading/writing UDP packets into.
    buf: Vec<u8>,

    /// Maps a tag name to sender/receiver pair. Used for signalling cancellations.
    tags: HashMap<String, (sync::watch::Sender<bool>, sync::watch::Receiver<bool>)>,
}

impl Server {
    pub async fn new(bind_addr: &str) -> Result<Self, io::Error> {
        debug!("Attempting to bind to: {}", bind_addr);
        let socket = UdpSocket::bind(bind_addr).await?;
        info!("Listening on: {}", socket.local_addr()?);
        Ok(Self {
            socket,
            buf: vec![0; 1024],
            tags: HashMap::new(),
        })
    }

    /// Main event loop, runs forever after server is started.
    async fn run(&mut self) -> Result<(), io::Error> {
        debug!("Starting main event loop");
        loop {
            if let Err(err) = self.next_event().await {
                warn!("{}", err);
            }
        }
    }

    /// Await UDP packet. Returns slice into server's buffer.

    /// Called from main server event loop (`run()`) on each iteration.
    ///
    /// Waits for incoming UDP packets containing OSC packets, either handling them immediately (in
    /// the case of e.g. `/flush` messages), or spawning futures to handle them in the future (in
    /// the case of e.g. `/send_after` bundles).
    async fn next_event(&mut self) -> Result<(), ServerError> {
        debug!("Waiting for UDP packet...");
        let raw_packet = self.recv_udp_packet().await?;
        debug!("Received UDP packet (size={})", raw_packet.len());

        debug!("Parsing OSC packet...");
        let osc_packet = rosc::decoder::decode(raw_packet)?;
        debug!("Received OSC packet: {:?}", osc_packet);

        match osc_packet {
            rosc::OscPacket::Message(msg) => {
                match msg.addr.as_ref() {
                    "/flush" => self.handle_msg_flush(&msg),
                    addr => {
                        let msg = format!("Ignoring unhandled OSC address: {}", addr);
                        return Err(ServerError::ProtocolError(msg));
                    }
                }
            },
            rosc::OscPacket::Bundle(bundle) => {
                if let rosc::OscType::Time(ntp_secs, ntp_subsecs) = bundle.timetag {
                    match bundle.content.first() {
                        Some(rosc::OscPacket::Message(msg)) => {
                            match msg.addr.as_ref() {
                                "/send_after" => self.handle_bundle_send_after(
                                    DEFAULT_TAG,
                                    timetag_to_duration(ntp_secs, ntp_subsecs),
                                    &msg.args
                                ),
                                "/send_after_tagged" => {
                                    match Self::parse_send_after_tag(&msg.args) {
                                        Ok(tag) => self.handle_bundle_send_after(
                                            &tag,
                                            timetag_to_duration(ntp_secs, ntp_subsecs),
                                            &msg.args[1..], // 1st argument is tag, already parsed
                                        ),
                                        Err(err) => {
                                            let msg = format!("Unexpected tag argument: {}", err);
                                            return Err(ServerError::ProtocolError(msg));
                                        },
                                    }
                                },
                                addr => {
                                    let msg = format!("Unhandled OSC address: {}", addr);
                                    return Err(ServerError::ProtocolError(msg));
                                },
                            }
                        },
                        other => {
                            let msg = format!("Unexpected OSC bundle content: {:?}", other);
                            return Err(ServerError::ProtocolError(msg));
                        }
                    }
                }
            },
        }

        Ok(())
    }
    async fn recv_udp_packet(&mut self) -> Result<&[u8], io::Error> {
        let (size, _) = self.socket.recv_from(&mut self.buf).await?;
        Ok(&self.buf[..size])
    }

    /// Handles /flush messages.
    fn handle_msg_flush(&mut self, msg: &rosc::OscMessage) {
        match msg.args.first() {
            Some(rosc::OscType::String(tag)) => {
                // Remove tag entry from hash map, and send termination signal to all listening
                // receivers.
                if let Some((_k, (tx, _rx))) = self.tags.remove_entry(tag) {
                    debug!("Flushing tag: {}", tag);
                    tx.broadcast(true).unwrap_or_else(|e| {
                        warn!("Failed to broadcast: {}", e);
                    });
                }
            },
            other => warn!("Ignoring unexpected /flush message: {:?}", other),
        };
    }

    fn handle_bundle_send_after(&mut self, tag: &str, send_after: Duration, msg_args: &[rosc::OscType]) {
        // TODO: find out general expected format, and parse
        let udp_addr = match Self::parse_command_address(msg_args) {
            Ok(addr) => addr,
            Err(err) => {
                warn!("Ignoring message: {}", err);
                return;
            },
        };

        // addr and OSX /<foo> addr
        let osc_cmd_addr = match msg_args.get(2) {
            Some(rosc::OscType::String(addr)) => addr,
            other => {
                warn!("Unexpected addr argument: {:?}", other);
                return;
            },
        };

        let remaining_args = &msg_args[3..];
        let (_tx, rx) = self.tags.entry(tag.to_owned()).or_insert(tokio::sync::watch::channel(false));

        debug!("Sending OSC command {:?} in: {}ms", remaining_args, send_after.as_millis());

        // TODO: parse bundle structure to determine what to send, this just
        // sends fixed message for testing.
        let new_msg = rosc::OscMessage {
            addr: osc_cmd_addr.to_owned(),
            args: remaining_args.to_vec(),
        };
        let packet = rosc::OscPacket::Message(new_msg);

        // TODO: error handling
        let new_buf = rosc::encoder::encode(&packet).expect("encoding failed");
        let mut rx2 = rx.clone();

        tokio::spawn(async move {
            // TODO: better way of doing this, configurable addr, etc.
            let loopback = std::net::Ipv4Addr::new(127, 0, 0, 1);
            let addr = std::net::SocketAddrV4::new(loopback, 0);

            // TODO: error handling
            let mut socket2 = UdpSocket::bind(addr).await.unwrap();

            // TODO: `watch.recv` always receives the initial channel value - look
            // for better way of handling this, rather than just reading/ignoring
            // then looping
            let mut done = false;
            while !done {
                select! {
                    _ = tokio::time::delay_for(send_after).fuse() => {
                        // `break;` will not work here, but look for better
                        // alternative
                        done = true;
                    },
                    cancel = rx2.recv().fuse() => {
                        match cancel {
                            Some(true) => {
                                debug!("cancelled timer");
                                return;
                            },
                            _ => {
                                // only needed to handle `false` values, i.e. to
                                // ignore initial state of the watch
                            },
                        }
                    },
                }
            }

            // TODO: error handling
            debug!("Sending OSC command to: {}", &udp_addr);
            socket2.send_to(&new_buf, &udp_addr).await.expect("send to failed!");
            debug!("OSC command sent");
        });
    }

    fn parse_send_after_tag(msg_args: &[rosc::OscType]) -> Result<String, String> {
        match msg_args.first() {
            Some(rosc::OscType::String(tag)) => Ok(tag.to_owned()),
            other => Err(format!("Unexpected tag argument: {:?}", other)),
        }
    }

    // TODO: error type
    /// Parse OSC server address (host and port) from given OSC message arguments (typically from
    /// `/send_after` messages).
    fn parse_command_address(msg_args: &[rosc::OscType]) -> Result<String, String> {
        let host = match msg_args.first() {
            Some(rosc::OscType::String(host)) => {
                // Workaround for https://github.com/rust-lang/rust/issues/34202
                // affecting OS X / Windows
                // TODO: check v6 status of Sonic Pi
                if host == "localhost" {
                    "127.0.0.1"
                } else {
                    host
                }
            },
            other => return Err(format!("Unexpected host argument: {:?}", other)),
        };

        let port = match msg_args.get(1) {
            Some(rosc::OscType::Int(port)) => port,
            other => return Err(format!("Unexpected port argument: {:?}", other)),
        };

        Ok(format!("{}:{}", host, port))
    }
}

#[derive(Debug)]
enum ServerError {
    /// Network error, typically caused by UDP send/recv here.
    IoError(io::Error),

    /// OSC error, typically caused by failing to encode/decode OSC data structures.
    OscError(rosc::OscError),

    /// Error in cases where valid OSC packets were received, but containing invalid payloads (e.g.
    /// a `/send_after` containing unexpected arguments).
    ProtocolError(String),
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::IoError(err) => write!(f, "IO error: {}", err),
            Self::OscError(err) => write!(f, "Failed to decode OSC packet: {:?}", err),
            Self::ProtocolError(err) => write!(f, "{}", err),
        }
    }
}

impl Error for ServerError {}

impl From<io::Error> for ServerError {
    fn from(err: io::Error) -> Self {
        Self::IoError(err)
    }
}

impl From<rosc::OscError> for ServerError {
    fn from(err: rosc::OscError) -> Self {
        Self::OscError(err)
    }
}


#[tokio::main]
async fn main() -> Result<(), io::Error> {
    env_logger::init();

    let addr = env::args().nth(1).unwrap_or("127.0.0.1:4560".to_string());
    Server::new(&addr).await?.run().await
}

#[cfg(test)]
mod tests {
    use crate::timetag_to_unix;

    #[test]
    fn time_tag_to_unix_1() {
        // 2^32 / 2 fractional seconds, i.e. 500,000μs
        assert_eq!(timetag_to_unix(3_608_146_800, 2_147_483_648), (1_399_158_000, 500_000));
    }

    #[test]
    fn time_tag_to_unix_2() {
        assert_eq!(timetag_to_unix(3549086042, 4010129359), (1340097242, 933680));
    }

    #[test]
    fn time_tag_to_unix_seconds_only() {
        assert_eq!(timetag_to_unix(3_608_146_800, 0), (1_399_158_000, 0));
    }

    // TODO: tests for time tags in the past, invalid time tags, once error requirement determined
}
