use std::sync::Arc;

use event::Event;
use storage::EventStore;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration, MissedTickBehavior};
use tracing::{error, info};

const MAX_BATCH: usize = 500;
const FLUSH_MS: u64 = 100;
const QUEUE_CAPACITY: usize = 10_000;

#[derive(Debug, Error)]
pub enum IngestError {
    #[error("ingestion queue full")]
    QueueFull,
}

#[derive(Clone)]
pub struct IngestHandle {
    tx: mpsc::Sender<Event>,
    live_tx: tokio::sync::broadcast::Sender<Event>,
}

impl IngestHandle {
    pub fn try_enqueue(&self, event: Event) -> Result<(), IngestError> {
        self.tx
            .try_send(event.clone())
            .map_err(|_| IngestError::QueueFull)?;
        let _ = self.live_tx.send(event);
        Ok(())
    }

    pub fn try_enqueue_batch(&self, events: Vec<Event>) -> Result<(), IngestError> {
        for event in events {
            self.try_enqueue(event)?;
        }
        Ok(())
    }

    pub fn subscribe_live(&self) -> tokio::sync::broadcast::Receiver<Event> {
        self.live_tx.subscribe()
    }
}

pub fn start_ingest_worker(store: Arc<EventStore>) -> IngestHandle {
    let (tx, rx) = mpsc::channel::<Event>(QUEUE_CAPACITY);
    let (live_tx, _) = tokio::sync::broadcast::channel(1024);

    tokio::spawn(async move {
        run_writer(store, rx).await;
    });

    IngestHandle { tx, live_tx }
}

async fn run_writer(store: Arc<EventStore>, mut rx: mpsc::Receiver<Event>) {
    let mut buffer: Vec<Event> = Vec::with_capacity(MAX_BATCH);
    let mut tick = interval(Duration::from_millis(FLUSH_MS));
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            maybe = rx.recv() => {
                match maybe {
                    Some(event) => {
                        buffer.push(event);
                        if buffer.len() >= MAX_BATCH {
                            flush(&store, &mut buffer);
                        }
                    }
                    None => {
                        flush(&store, &mut buffer);
                        info!("ingest worker shutting down");
                        break;
                    }
                }
            }
            _ = tick.tick() => {
                if !buffer.is_empty() {
                    flush(&store, &mut buffer);
                }
            }
        }
    }
}

fn flush(store: &EventStore, buffer: &mut Vec<Event>) {
    if buffer.is_empty() {
        return;
    }
    let batch = std::mem::take(buffer);
    if let Err(err) = store.write_batch(&batch) {
        error!("failed to write event batch ({} events): {err}", batch.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn store() -> (tempfile::TempDir, Arc<EventStore>) {
        let dir = tempfile::tempdir().unwrap();
        let store = EventStore::open(dir.path()).unwrap();
        (dir, store)
    }

    fn event(msg: &str) -> Event {
        let mut e = Event::new_log();
        e.message = Some(msg.into());
        e
    }

    async fn wait_for_count(store: &EventStore, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let _ = store.reload();
            let count = store.event_count().unwrap();
            if count == expected {
                return;
            }
            assert!(Instant::now() < deadline, "timed out: {count} of {expected} events written");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn single_event_is_flushed_by_timer() {
        let (_dir, store) = store();
        let handle = start_ingest_worker(store.clone());
        handle.try_enqueue(event("hello")).unwrap();
        wait_for_count(&store, 1).await;
    }

    #[tokio::test]
    async fn batch_larger_than_max_batch_is_fully_written() {
        let (_dir, store) = store();
        let handle = start_ingest_worker(store.clone());
        let events: Vec<_> = (0..MAX_BATCH * 2 + 17).map(|i| event(&format!("e{i}"))).collect();
        handle.try_enqueue_batch(events).unwrap();
        wait_for_count(&store, MAX_BATCH * 2 + 17).await;
    }

    #[tokio::test]
    async fn live_subscribers_receive_enqueued_events() {
        let (_dir, store) = store();
        let handle = start_ingest_worker(store);
        let mut rx = handle.subscribe_live();
        let e = event("live");
        let id = e.id;
        handle.try_enqueue(e).unwrap();
        let got = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await.unwrap().unwrap();
        assert_eq!(got.id, id);
    }

    #[tokio::test]
    async fn enqueue_without_subscribers_succeeds() {
        let (_dir, store) = store();
        let handle = start_ingest_worker(store);
        // broadcast::send errors with no receivers; that must not surface as an ingest error.
        handle.try_enqueue(event("nobody listening")).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn full_queue_reports_queue_full() {
        let (_dir, store) = store();
        // On a current-thread runtime the worker can't drain while we hold the thread.
        let handle = start_ingest_worker(store);
        for i in 0..QUEUE_CAPACITY {
            handle.try_enqueue(event(&format!("e{i}"))).unwrap();
        }
        assert!(matches!(handle.try_enqueue(event("overflow")), Err(IngestError::QueueFull)));
    }

    #[tokio::test]
    async fn worker_flushes_remaining_events_on_shutdown() {
        let (_dir, store) = store();
        let (tx, rx) = mpsc::channel(16);
        let worker = tokio::spawn(run_writer(store.clone(), rx));
        tx.send(event("a")).await.unwrap();
        tx.send(event("b")).await.unwrap();
        drop(tx);
        tokio::time::timeout(Duration::from_secs(5), worker).await.unwrap().unwrap();
        let _ = store.reload();
        assert_eq!(store.event_count().unwrap(), 2);
    }
}
