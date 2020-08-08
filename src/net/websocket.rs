use futures_util::pin_mut;
use futures_util::stream::{StreamExt, TryStreamExt};
use log::{error, warn};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::thread;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::mpsc::error::TryRecvError;
use tungstenite::protocol::Message as WsMessage;

use super::{Message, NetError, Server};

/// HashMap containing the sender channel for the websockets
type Writers = Arc<Mutex<HashMap<
    SocketAddr,
    UnboundedSender<WsMessage>,
>>>;

async fn handle_connection(
    sender: UnboundedSender<(Message, SocketAddr)>,
    writers: Writers,
    stream: TcpStream,
    addr: SocketAddr,
) {
    let ret: Result<(), tungstenite::error::Error> = async {
        // Establish WebSocket
        let ws = tokio_tungstenite::accept_async(stream).await?;
        let (send, recv) = ws.split();

        // Create an MPSC channel. We can't just pass the SplitSink because it
        // is not Sync, so the sending task can't hold on to it across await
        // (for example while it await sends on it)
        let (tx, rx) = unbounded_channel();

        // Insert sender half in the HashMap
        writers.lock().unwrap().insert(addr, tx);

        let forward = rx.map(Ok).forward(send);

        // Get messages, put them in the queue
        let receive = recv.try_for_each(|msg| {
            match msg {
                WsMessage::Text(_) => warn!("Got TEXT message from {}", addr),
                WsMessage::Binary(b) => {
                    match Message::parse(&b) {
                        Some(msg) => sender.send((msg, addr)).unwrap(),
                        None => warn!("Invalid message from {}", addr),
                    }
                }
                WsMessage::Ping(_) => {}
                WsMessage::Pong(_) => {}
                WsMessage::Close(r) => {}
            }
            futures_util::future::ok(())
        });

        pin_mut!(forward, receive);
        futures_util::future::select(forward, receive).await;

        Ok(())
    }.await;
    match ret {
        Ok(()) => {}
        Err(e) => error!("Error from {}: {}", addr, e),
    }
}

async fn handle_writes(
    mut write_queue: UnboundedReceiver<(Message, SocketAddr)>,
    writers: Writers,
) {
    loop {
        let (msg, addr) = match write_queue.recv().await {
            Some(r) => r,
            None => break,
        };

        let mut writers = writers.lock().unwrap();

        // Send message
        match writers.get_mut(&addr) {
            Some(w) => {
                match w.send(WsMessage::Binary(msg.bytes())) {
                    Ok(()) => {}
                    Err(err) => warn!("Error sending to {}: {}", addr, err),
                }
            }
            None => warn!("Can't send message to disconnected {}", addr),
        }
    }
}

/// WebSocket server, accepting connections and starting tasks for them.
async fn server(
    port: u16,
    sender: UnboundedSender<(Message, SocketAddr)>,
    write_queue: UnboundedReceiver<(Message, SocketAddr)>,
) {
    // Writers hashmap, connection handlers will add their sending half to it
    let writers = Arc::new(Mutex::new(HashMap::new()));

    // Start sending task, getting from write_queue and sending to websockets
    tokio::spawn(handle_writes(write_queue, writers.clone()));

    // Create TCP listener
    let unspec = IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0));
    let mut listener = match TcpListener::bind(SocketAddr::new(
        unspec,
        port,
    ))
    .await
    {
        Ok(s) => s,
        Err(e) => panic!("Couldn't listen on port {}: {}", port, e),
    };

    // Accepting loop
    while let Ok((stream, addr)) = listener.accept().await {
        tokio::spawn(handle_connection(sender.clone(), writers.clone(), stream, addr));
    }
}

pub struct WebsocketServer {
    recv_queue: UnboundedReceiver<(Message, SocketAddr)>,
    write_queue: UnboundedSender<(Message, SocketAddr)>
}

impl WebsocketServer {
    pub fn new(port: u16) -> WebsocketServer {
        let (recv_sender, recv_recv) = unbounded_channel();
        let (write_send, write_recv) = unbounded_channel();
        thread::spawn(move || {
            let mut rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                server(port, recv_sender, write_recv).await;
            });
        });
        WebsocketServer {
            recv_queue: recv_recv,
            write_queue: write_send,
        }
    }
}

impl Server for WebsocketServer {
    type Address = SocketAddr;

    fn send(&self, msg: &Message, addr: &SocketAddr) -> Result<(), NetError> {
        // Add it to the queue, handle_writes() task will send it
        self.write_queue.send((msg.to_owned(), addr.clone())).unwrap();
        Ok(())
    }

    fn recv(&mut self) -> Result<(Message, SocketAddr), NetError> {
        match self.recv_queue.try_recv() {
            Err(TryRecvError::Empty) => Err(NetError::FlowControl),
            Err(TryRecvError::Closed) => panic!("Network thread error"),
            Ok((msg, src)) => {
                Ok((msg, src))
            }
        }
    }
}
