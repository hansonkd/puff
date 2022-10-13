use puff_rs::axum::extract::ws::{Message, WebSocket};
use puff_rs::axum::extract::WebSocketUpgrade;
use puff_rs::prelude::*;
use puff_rs::program::commands::ServerCommand;

fn main() -> ExitCode {
    let app = Router::new().get("/", root).get("/ws", ws_handler);

    let rc = RuntimeConfig::default().set_pubsub(true);

    Program::new("my_first_ws_app")
        .about("This is my first websocket app")
        .runtime_config(rc)
        .command(ServerCommand::new(app))
        .run()
}

async fn root() -> Text {
    "Ok".to_text()
}

// Handle Websocket connections... Maybe use additional extractors for auth.
async fn ws_handler(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(on_upgrade)
}

async fn on_upgrade(mut socket: WebSocket) {
    let pubsub = with_puff_context(|ctx| ctx.pubsub());
    let (conn, mut rec) = pubsub.connection().expect("No connection");
    let channel_name = "my_pubsub_channel";
    conn.subscribe(channel_name).await.unwrap();

    loop {
        tokio::select! {
            Some(v) = rec.recv() => {
                let text = v.text().unwrap_or("invalid utf8".into());
                let msg = format!("{} said {}", v.from(), text);
                if socket.send(Message::Text(msg)).await.is_err() {
                    // client disconnected
                    return;
                }
            },
            Some(msg) = socket.recv() => {
                if let Ok(msg) = msg {
                    if !conn.publish(channel_name, msg.into_data()).await.unwrap() {
                        break
                    }
                } else {
                    // client disconnected
                    return;
                };
            },
            else => {
                // client disconnected
                break
            }
        }
    }
}
