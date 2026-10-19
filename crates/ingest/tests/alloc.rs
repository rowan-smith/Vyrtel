//! Real heap, not the decoders' own accounting: a counting allocator
//! measures what native parsing allocates. Counters are per thread, so
//! tests running in parallel do not see each other's allocations.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use ingest::{IngestError, Limits, native};
use telemetry::Timestamp;

struct Counting;

thread_local! {
    static CURRENT: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn add(n: isize) {
    let _ = CURRENT.try_with(|c| {
        c.set(c.get() + n);
        let _ = PEAK.try_with(|p| p.set(p.get().max(c.get())));
    });
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        add(layout.size() as isize);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        add(-(layout.size() as isize));
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        add(new_size as isize - layout.size() as isize);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Heap peak of parsing `body`, above what was held before.
fn peak(body: &[u8], format: native::Format, limits: Limits) -> (Result<usize, IngestError>, usize) {
    let base = CURRENT.with(Cell::get);
    PEAK.with(|p| p.set(base));
    let r = native::parse(body, format, Timestamp(1), limits).map(|v| v.len());
    (r, (PEAK.with(Cell::get) - base) as usize)
}

/// `key` as a JSON string with its first character written as a `\u`
/// escape.
fn escaped(key: &str) -> String {
    let mut chars = key.chars();
    let first = chars.next().unwrap();
    format!("\"{}u{:04x}{}\"", '\\', first as u32, chars.as_str())
}

// Finding: an envelope whose key is escaped was parsed whole before its
// events were counted. It is now refused as soon as the count passes the
// limit; the tree of its 40,000 elements is never built.
#[test]
fn an_escaped_envelope_over_the_event_limit_is_not_built() {
    const N: usize = 40_000;
    let body = format!("{{{}:[{}]}}", escaped("events"), vec!["{}"; N].join(","));
    assert!(body.starts_with(&format!("{{\"{}u0065vents\":[", '\\')), "the key is escaped: {}", &body[..20]);
    let limits = Limits { max_events: 1000, ..Limits::UNBOUNDED };
    let (r, refused) = peak(body.as_bytes(), native::Format::Json, limits);
    assert_eq!(r, Err(IngestError::TooManyEvents { limit: 1000 }));
    let tree = N * size_of::<serde_json::Value>();
    assert!(refused < tree / 4, "refused at {refused} bytes; the tree alone is {tree}");
    // Within the limit it is stored, and costs about what a plain one does.
    let (r, escaped_peak) = peak(body.as_bytes(), native::Format::Json, Limits::UNBOUNDED);
    assert_eq!(r, Ok(N));
    let plain = format!(r#"{{"events":[{}]}}"#, vec!["{}"; N].join(","));
    let (_, plain_peak) = peak(plain.as_bytes(), native::Format::Json, Limits::UNBOUNDED);
    assert!(escaped_peak <= plain_peak * 2, "escaped {escaped_peak}, plain {plain_peak}");
}

// Finding: arrays reserved a growth step before knowing another element
// existed. An empty array now allocates nothing, and an array of exactly
// 4, 8 or 16 elements no more than one of that length needs.
#[test]
fn arrays_allocate_only_for_elements_that_exist() {
    let event = |n: usize| format!(r#"{{"message":"m","p":[{}]}}"#, vec!["0"; n].join(","));
    let at = |n: usize| peak(event(n).as_bytes(), native::Format::NdJson, Limits::UNBOUNDED).1;
    let none = peak(br#"{"message":"m","p":0}"#, native::Format::NdJson, Limits::UNBOUNDED).1;
    assert!(at(0) <= none, "[] allocates nothing: {} against {none} for a number", at(0));
    for n in [4, 8, 16] {
        assert!(at(n) < at(n + 1), "{n} elements need less than {} (one growth step)", n + 1);
    }
}
