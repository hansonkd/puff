use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use anyhow::anyhow;
use crate::errors::PuffResult;
use crate::runtime::{yield_to_future, yield_to_future_io};
use bb8_redis::bb8::Pool;
use bb8_redis::redis::{AsyncCommands, Connection, FromRedisValue, IntoConnectionInfo, Msg};
use bb8_redis::redis::aio::PubSub;
use bb8_redis::RedisConnectionManager;
use futures::StreamExt;
use crate::context::{supervised_task, with_puff_context};
use crate::types::{Bytes, Puff, Text};
pub use bb8_redis::redis::Cmd;
use clap::{Arg, Command};
use futures_util::stream::{BoxStream, Next};
use tokio::runtime::Handle;
use tracing::{error, info, warn};
use tracing::instrument::WithSubscriber;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{Sender, UnboundedSender, UnboundedReceiver};
use tokio::task::JoinHandle;
use serde::{Serialize, Deserialize};
use futures_util::FutureExt;

type ConnectionId = uuid::Uuid;

enum PubSubEvent {
    Sub(Text, ConnectionId, UnboundedSender<PubSubMessage>),
    UnSub(Text, ConnectionId),
}

///
#[derive(Clone, Serialize, Deserialize)]
pub struct PubSubMessage {
    channel: Text,
    from: ConnectionId,
    body: Bytes
}

impl Puff for PubSubMessage {}

impl PubSubMessage {
    fn new(channel: Text, from: ConnectionId, body: Bytes) -> Self {
        Self {
            channel, from, body
        }
    }

    /// Body of the message
    pub fn body(&self) -> Bytes { self.body.clone() }

    /// Body as Text
    pub fn text(&self) -> Option<Text> { Text::from_utf8(self.body.as_ref())}

    /// What channel the message was sent on.
    pub fn channel(&self) -> Text { self.channel.clone() }
    /// What PubSubConnection sent the message.
    pub fn from(&self) -> ConnectionId { self.from.clone() }
}

/// A client to work with PubSub. A pubsub client currently is assumed to be alive for the lifetime
/// of a program and maintains a single persistent connection to Redis.
///
/// PubSubConnections do not create new Redis connections instead share the same one and the
/// client broadcasts new messages over unbounded channels.
#[derive(Clone)]
pub struct PubSubClient {
    client: Pool<RedisConnectionManager>,
    task_name: Text,
    /// Note event sender should be bounded so we don't lose messages.
    events_sender: Arc<Mutex<Option<Sender<PubSubEvent>>>>,
    channels: Arc<Mutex<HashMap<Text, HashMap<ConnectionId, UnboundedSender<PubSubMessage>>>>>
}


impl Puff for PubSubClient {}


async fn handle_event(client: &PubSubClient, event: PubSubEvent, pubsub: &mut PubSub) -> PuffResult<()> {
    match event {
        PubSubEvent::Sub(chan, conn, sender) => {
            let maybe_sub = {
                let mut mutex_guard = client.channels.lock().unwrap();
                    match mutex_guard.get_mut(&chan) {
                    Some(v) => {
                        v.insert(conn, sender);
                        None
                    }
                    None => {
                        mutex_guard.insert(chan.clone(),HashMap::from([(conn, sender)]));
                        Some(chan)
                    }
                }
            };
            match maybe_sub {
                Some(chan) => pubsub.subscribe(chan).await?,
                None => (),
            }

        }
        PubSubEvent::UnSub(chan, conn) => {
            let maybe_unsub = {
                let mut mutex_guard = client.channels.lock().unwrap();
                if let Some(v) = mutex_guard.get_mut(&chan) {
                    v.remove(&conn);
                    if v.is_empty() {
                        mutex_guard.remove(&chan);
                        Some(chan)
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            match maybe_unsub {
                Some(chan) => pubsub.unsubscribe(chan).await?,
                None => (),
            }
        }
    }
    Ok(())
}

impl PubSubClient {
    pub fn new<T: IntoConnectionInfo + Send + 'static>(conn: T) -> PuffResult<Self> {
        let client = yield_to_future_io(new_pubsub_async(conn, false))??;
        Ok(client)
    }

    pub fn start_supervised_listener(&self) {
        let task_name = self.task_name.clone();
        let inner_client = self.clone();
        with_puff_context(move |ctx| {
            supervised_task(ctx, task_name, move || {
                let inner_client = inner_client.clone();
                let fut = async move {
                    let client = inner_client.client.dedicated_connection().await?;
                    let mut pubsub = client.into_pubsub();
                    {
                        let vec: Vec<Text> = {
                            let mut mutex_guard = inner_client.channels.lock().unwrap();
                            mutex_guard.keys().map(|c| c.clone()).collect()
                        };

                        for channel in vec {
                            pubsub.subscribe(channel).await?
                        }
                    }

                    let (events, mut new_events) = mpsc::channel(1);

                    {
                        let mut s_mutex = inner_client.events_sender.lock().unwrap();
                        *s_mutex = Some(events);
                    }

                    loop {
                        let mut on_message = pubsub.on_message();
                        tokio::select! {
                            Some(msg) = on_message.next() => {
                                inner_client.handle_msg(msg)
                            },
                            Some(event) = new_events.recv() => {
                                drop(on_message);
                                handle_event(&inner_client, event, &mut pubsub).await?;
                            }
                            else => {
                                warn!("Got no message in pubsub loop... Restarting loop.");
                                break;
                            }
                        }
                    }

                    Ok(())
                };
                fut.boxed()
            })
        })
    }

    fn handle_msg(&self, msg: Msg) {
        match bincode::deserialize::<PubSubMessage>(msg.get_payload_bytes()) {
            Ok(pubsub_msg) => {
                let mut hm = self.channels.lock().unwrap();
                if let Some(new_hm) = hm.get_mut(&pubsub_msg.channel) {
                    new_hm.retain(|conn, sender| {
                        sender.send(pubsub_msg.puff()).is_ok()
                    })
                };
            }
            Err(_e) => {
                error!("Got an unexpected error deserializing pubsub message {_e}")
            }
        }
    }

    /// Create a connection that can subscribe to channels.
    pub fn connection(&self) -> PuffResult<(PubSubConnection, UnboundedReceiver<PubSubMessage>)> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let conn = PubSubConnection{
            connection_id: uuid::Uuid::new_v4(),
            sender,
            client: self.client.clone(),
            events_sender: self.events_sender.clone()
        };
        Ok((conn, receiver))
    }
}


/// A connection that can subscribe to new messages.
pub struct PubSubConnection {
    connection_id: ConnectionId,
    client: Pool<RedisConnectionManager>,
    sender: UnboundedSender<PubSubMessage>,
    events_sender: Arc<Mutex<Option<Sender<PubSubEvent>>>>,
}

impl PubSubConnection {
    /// Get the ConnectionId, useful for filtering messages from yourself.
    pub fn who_am_i(&self) -> ConnectionId {
        self.connection_id.clone()
    }

    /// Subscribe to the channel. Queues the command even if you don't await the handle.
    pub fn subscribe<T: Into<Text>>(&self, channel: T) -> JoinHandle<PuffResult<()>> {
        let new_sender = self.sender.clone();
        let event = PubSubEvent::Sub(channel.into(), self.connection_id.clone(), new_sender);
        let inner_sender = self.events_sender.clone();
        with_puff_context(move |ctx| {
            ctx.handle().spawn(async move {
                let s = {
                    let m = inner_sender.lock().unwrap();
                    (*m).clone().expect("Pub loop not started yet.")
                };
                let r = s.send(event).await;
                r.map_err(|_| anyhow!("Could not send subscribe message"))
            })
        })
    }

    /// Unsubscribe from the channel. Queues the command even if you don't await the handle.
    pub fn unsubscribe<T: Into<Text>>(&self, channel: T) -> JoinHandle<PuffResult<()>> {
        let event = PubSubEvent::UnSub(channel.into(), self.connection_id.clone());
        let inner_sender = self.events_sender.clone();
        with_puff_context(move |ctx| {
            ctx.handle().spawn(async move {
                let s = {
                    let m = inner_sender.lock().unwrap();
                    (*m).clone().expect("Sub loop not started yet.")
                };

                let r = s.send(event).await;
                r.map_err(|_| anyhow!("Could not send subscribe message"))
            })
        })
    }

    /// Try to broadcast. Will run whether you await result or not.
    pub fn publish<T: Into<Text>, M: Into<Bytes>>(&self, channel: T, body: M) -> JoinHandle<PuffResult<()>> {
        let channel_text = channel.into();
        let message = PubSubMessage::new(channel_text.clone(), self.connection_id.clone(), body.into());

        with_puff_context(|ctx| {
            let inner_client = self.client.clone();
            ctx.handle().spawn(async move {
                let inner_client = inner_client.clone();
                let body_bytes = bincode::serialize(&message)?;
                let mut conn = inner_client.get().await?;
                info!("Publishing to Redis...");
                let r: () = conn.publish(channel_text, body_bytes).await?;
                info!("Done.");
                Ok(r)
            })
        })
    }
}

/// Build a new PubSubClient with the provided connection information.
pub async fn new_pubsub_async<T: IntoConnectionInfo>(conn: T, check: bool) -> PuffResult<PubSubClient> {
    let conn_info = conn.into_connection_info()?;
    let manager = RedisConnectionManager::new(conn_info.clone())?;
    let pool = Pool::builder().build(manager).await?;
    let local_pool = pool.clone();
    if check {
        info!("Checking PubSub connectivity...");
        let check_fut = async {
            let mut conn = local_pool.get().await?;
            PuffResult::Ok(Cmd::new().arg("PING").query_async(&mut *conn).await?)
        };

        tokio::time::timeout(Duration::from_secs(5), check_fut).await??;
        info!("PubSub looks good.");
    }
    let task_name = format!("pubsub-listener-{}", conn_info.addr).into();
    let channels = Arc::new(Mutex::new(HashMap::new()));
    let events_sender = Arc::new(Mutex::new(None));
    let client = PubSubClient { task_name, channels, events_sender, client: pool };
    Ok(client)
}


pub(crate) fn add_pubsub_command_arguments(command: Command) -> Command {
    command.arg(
        Arg::new("pubsub_url")
            .long("pubsub-url")
            .takes_value(true)
            .value_name("PUBSUB_URL")
            .env("PUFF_PUBSUB_URL")
            .default_value("redis://localhost:6379")
            .help("Global Redis PubSub configuration."),
    )
}
