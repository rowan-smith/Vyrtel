//! Locate the built web UI (`web/dist`) for embedding.
//!
//! If the UI has not been built (e.g. a backend-only `cargo test`), a tiny
//! placeholder page is embedded instead so the server still compiles and
//! tells the user how to build the UI.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dist = manifest.join("../../web/dist");
    println!("cargo:rerun-if-changed=../../web/dist");
    println!("cargo:rerun-if-env-changed=VYRTEL_WEB_DIST");

    let dir = if let Ok(custom) = std::env::var("VYRTEL_WEB_DIST") {
        PathBuf::from(custom)
    } else if dist.join("index.html").exists() {
        dist.canonicalize().unwrap()
    } else {
        let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("web-placeholder");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(
            out.join("index.html"),
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>Vyrtel</title></head>\
             <body style=\"font-family:system-ui;padding:2rem\"><h1>Vyrtel</h1>\
             <p>The web UI was not built into this binary. Run <code>npm ci &amp;&amp; npm run build</code> \
             in <code>web/</code> and rebuild the server.</p><p>The HTTP API is fully available.</p></body></html>",
        )
        .unwrap();
        out
    };
    println!("cargo:rustc-env=VYRTEL_WEB_DIST={}", dir.display());
}
