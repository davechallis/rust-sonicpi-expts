use std::error::Error;
use std::{env, io};
use tokio::sync;
use tokio::net::UdpSocket;
use std::time::Duration;
use chrono::offset::TimeZone;
use log::{debug, info, warn};
use futures::select;
use futures::future::FutureExt;
use std::collections::HashMap;


// Delta between NTP epoch (1900-01-01 00:00:00) and Unix epoch (1970-01-01 00:00:00).
// Contains 53 non-leap years, and 17 leap years, in seconds, this is:
// (53 * 365 + 17 * 366) * 86400 = 2208988800.
const EPOCH_DELTA: u64 = 2_208_988_800;

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
    socket: UdpSocket,
    buf: Vec<u8>,
}

impl Server {
    // TODO: implement server constructor
    // fn new() -> Self { }

    async fn run(self) -> Result<(), io::Error> {
        let Server { mut socket, mut buf } = self;

        // TODO: move to server constructor
        // Maps a tag name to sender/receiver pair. Used for signalling cancellations.
        let mut tags: HashMap<&str, (sync::watch::Sender<bool>, sync::watch::Receiver<bool>)> = HashMap::new();

        debug!("Starting main event loop");
        loop {
            debug!("Waiting on OSC message...");
            let (size, _) = socket.recv_from(&mut buf).await?;

            // TODO: error handling for badly formed OSC messages
            let osccmd = rosc::decoder::decode(&buf[..size]).expect("OSC decoding failed");
            match osccmd {
                rosc::OscPacket::Message(msg) => {
                    debug!("Received OSC Message: {:?}", msg);

                    // TODO: parse `/flush` messages to get specific tags, currently hardcoded to "foo"
                    let tag = "foo";

                    // Remove tag entry from hash map, and send termination signal to all listening
                    // receivers.
                    if let Some((_k, (tx, _rx))) = tags.remove_entry(tag) {
                        debug!("Flushing tag: {}", tag);
                        tx.broadcast(true).unwrap_or_else(|e| {
                            warn!("Failed to broadcast: {}", e);
                        });
                    }
                },
                rosc::OscPacket::Bundle(bundle) => {
                    if let rosc::OscType::Time(a, b) = bundle.timetag {
                        let (_tx, rx) = tags.entry("foo").or_insert(tokio::sync::watch::channel(false));

                        debug!("Received OSC Bundle: {:?}", bundle);
                        let dur = timetag_to_duration(a, b);

                        debug!("Sending OSC command in: {}ms", dur.as_millis());

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
                                    _ = tokio::time::delay_for(dur).fuse() => {
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

                            // TODO: parse bundle structure to determine what to send, this just
                            // sends fixed message for testing.
                            let new_msg = rosc::OscMessage {
                                addr: "/*/note_on".to_owned(),
                                args: vec![rosc::OscType::Int(-1),
                                           rosc::OscType::Int(40),
                                           rosc::OscType::Int(127)],
                            };
                            let packet = rosc::OscPacket::Message(new_msg);

                            // TODO: error handling
                            let new_buf = rosc::encoder::encode(&packet).expect("encoding failed");

                            // TODO: addr should be configurable here
                            // TODO: error handling
                            socket2.send_to(&new_buf, "127.0.0.1:4561").await.expect("send to failed!");
                            debug!("OSC command sent");
                        });
                    }
                },
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let addr = env::args().nth(1).unwrap_or("127.0.0.1:4560".to_string());

    debug!("Attempting to bind to: {}", &addr);
    let socket = UdpSocket::bind(&addr).await?;
    info!("Listening on: {}", socket.local_addr()?);

    let server = Server {
        socket,
        buf: vec![0; 1024],
    };

    server.run().await?;
    Ok(())
}
