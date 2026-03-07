//! NetstackBridge: server-side userland TCP/IP bridge for Fullhouse (ligolo-like)
//!
//! Responsibilities (SRP):
//! - Own the server TUN device lifetime while active.
//! - Read IP packets from TUN and drive a userland TCP/IP stack (smoltcp when enabled).
//! - For new TCP flows, request the agent to open an outbound socket and bridge payload over the Stream channel.
//! - Forward agent payload back into the stack for retransmit to the TUN.
//!
//! Integration status:
//! - This module wires flow detection, agent stream bridging, and a skeleton smoltcp integration behind
//!   the `netstack_smoltcp` feature. The data-plane is structured for incremental bring-up.

use crate::protocol::Message;
use crate::streaming::models::{ConnectionId, DataDirection, PortMapping, StreamMessage};
use bytes::Bytes;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::{Mutex as StdMutex, OnceLock};
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

// Global inbound hook so server agent reader can deliver Stream messages to the bridge
static INBOUND_STREAM_TX: OnceLock<StdMutex<Option<mpsc::Sender<StreamMessage>>>> = OnceLock::new();

fn inbound_sender_slot() -> &'static StdMutex<Option<mpsc::Sender<StreamMessage>>> {
    INBOUND_STREAM_TX.get_or_init(|| StdMutex::new(None))
}

// 4-tuple flow key for IPv4 TCP
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FlowKey {
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
}

#[derive(Debug)]
struct FlowEntry {
    connection_id: ConnectionId,
    key: FlowKey,
    established: bool,
}

pub struct NetstackBridge {
    tun: tokio_tun::Tun,
    agent_tx: mpsc::Sender<Message>,

    flows: Arc<Mutex<HashMap<FlowKey, FlowEntry>>>,

    // internal channel for agent->stack messages
    inbound_rx: mpsc::Receiver<StreamMessage>,
}

impl NetstackBridge {
    pub fn start(tun: tokio_tun::Tun, agent_tx: mpsc::Sender<Message>) {
        // Channel for inbound agent stream messages
        let (inbound_tx, inbound_rx) = mpsc::channel::<StreamMessage>(2048);
        if let Ok(mut slot) = inbound_sender_slot().lock() {
            *slot = Some(inbound_tx);
        }

        let bridge = Self {
            tun,
            agent_tx,
            flows: Arc::new(Mutex::new(HashMap::new())),
            inbound_rx,
        };

        tokio::spawn(async move { bridge.run().await });
    }

    // Called from agent_connection to deliver streaming messages from the agent.
    // Returns true if the message is for Fullhouse flows and was queued.
    pub async fn try_handle_agent_stream(msg: &StreamMessage) -> bool {
        if let Ok(guard) = inbound_sender_slot().lock() {
            if let Some(tx) = guard.as_ref() {
                // Only handle stream messages relevant to Fullhouse data path
                match msg {
                    StreamMessage::Data { .. }
                    | StreamMessage::Close { .. }
                    | StreamMessage::SetupAck { .. } => {
                        // best-effort enqueue without backpressure to avoid blocking the control path
                        let _ = tx.try_send(msg.clone());
                        return true;
                    }
                    _ => {}
                }
            }
        }
        false
    }

    async fn run(self) {
        info!("Fullhouse: NetstackBridge running (server-side TUN)");

        // Move owned TUN into reader task (no clone available on tokio_tun::Tun)
        let mut tun_reader = self.tun;
        let flows = self.flows.clone();
        let agent_tx = self.agent_tx.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            loop {
                match tun_reader.read(&mut buf).await {
                    Ok(0) => continue,
                    Ok(n) => {
                        let payload = &buf[..n];
                        if let Some((key, tcp_off, _ip_off)) = parse_ipv4_tcp_offsets(payload) {
                            debug!(len = n, ?key, "TUN frame received");

                            let mut flows_lock = flows.lock().await;
                            if let std::collections::hash_map::Entry::Vacant(e) =
                                flows_lock.entry(key)
                            {
                                // New flow detected -> allocate connection_id and notify agent
                                let connection_id = uuid::Uuid::new_v4();
                                let mapping = PortMapping {
                                    local_port: 0,
                                    target_host: key.dst_ip.to_string(),
                                    target_port: key.dst_port,
                                };
                                let setup = StreamMessage::Setup {
                                    connection_id,
                                    mapping,
                                };
                                let _ = agent_tx.send(Message::Stream(setup)).await;

                                e.insert(FlowEntry {
                                    connection_id,
                                    key,
                                    established: false,
                                });
                            } else {
                                // Existing flow -> if established, forward payload (if any)
                                if let Some(entry) = flows_lock.get(&key) {
                                    if entry.established {
                                        if let Some(data) = tcp_payload_slice(payload, tcp_off) {
                                            if !data.is_empty() {
                                                let _ = agent_tx
                                                    .send(Message::Stream(StreamMessage::Data {
                                                        connection_id: entry.connection_id,
                                                        payload: Bytes::copy_from_slice(data),
                                                        direction: DataDirection::ClientToTarget,
                                                    }))
                                                    .await;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!("NetstackBridge TUN read error: {}", e);
                        break;
                    }
                }
            }
            info!("Fullhouse: NetstackBridge TUN reader stopped");
        });

        // Inbound agent->stack messages
        let flows_for_agent = self.flows.clone();
        let mut inbound_rx = self.inbound_rx;
        tokio::spawn(async move {
            while let Some(msg) = inbound_rx.recv().await {
                match msg {
                    StreamMessage::Data {
                        connection_id,
                        payload,
                        direction,
                    } => {
                        if direction == DataDirection::TargetToClient {
                            // Lookup flow and write to stack/TUN
                            let key_opt = {
                                let map = flows_for_agent.lock().await;
                                map.values()
                                    .find(|f| f.connection_id == connection_id)
                                    .map(|f| f.key)
                            };
                            if let Some(_key) = key_opt {
                                // For now, write raw payload handling is not implemented.
                                // Future: feed into smoltcp socket rx buffer and let the stack emit IP packets.
                                debug!(cid = %connection_id, len = payload.len(), "Agent->client payload received");
                                // Placeholder: drop or echo logic could go here.
                            }
                        }
                    }
                    StreamMessage::Close { connection_id, .. } => {
                        let mut map = flows_for_agent.lock().await;
                        if let Some(k) = map
                            .iter()
                            .find(|(_k, v)| v.connection_id == connection_id)
                            .map(|(k, _)| *k)
                        {
                            map.remove(&k);
                        }
                    }
                    StreamMessage::SetupAck {
                        connection_id,
                        success,
                        error_message,
                    } => {
                        if !success {
                            warn!(cid = %connection_id, err = ?error_message, "Agent failed to setup target socket");
                        } else {
                            let mut map = flows_for_agent.lock().await;
                            if let Some((_k, entry)) = map
                                .iter_mut()
                                .find(|(_k, v)| v.connection_id == connection_id)
                            {
                                entry.established = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            info!("Fullhouse: NetstackBridge inbound stream handler stopped");
        });

        #[cfg(feature = "netstack_smoltcp")]
        {
            warn!(
                "netstack_smoltcp feature enabled, but smoltcp bridging is currently disabled in NetstackBridge"
            );
        }
    }
}

// Minimal IPv4/TCP header parsing to detect flows (no checksum validation)
// Removed unused IPv4 4-tuple parser (using offsets variant below)

// Returns (flow_key, tcp_offset, ip_offset)
fn parse_ipv4_tcp_offsets(pkt: &[u8]) -> Option<(FlowKey, usize, usize)> {
    if pkt.len() < 20 {
        return None;
    }
    let version = pkt[0] >> 4;
    if version != 4 {
        return None;
    }
    let ihl = (pkt[0] & 0x0f) as usize * 4;
    if pkt.len() < ihl + 20 {
        return None;
    }
    let proto = pkt[9];
    if proto != 6 {
        return None;
    }
    let src_ip = Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15]);
    let dst_ip = Ipv4Addr::new(pkt[16], pkt[17], pkt[18], pkt[19]);
    let tcp_off = ihl;
    let tcp = &pkt[tcp_off..];
    let src_port = u16::from_be_bytes([tcp[0], tcp[1]]);
    let dst_port = u16::from_be_bytes([tcp[2], tcp[3]]);
    Some((
        FlowKey {
            src_ip,
            dst_ip,
            src_port,
            dst_port,
        },
        tcp_off,
        0,
    ))
}

fn tcp_payload_slice(pkt: &[u8], tcp_off: usize) -> Option<&[u8]> {
    if pkt.len() < tcp_off + 20 {
        return None;
    }
    let data_offset = (pkt[tcp_off + 12] >> 4) as usize * 4; // upper 4 bits in offset/flags
    let start = tcp_off + data_offset;
    if pkt.len() < start {
        return None;
    }
    Some(&pkt[start..])
}
