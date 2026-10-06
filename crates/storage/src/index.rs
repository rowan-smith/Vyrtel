use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use event::Event;
use tantivy::directory::MmapDirectory;
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyError};
use tracing::{info, warn};

use crate::schema::EventSchema;
use crate::search::{
    count_all, search_events, EventSearchParams, EventSearchResult, SearchError,
};
use crate::writer::EventWriter;

pub struct EventStore {
    pub schema: EventSchema,
    index: Index,
    reader: IndexReader,
    writer: Mutex<IndexWriter>,
    event_writer: EventWriter,
}

fn open_index(path: &Path, schema: &EventSchema) -> tantivy::Result<Index> {
    let directory = MmapDirectory::open(path)?;
    Index::open_or_create(directory, schema.schema.clone())
}

impl EventStore {
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        std::fs::create_dir_all(path).context("create index directory")?;
        let schema = EventSchema::build();
        let index = match open_index(path, &schema) {
            Err(TantivyError::SchemaError(reason)) => {
                warn!(
                    "index at {} has an incompatible schema ({reason}); deleting it and starting empty",
                    path.display()
                );
                std::fs::remove_dir_all(path).context("remove incompatible index")?;
                std::fs::create_dir_all(path).context("create index directory")?;
                open_index(path, &schema)
            }
            other => other,
        }
        .context("open or create tantivy index")?;

        let writer = index
            .writer(50_000_000)
            .context("create index writer")?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .context("create index reader")?;

        let event_writer = EventWriter::new(EventSchema {
            schema: schema.schema.clone(),
            fields: schema.fields.clone(),
        });

        info!("Tantivy index ready at {}", path.display());

        Ok(Arc::new(Self {
            schema,
            index,
            reader,
            writer: Mutex::new(writer),
            event_writer,
        }))
    }

    pub fn writer_helper(&self) -> &EventWriter {
        &self.event_writer
    }

    pub fn index(&self) -> &Index {
        &self.index
    }

    pub fn searcher(&self) -> tantivy::Searcher {
        self.reader.searcher()
    }

    pub fn reload(&self) -> Result<()> {
        self.reader.reload()?;
        Ok(())
    }

    pub fn write_batch(&self, events: &[Event]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let mut writer = self.writer.lock().expect("writer lock");
        self.event_writer.add_events(&mut writer, events)?;
        self.event_writer.commit(&mut writer)?;
        drop(writer);
        self.reader.reload()?;
        Ok(())
    }

    pub fn search(&self, params: EventSearchParams) -> Result<EventSearchResult, SearchError> {
        let searcher = self.searcher();
        search_events(&searcher, &self.schema, &self.event_writer, params)
    }

    pub fn event_count(&self) -> Result<usize, SearchError> {
        count_all(&self.searcher())
    }

    pub fn delete_before(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        let mut writer = self.writer.lock().expect("writer lock");
        self.event_writer.delete_before(&mut writer, cutoff)?;
        let opstamp = self.event_writer.commit(&mut writer)?;
        drop(writer);
        self.reader.reload()?;
        Ok(opstamp)
    }

    pub fn schema_fields(&self) -> &crate::schema::SchemaFields {
        &self.schema.fields
    }
}
