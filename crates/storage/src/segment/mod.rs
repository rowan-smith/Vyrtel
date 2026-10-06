//! Immutable, versioned, block-compressed segment files.

pub mod format;
mod reader;
mod writer;

pub use format::*;
pub use reader::{BlockData, MetaRow, Segment};
pub use writer::{SegmentWriteOptions, WrittenSegment, paths, write_segment};

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use telemetry::testing::any_event;
    use telemetry::*;

    fn dirs() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let seg = d.path().join("segments");
        let tmp = d.path().join("tmp");
        std::fs::create_dir_all(&seg).unwrap();
        std::fs::create_dir_all(&tmp).unwrap();
        (d, seg, tmp)
    }

    fn refs(e: &[TelemetryEvent]) -> Vec<&TelemetryEvent> {
        e.iter().collect()
    }

    fn sorted(mut events: Vec<TelemetryEvent>) -> Vec<TelemetryEvent> {
        // Ids must be unique within a stream.
        for (i, e) in events.iter_mut().enumerate() {
            e.id = EventId(i as u64 + 1);
        }
        events.sort_by_key(|e| (e.timestamp, e.id));
        events
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// segment writing → reading → identical event set.
        #[test]
        fn write_read_round_trip(events in proptest::collection::vec(any_event(), 1..60), block in 1usize..16) {
            let (_d, seg, tmp) = dirs();
            let events = sorted(events);
            let opts = SegmentWriteOptions { block_events: block, ..Default::default() };
            let w = write_segment(&seg, &tmp, 7, Signal::Logs, &refs(&events), vec![], &opts).unwrap();
            let s = Segment::open(&w.path).unwrap();
            prop_assert_eq!(&s.summary, &w.summary);
            prop_assert_eq!(s.summary.event_count as usize, events.len());
            prop_assert_eq!(s.read_all().unwrap(), events);
            let idx = s.read_index().unwrap();
            prop_assert_eq!(idx.blocks.len(), s.summary.blocks.len());
        }
    }

    fn log(ts: i64, svc: &str, level: Level, attrs: &[(&str, Value)]) -> TelemetryEvent {
        TelemetryEvent {
            id: EventId(0),
            timestamp: Timestamp(ts),
            observed_timestamp: Timestamp(ts),
            service: Some(svc.into()),
            environment: None,
            trace_id: TraceId::new(&format!("{ts:032x}")),
            span_id: None,
            resource: Fields::new(),
            attributes: attrs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
            payload: TelemetryPayload::Log(LogEvent {
                level,
                message: format!("event {ts}"),
                message_template: None,
                exception: None,
            }),
        }
    }

    #[test]
    fn summary_and_indexes_describe_content() {
        let (_d, seg, tmp) = dirs();
        let events = sorted(
            (0..100)
                .map(|i| {
                    log(
                        1_000 + i,
                        if i < 50 { "payments" } else { "orders" },
                        if i % 10 == 0 { Level::Error } else { Level::Information },
                        &[("durationMs", Value::Int(i)), ("customerId", Value::from(format!("c{i}")))],
                    )
                })
                .collect(),
        );
        let opts = SegmentWriteOptions { block_events: 25, ..Default::default() };
        let w = write_segment(&seg, &tmp, 1, Signal::Logs, &refs(&events), vec![], &opts).unwrap();
        let s = Segment::open(&w.path).unwrap();
        let sum = &s.summary;
        assert_eq!(sum.blocks.len(), 4);
        assert_eq!(sum.min_ts, Timestamp(1_000));
        assert_eq!(sum.max_ts, Timestamp(1_099));
        assert_eq!(sum.services, vec!["payments".to_string(), "orders".to_string()]);
        // payments only in the first two blocks.
        assert_eq!(sum.service_blocks[0].iter().collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(sum.service_blocks[1].iter().collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(sum.level_blocks[Level::Error.code() as usize].count(), 4);
        assert!(sum.index_bytes > 0 && sum.data_bytes > 0);

        let idx = s.read_index().unwrap();
        let z = idx.zone_field("durationMs").unwrap();
        let zone = idx.blocks[2].zones.iter().find(|x| x.0 == z).unwrap();
        assert_eq!((zone.1, zone.2), (50.0, 74.0));
        let key = crate::index::keys::string_key("customerId", "c60");
        assert!(idx.blocks[2].bloom.may_contain(key));
        let stat = idx.fields.iter().find(|f| f.path == "customerId").unwrap();
        assert_eq!(stat.present, 100);
        let d = stat.distinct.estimate();
        assert!((60..=140).contains(&d), "{d}");

        // Reading only some columns works and skips the others.
        let partial = s.read_block(1, ColumnSet::new().with(Column::Meta)).unwrap();
        assert!(partial.body.is_none() && partial.meta.is_some());
        assert!(partial.bytes_read < sum.blocks[1].compressed_bytes());
    }

    #[test]
    fn corrupt_chunk_is_detected() {
        let (_d, seg, tmp) = dirs();
        let events = sorted((0..10).map(|i| log(i, "a", Level::Information, &[])).collect());
        let w = write_segment(&seg, &tmp, 1, Signal::Logs, &refs(&events), vec![], &Default::default()).unwrap();
        let s = Segment::open(&w.path).unwrap();
        let body = s.summary.blocks[0].columns[Column::Body as usize];
        drop(s);
        let mut bytes = std::fs::read(&w.path).unwrap();
        bytes[body.offset as usize + 1] ^= 0xff;
        std::fs::write(&w.path, &bytes).unwrap();
        let s = Segment::open(&w.path).unwrap();
        assert!(s.read_block(0, ColumnSet::new().with(Column::Meta)).is_ok());
        assert!(matches!(s.read_block(0, ColumnSet::ALL), Err(crate::StorageError::Corrupt { .. })));
    }

    #[test]
    fn truncated_file_is_rejected() {
        let (_d, seg, tmp) = dirs();
        let events = sorted((0..10).map(|i| log(i, "a", Level::Information, &[])).collect());
        let w = write_segment(&seg, &tmp, 1, Signal::Logs, &refs(&events), vec![], &Default::default()).unwrap();
        let len = std::fs::metadata(&w.path).unwrap().len();
        let f = std::fs::OpenOptions::new().write(true).open(&w.path).unwrap();
        f.set_len(len - 10).unwrap();
        drop(f);
        assert!(Segment::open(&w.path).is_err());
    }

    #[test]
    fn temp_file_removed_and_nothing_committed_on_failure() {
        let (_d, seg, tmp) = dirs();
        // Writing into a directory that does not exist fails before commit.
        let missing = seg.join("missing");
        let events = sorted((0..3).map(|i| log(i, "a", Level::Information, &[])).collect());
        assert!(write_segment(&missing, &tmp, 1, Signal::Logs, &refs(&events), vec![], &Default::default()).is_err());
        assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
    }
}
