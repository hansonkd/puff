use clap::{Arg, Command};
use notify::{Config, Event, EventHandler, RecommendedWatcher, RecursiveMode, Result, Watcher};
use std::path::Path;
use std::time::Duration;
use tokio::process::{Child, Command as TokioCommand};
use tokio::sync::mpsc::{channel, Sender};
use tokio::task::JoinHandle;
use tracing::{error, info};

struct EventSender(Sender<bool>);

impl EventHandler for EventSender {
    fn handle_event(&mut self, event: Result<Event>) {
        match event {
            Ok(event) => {
                let is_pycache = event
                    .paths
                    .iter()
                    .find(|p| p.to_str().unwrap_or_default().contains("__pycache__"))
                    .is_some();
                if !event.kind.is_access() && !is_pycache {
                    self.0.try_send(false).unwrap_or_default()
                }
            }
            Err(e) => error!("Watch event error: {}", e),
        }
    }
}

fn main() {
    tracing_subscriber::fmt::init();

    let command = Command::new("puff-watch")
        .about("Watch for file changes and run a command")
        .arg(
            Arg::new("watch")
                .long("watch")
                .value_parser(clap::value_parser!(bool))
                .num_args(0..=1)
                .require_equals(true)
                .default_missing_value("true")
                .help("Monitor a directory for changes."),
        )
        .arg(
            Arg::new("dir")
                .num_args(1)
                .value_name("DIR")
                .short('d')
                .long("dir")
                .help("Directory to monitor.")
                .default_value("."),
        )
        .arg(
            Arg::new("num")
                .num_args(1)
                .value_name("NUM")
                .short('n')
                .long("num")
                .value_parser(clap::value_parser!(usize))
                .help("Number of processes to use.")
                .default_value("1"),
        )
        .arg(
            Arg::new("args")
                .num_args(1..)
                .value_name("ARGS")
                .help("Arguments to pass to puff."),
        );

    let args = command.get_matches();
    let should_watch = args
        .get_one::<bool>("watch")
        .map(|f| f.clone())
        .unwrap_or_else(|| true);
    let dir = args.get_one::<String>("dir").unwrap().clone();
    let num = args.get_one::<usize>("num").unwrap();
    let args = args.get_raw("args").unwrap();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    // Create a channel to receive the events.
    let (sender, mut receiver) = channel(1);

    sender.blocking_send(true).unwrap();

    // Create a watcher object, delivering debounced events.
    // The notification back-end is selected based on the platform.
    let mut watcher =
        RecommendedWatcher::new(EventSender(sender.clone()), Config::default()).unwrap();

    if should_watch {
        info!("Watching directory");

        let path = Path::new(dir.as_str());
        watcher.watch(&path, RecursiveMode::Recursive).unwrap();
    }

    rt.block_on(async {
        let mut children_procs: Vec<Child> = Vec::new();
        let mut pending: Option<JoinHandle<()>> = None;
        while let Some(force) = receiver.recv().await {
            if let Some(task) = &pending {
                if force {
                    task.abort();
                    pending = None;
                } else {
                    continue;
                }
            } else if !force {
                info!("Scheduling reload.\n\n");
                let new_sender = sender.clone();
                let task = tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(1000)).await;
                    new_sender.send(true).await.unwrap_or_default()
                });
                pending = Some(task);

                continue;
            }

            for mut child in children_procs {
                info!("Shutting down: {:?}", child.id());
                if let Err(e) = child.kill().await {
                    error!("Could not kill child process: {}", e);
                }
            }

            children_procs = Vec::with_capacity(*num);

            for _ in 0..*num {
                let mut cli_command = TokioCommand::new("puff");
                cli_command.kill_on_drop(true);
                for part in args.clone() {
                    cli_command.arg(part);
                }
                match cli_command.spawn() {
                    Ok(child) => {
                        info!("Spawned: {:?}", child.id());
                        children_procs.push(child)
                    }
                    Err(err) => error!("Error starting process: {}", err),
                }
            }
        }
    })
}
