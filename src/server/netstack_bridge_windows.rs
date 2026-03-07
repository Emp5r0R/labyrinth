//! Windows Wintun-backed NetstackBridge for Fullhouse.

use crate::protocol::Message;
use crate::streaming::models::{ConnectionId, DataDirection, PortMapping, StreamMessage};
use bytes::Bytes;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};
use wintun::{Adapter, Session, MAX_RING_CAPACITY};

static INBOUND_STREAM_TX: OnceLock<StdMutex<Option<mpsc::Sender<StreamMessage>>>> = OnceLock::new();

fn inbound_sender_slot() -> &'static StdMutex<Option<mpsc::Sender<StreamMessage>>> {
    INBOUND_STREAM_TX.get_or_init(|| StdMutex::new(None))
}

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

pub struct WindowsNetstackBridge {
    agent_tx: mpsc::Sender<Message>,
    flows: Arc<Mutex<HashMap<FlowKey, FlowEntry>>>,
    inbound_rx: mpsc::Receiver<StreamMessage>,
}

impl WindowsNetstackBridge {
    pub fn start(adapter_name: &str, agent_tx: mpsc::Sender<Message>) -> Result<(), String> {
        let wintun =
            unsafe { wintun::load() }.map_err(|e| format!("Failed to load wintun.dll: {}", e))?;

        let adapter = match Adapter::open(&wintun, adapter_name) {
            Ok(a) => a,
            Err(_) => Adapter::create(&wintun, adapter_name, "Labyrinth", None)
                .map_err(|e| format!("Failed to create Wintun adapter: {}", e))?,
        };

        let session = Arc::new(
            adapter
                .start_session(MAX_RING_CAPACITY)
                .map_err(|e| format!("Failed to start Wintun session: {}", e))?,
        );

        let (inbound_tx, inbound_rx) = mpsc::channel::<StreamMessage>(2048);
        if let Ok(mut slot) = inbound_sender_slot().lock() {
            *slot = Some(inbound_tx);
        }

        let bridge = Self {
            agent_tx,
            flows: Arc::new(Mutex::new(HashMap::new())),
            inbound_rx,
        };

        tokio::spawn(async move { bridge.run(session).await });
        Ok(())
    }

    pub async fn try_handle_agent_stream(msg: &StreamMessage) -> bool {
        if let Ok(guard) = inbound_sender_slot().lock() {
            if let Some(tx) = guard.as_ref() {
                match msg {
                    StreamMessage::Data { .. }
                    | StreamMessage::Close { .. }
                    | StreamMessage::SetupAck { .. } => {
                        let _ = tx.try_send(msg.clone());
                        return true;
                    }
                    _ => {}
                }
            }
        }
        false
    }

    async fn run(self, session: Arc<Session>) {
        info!("Fullhouse: WindowsNetstackBridge running (Wintun)");

        let (packet_tx, mut packet_rx) = mpsc::channel::<Vec<u8>>(2048);
        std::thread::spawn(move || loop {
            match session.receive_blocking() {
                Ok(pkt) => {
                    let data = pkt.bytes().to_vec();
                    if packet_tx.blocking_send(data).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    warn!("Wintun receive error: {}", e);
                    break;
                }
            }
        });

        let flows = self.flows.clone();
        let agent_tx = self.agent_tx.clone();
        tokio::spawn(async move {
            while let Some(payload) = packet_rx.recv().await {
                if let Some((key, tcp_off, _)) = parse_ipv4_tcp_offsets(&payload) {
                    debug!(len = payload.len(), ?key, "Wintun frame received");
                    let mut flows_lock = flows.lock().await;
                    if let std::collections::hash_map::Entry::Vacant(e) = flows_lock.entry(key) {
                        let connection_id = uuid::Uuid::new_v4();
                        let mapping = PortMapping {
                            local_port: 0,
                            target_host: key.dst_ip.to_string(),
                            target_port: key.dst_port,
                        };
                        let _ = agent_tx
                            .send(Message::Stream(StreamMessage::Setup {
                                connection_id,
                                mapping,
                            }))
                            .await;
                        e.insert(FlowEntry {
                            connection_id,
                            key,
                            established: false,
                        });
                    } else if let Some(entry) = flows_lock.get(&key) {
                        if entry.established {
                            if let Some(data) = tcp_payload_slice(&payload, tcp_off) {
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
        });

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
                            let key_opt = {
                                let map = flows_for_agent.lock().await;
                                map.values()
                                    .find(|f| f.connection_id == connection_id)
                                    .map(|f| f.key)
                            };
                            if let Some(_key) = key_opt {
                                debug!(
                                    cid = %connection_id,
                                    len = payload.len(),
                                    "Agent->client payload received (windows)"
                                );
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
                            warn!(
                                cid = %connection_id,
                                err = ?error_message,
                                "Agent failed to setup target socket"
                            );
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
        });
    }
}

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
    if pkt[9] != 6 {
        return None;
    }
    let src_ip = Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15]);
    let dst_ip = Ipv4Addr::new(pkt[16], pkt[17], pkt[18], pkt[19]);
    let tcp = &pkt[ihl..];
    let src_port = u16::from_be_bytes([tcp[0], tcp[1]]);
    let dst_port = u16::from_be_bytes([tcp[2], tcp[3]]);
    Some((
        FlowKey {
            src_ip,
            dst_ip,
            src_port,
            dst_port,
        },
        ihl,
        0,
    ))
}

fn tcp_payload_slice(pkt: &[u8], tcp_off: usize) -> Option<&[u8]> {
    if pkt.len() < tcp_off + 20 {
        return None;
    }
    let data_offset = (pkt[tcp_off + 12] >> 4) as usize * 4;
    let start = tcp_off + data_offset;
    if pkt.len() < start {
        return None;
    }
    Some(&pkt[start..])
}
