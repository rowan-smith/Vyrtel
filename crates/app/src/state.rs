use std::sync::Arc;

use ingest::IngestHandle;
use metadata::MetadataStore;
use storage::EventStore;

use crate::config::Config;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<EventStore>,
    pub ingest: IngestHandle,
    pub metadata: Arc<MetadataStore>,
    pub config: Config,
}
