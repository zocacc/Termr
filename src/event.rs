use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event};
use tokio::sync::mpsc;
use tokio::task::JoinHandle as TokioJoinHandle;

use crate::app::UiEvent;
use crate::inventory::Inventory;

pub struct EventPump {
    receiver: mpsc::UnboundedReceiver<UiEvent>,
    sender: mpsc::UnboundedSender<UiEvent>,
    cancelled: Arc<AtomicBool>,
    input_thread: Option<JoinHandle<()>>,
    tick_task: TokioJoinHandle<()>,
}

impl EventPump {
    pub fn start() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let cancelled = Arc::new(AtomicBool::new(false));

        let input_sender = sender.clone();
        let input_cancelled = Arc::clone(&cancelled);
        let input_thread = thread::spawn(move || {
            while !input_cancelled.load(Ordering::Relaxed) {
                let Ok(ready) = event::poll(Duration::from_millis(50)) else {
                    continue;
                };
                if !ready {
                    continue;
                }

                let message = match event::read() {
                    Ok(Event::Key(key)) => Some(UiEvent::Key(key)),
                    Ok(Event::Resize(width, height)) => Some(UiEvent::Resize { width, height }),
                    Ok(_) | Err(_) => None,
                };

                if message.is_some_and(|message| input_sender.send(message).is_err()) {
                    break;
                }
            }
        });

        let tick_sender = sender.clone();
        let tick_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            loop {
                interval.tick().await;
                if tick_sender.send(UiEvent::Tick).is_err() {
                    break;
                }
            }
        });

        Self {
            receiver,
            sender,
            cancelled,
            input_thread: Some(input_thread),
            tick_task,
        }
    }

    pub async fn receive(&mut self) -> Option<UiEvent> {
        self.receiver.recv().await
    }

    pub fn reload_inventory(&self, path: PathBuf) {
        let sender = self.sender.clone();
        tokio::task::spawn_blocking(move || {
            let result = Inventory::load(&path).map_err(|error| error.to_string());
            let _ = sender.send(UiEvent::InventoryReloaded(result));
        });
    }
}

impl Drop for EventPump {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.tick_task.abort();
        if let Some(thread) = self.input_thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .context("failed to initialize the asynchronous runtime")
}
