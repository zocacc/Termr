use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    operation_generation: Arc<AtomicU64>,
    input_thread: Option<JoinHandle<()>>,
    tick_task: TokioJoinHandle<()>,
}

impl EventPump {
    pub fn start() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let operation_generation = Arc::new(AtomicU64::new(0));

        let input_sender = sender.clone();
        let input_cancelled = Arc::clone(&cancelled);
        let input_thread = thread::spawn(move || {
            while !input_cancelled.load(Ordering::Relaxed) {
                let ready = match event::poll(Duration::from_millis(50)) {
                    Ok(ready) => ready,
                    Err(error) => {
                        let _ = input_sender.send(UiEvent::TerminalFailure(format!(
                            "failed to poll terminal input: {error}"
                        )));
                        break;
                    }
                };
                if !ready {
                    continue;
                }

                let message = match event::read() {
                    Ok(Event::Key(key)) => Some(UiEvent::Key(key)),
                    Ok(Event::Resize(width, height)) => Some(UiEvent::Resize { width, height }),
                    Ok(_) => None,
                    Err(error) => {
                        let _ = input_sender.send(UiEvent::TerminalFailure(format!(
                            "failed to read terminal input: {error}"
                        )));
                        break;
                    }
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
            operation_generation,
            input_thread: Some(input_thread),
            tick_task,
        }
    }

    pub async fn receive(&mut self) -> Option<UiEvent> {
        self.receiver.recv().await
    }

    pub fn reload_inventory(&self, path: PathBuf) {
        let sender = self.sender.clone();
        let operation_generation = Arc::clone(&self.operation_generation);
        let generation = operation_generation.fetch_add(1, Ordering::Relaxed) + 1;
        tokio::task::spawn_blocking(move || {
            let result = Inventory::load(&path);
            if operation_generation.load(Ordering::Relaxed) == generation {
                let _ = sender.send(UiEvent::InventoryReloaded(result));
            }
        });
    }

    pub fn cancel_operation(&self) {
        self.operation_generation.fetch_add(1, Ordering::Relaxed);
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
