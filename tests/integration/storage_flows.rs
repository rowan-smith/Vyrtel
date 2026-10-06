//! start → ingest → query → restart → query; rotation; retention.

use std::time::Duration;

use serde_json::json;
use telemetry::Signal;

use crate::common::*;

#[tokio::test(flavor = "multi_thread")]
async fn ingest_query_restart_query() {
    let s = TestServer::start().await;
    s.ingest_ndjson(fixture("logs.ndjson")).await.error_for_status().unwrap();
    let before = s.query(r#"level = "Error" and service = "payments""#).await;
    assert_eq!(before["events"].as_array().unwrap().len(), 1);
    let all_before = s.messages("").await;
    assert_eq!(all_before.len(), 8);

    // Restart without sealing: the events come back from the WAL.
    let s = s.restart().await;
    assert_eq!(s.messages("").await, all_before);
    let after = s.query(r#"level = "Error" and service = "payments""#).await;
    assert_eq!(after["events"], before["events"], "identical events, including ids");

    // New ids continue after the recovered ones.
    s.ingest(json!({"message": "after restart"})).await;
    let v = s.query(r#"message = "after restart""#).await;
    let id = v["events"][0]["id"].as_str().unwrap();
    assert_eq!(id, "0000000000000009");
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ingest_rotate_query_and_restart() {
    let s = TestServer::start().await;
    s.ingest_ndjson(fixture("logs.ndjson")).await.error_for_status().unwrap();
    s.rotate_all();
    s.ingest(json!({"message": "unsealed tail", "level": "Error", "service": "payments"})).await;

    let v = s.query(r#"level = Error and service = payments"#).await;
    assert_eq!(v["events"].as_array().unwrap().len(), 2);
    let d = &v["diagnostics"];
    assert_eq!(d["segmentsConsidered"], 1);
    assert!(d["unsealedEventsScanned"].as_u64().unwrap() >= 1);
    assert!(d["indexes"].as_array().unwrap().iter().any(|i| i["field"] == "level" && i["kind"] == "bitmap"));

    // An impossible service is answered from the segment summary alone.
    let v = s.query(r#"service = "nope""#).await;
    assert_eq!(v["diagnostics"]["blocksRead"], 0);
    assert_eq!(v["diagnostics"]["segmentsSkippedByIndex"], 1);

    let storage = s.get_ok("/api/v1/system/storage").await;
    assert_eq!(storage["segmentCount"], 1);
    assert!(storage["compressionRatio"].as_f64().unwrap() > 0.0);
    assert_eq!(storage["eventCount"], 9);

    let s = s.restart().await;
    assert_eq!(s.messages("level = Error and service = payments").await.len(), 2);
    assert_eq!(s.get_ok("/api/v1/system/storage").await["segmentCount"], 1);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn retention_deletes_old_segments_across_restart() {
    let s = TestServer::start_with(|c| {
        c.retention.logs = Some(Duration::from_secs(3600));
    })
    .await;
    let three_hours_ago = telemetry::Timestamp::now().saturating_sub_nanos(3 * 3600 * telemetry::NANOS_PER_SEC);
    s.ingest(json!({"message": "ancient", "timestamp": three_hours_ago.to_rfc3339()})).await;
    s.rotate_all();
    s.ingest(json!({"message": "fresh"})).await;
    s.rotate_all();
    assert_eq!(s.messages("").await, ["ancient", "fresh"]);

    let st = s.state().clone();
    tokio::task::spawn_blocking(move || st.storage.maintain_blocking(Signal::Logs).unwrap()).await.unwrap();
    assert_eq!(s.messages("").await, ["fresh"]);

    let s = s.restart().await;
    assert_eq!(s.messages("").await, ["fresh"]);
    assert_eq!(s.get_ok("/api/v1/system/storage").await["segmentCount"], 1);
    s.stop().await;
}
