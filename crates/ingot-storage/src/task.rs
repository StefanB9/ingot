use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{info, instrument};

use crate::{event::StorageEvent, service::StorageService};

/// Run the storage background task. Receives events from the engine and
/// persists them to `PostgreSQL` (batched).
///
/// Exits when the channel is closed (all senders dropped).
#[instrument(skip_all, fields(batch_size, flush_interval_ms))]
pub async fn run_storage_task(
    service: StorageService,
    mut rx: mpsc::Receiver<StorageEvent>,
    batch_size: usize,
    flush_interval_ms: u64,
) -> Result<()> {
    let mut pg_buffer: Vec<StorageEvent> = Vec::with_capacity(batch_size);
    let flush_interval = Duration::from_millis(flush_interval_ms);
    let mut flush_timer = tokio::time::interval(flush_interval);
    // The first tick completes immediately — consume it so the timer
    // starts from now.
    flush_timer.tick().await;

    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    // Tick → QuestDB (Step 2.7), OrderPlaced → order history (Step 2.6)
                    Some(StorageEvent::Tick { .. } | StorageEvent::OrderPlaced { .. }) => {}
                    Some(StorageEvent::Flush { done }) => {
                        flush_pg_buffer(&service, &mut pg_buffer).await?;
                        let _ = done.send(());
                    }
                    Some(event) => {
                        pg_buffer.push(event);
                        if pg_buffer.len() >= batch_size {
                            flush_pg_buffer(&service, &mut pg_buffer).await?;
                        }
                    }
                    None => {
                        // Channel closed — final flush
                        flush_pg_buffer(&service, &mut pg_buffer).await?;
                        break;
                    }
                }
            }
            _ = flush_timer.tick() => {
                if !pg_buffer.is_empty() {
                    flush_pg_buffer(&service, &mut pg_buffer).await?;
                }
            }
        }
    }

    info!("Storage task exiting");
    Ok(())
}

async fn flush_pg_buffer(service: &StorageService, buffer: &mut Vec<StorageEvent>) -> Result<()> {
    for event in buffer.drain(..) {
        match event {
            StorageEvent::AccountCreated { account } => {
                service
                    .save_account(&account)
                    .await
                    .context("failed to persist account")?;
            }
            StorageEvent::TransactionPosted { transaction } => {
                service
                    .save_transaction(&transaction)
                    .await
                    .context("failed to persist transaction")?;
            }
            StorageEvent::OrderPlaced { .. }
            | StorageEvent::Tick { .. }
            | StorageEvent::Flush { .. } => {}
        }
    }
    Ok(())
}
