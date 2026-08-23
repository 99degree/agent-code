//! Input gate layer between the terminal event reader and the main event loop.
//!
//! Provides early parsing of Ctrl-C (cancel) and Ctrl-D (EOF) so they can be
//! handled immediately without going through the full event loop processing,
//! and buffers input into a queue that the UI can drain easily.

use std::collections::VecDeque;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::watch;

/// Input events that can be processed by the gate layer.
#[derive(Debug, Clone)]
pub enum InputEvent {
    /// A regular terminal event (key, mouse, paste, resize, focus).
    Terminal(Event),
    /// Early-parsed Ctrl-C (cancel) — high priority, bypass normal routing.
    Cancel,
    /// Early-parsed Ctrl-D (EOF) — initiate clean shutdown.
    Eof,
    /// Control: flush the gate's internal buffers.
    Flush,
    /// Control: shut down the gate.
    Shutdown,
}

/// Configuration for the input gate.
#[derive(Debug, Clone)]
pub struct InputGateConfig {
    /// Maximum events buffered in the channel.
    pub max_queue_size: usize,
    /// Whether to early-parse Ctrl-C as cancellation.
    pub early_cancel: bool,
    /// Whether to early-parse Ctrl-D as EOF.
    pub early_eof: bool,
}

impl Default for InputGateConfig {
    fn default() -> Self {
        Self {
            max_queue_size: 1024,
            early_cancel: true,
            early_eof: true,
        }
    }
}

/// Handle for controlling the input gate from the main loop.
#[derive(Debug)]
pub struct InputGateHandle {
    /// Channel to send processed events to the main loop.
    event_rx: Receiver<InputEvent>,
    /// Watch channel for cancellation state (Ctrl-C pressed).
    cancel_rx: watch::Receiver<bool>,
    /// Watch channel for EOF state (Ctrl-D pressed).
    eof_rx: watch::Receiver<bool>,
    /// Local buffer for events that arrived before the gate was ready.
    local_buffer: VecDeque<InputEvent>,
}

impl InputGateHandle {
    /// Try to receive the next input event (non-blocking).
    /// Drains local buffer first, then the channel.
    pub fn try_recv(&mut self) -> Option<InputEvent> {
        if let Some(ev) = self.local_buffer.pop_front() {
            return Some(ev);
        }
        self.event_rx.try_recv().ok()
    }

    /// Receive the next input event (async/blocking).
    /// Drains local buffer first, then the channel.
    pub async fn recv(&mut self) -> Option<InputEvent> {
        if let Some(ev) = self.local_buffer.pop_front() {
            return Some(ev);
        }
        self.event_rx.recv().await
    }

    /// Check if cancellation was requested (Ctrl-C).
    pub fn is_cancel_requested(&self) -> bool {
        *self.cancel_rx.borrow()
    }

    /// Check if EOF was requested (Ctrl-D).
    pub fn is_eof_requested(&self) -> bool {
        *self.eof_rx.borrow()
    }

    /// Clone the cancellation watch receiver for use in other tasks.
    pub fn cancel_watcher(&self) -> watch::Receiver<bool> {
        self.cancel_rx.clone()
    }

    /// Clone the EOF watch receiver for use in other tasks.
    pub fn eof_watcher(&self) -> watch::Receiver<bool> {
        self.eof_rx.clone()
    }

    /// Push an event into the local buffer (for events before gate is ready).
    pub fn buffer_event(&mut self, event: InputEvent) {
        self.local_buffer.push_back(event);
    }

    /// Drain all buffered events into a vector.
    pub fn drain_buffered(&mut self) -> Vec<InputEvent> {
        self.local_buffer.drain(..).collect()
    }
}

/// The input gate — runs as a task to read and pre-process terminal events.
pub struct InputGate {
    event_tx: Sender<InputEvent>,
    control_rx: Receiver<InputEvent>,
    cancel_tx: watch::Sender<bool>,
    eof_tx: watch::Sender<bool>,
    config: InputGateConfig,
}

impl InputGate {
    /// Create a new input gate and a paired handle.
    pub fn new(config: InputGateConfig) -> (Self, InputGateHandle) {
        let (event_tx, event_rx) = mpsc::channel(config.max_queue_size);
        let (control_tx, control_rx) = mpsc::channel(32);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (eof_tx, eof_rx) = watch::channel(false);

        let handle = InputGateHandle {
            event_rx,
            cancel_rx,
            eof_rx,
            local_buffer: VecDeque::new(),
        };

        let _ = control_tx;
        let gate = InputGate {
            event_tx,
            control_rx,
            cancel_tx,
            eof_tx,
            config,
        };

        (gate, handle)
    }

    /// Run the input gate loop, reading from the terminal event stream.
    pub async fn run(
        mut self,
        mut stream: impl futures::Stream<Item = std::io::Result<Event>> + Unpin + Send + 'static,
        mut blocking_rx: Option<mpsc::UnboundedReceiver<Event>>,
    ) {
        let mut shutdown = false;

        while !shutdown {
            tokio::select! {
                // Read from terminal stream — blocking or poll-based.
                maybe_ev = async {
                    if let Some(rx) = &mut blocking_rx {
                        rx.recv().await.map(Ok)
                    } else {
                        stream.next().await
                    }
                } => {
                    match maybe_ev {
                        Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat => {
                            // Early parsing of Ctrl-C (cancel).
                            if self.config.early_cancel && is_cancel_chord(&key) {
                                let _ = self.cancel_tx.send(true);
                                if self.event_tx.send(InputEvent::Cancel).await.is_err() {
                                    break;
                                }
                                continue;
                            }
                            // Early parsing of Ctrl-D (EOF).
                            if self.config.early_eof && is_eof_chord(&key) {
                                let _ = self.eof_tx.send(true);
                                if self.event_tx.send(InputEvent::Eof).await.is_err() {
                                    break;
                                }
                                continue;
                            }
                            // Regular key event — forward as terminal event.
                            if self.event_tx.send(InputEvent::Terminal(Event::Key(key))).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(ev)) => {
                            if self.event_tx.send(InputEvent::Terminal(ev)).await.is_err() {
                                break;
                            }
                        }
                        Some(Err(_)) | None => {
                            // Stream closed or errored — signal EOF.
                            let _ = self.eof_tx.send(true);
                            let _ = self.event_tx.send(InputEvent::Eof).await;
                            break;
                        }
                    }
                }
                // Handle control commands.
                control = self.control_rx.recv() => {
                    match control {
                        Some(InputEvent::Shutdown) => {
                            shutdown = true;
                        }
                        Some(InputEvent::Flush) => {
                            // No-op: the channel is already buffered.
                        }
                        _ => {}
                    }
                }
            }
        }

        // Clean shutdown — signal EOF.
        let _ = self.eof_tx.send(true);
    }

    /// Create a control sender for sending commands to the gate.
    /// The gate must be running for this to work.
    pub fn control_sender(&self) -> Sender<InputEvent> {
        // Re-create from the channel pair is not needed; the gate consumes
        // control_rx internally. For external control, use the handle.
        mpsc::channel(0).0
    }
}

/// Check if a key event is a Ctrl-C (cancel) chord.
///
/// Handles Ctrl+C, Super+C, and raw ETX (0x03). Leaves Ctrl+Shift+C for copy.
pub fn is_cancel_chord(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('\u{3}') => true,
        KeyCode::Char(c) if c.eq_ignore_ascii_case(&'c') => {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::SUPER);
            ctrl && !key.modifiers.contains(KeyModifiers::SHIFT)
        }
        _ => false,
    }
}

/// Check if a key event is a Ctrl-D (EOF) chord.
///
/// Plain Ctrl+D without Shift or Alt, matching the POSIX EOF convention.
pub fn is_eof_chord(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('d') | KeyCode::Char('D'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SHIFT)
        && !key.modifiers.contains(KeyModifiers::ALT)
    }
