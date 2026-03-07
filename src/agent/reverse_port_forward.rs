use crate::protocol::Message;
use std::sync::{Arc, OnceLock};
use tokio::sync::{mpsc, Mutex};

// Global channel for sending responses from background tasks to the main agent loop
type ResponseChannel = (mpsc::Sender<Message>, Arc<Mutex<mpsc::Receiver<Message>>>);
static RESPONSE_CHANNEL: OnceLock<ResponseChannel> = OnceLock::new();

pub fn get_response_channel(
) -> &'static (mpsc::Sender<Message>, Arc<Mutex<mpsc::Receiver<Message>>>) {
    RESPONSE_CHANNEL.get_or_init(|| {
        let (tx, rx) = mpsc::channel(1000);
        (tx, Arc::new(Mutex::new(rx)))
    })
}
