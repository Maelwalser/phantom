//! Concurrency smoke test for [`OverlayLayer`].
//!
//! Verifies the fine-grained `RwLock` strategy does not deadlock under a
//! mix of readers and writers hitting distinct paths.

mod common;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::thread;

use phantom_overlay::OverlayLayer;

use common::setup;

#[test]
fn concurrent_read_write_no_deadlock() {
    let (lower, upper) = setup();
    fs::write(lower.path().join("shared.txt"), b"trunk data").unwrap();

    let layer = Arc::new(
        OverlayLayer::new(lower.path().to_path_buf(), upper.path().to_path_buf()).unwrap(),
    );

    let mut handles = Vec::new();

    // Spawn reader threads.
    for i in 0..4 {
        let layer = Arc::clone(&layer);
        handles.push(thread::spawn(move || {
            for _ in 0..50 {
                let _ = layer.exists(Path::new("shared.txt"));
                let _ = layer.read_file(Path::new("shared.txt"));
                let _ = layer.getattr(Path::new("shared.txt"));
                let _ = layer.read_dir(Path::new(""));
                let _ = layer.deleted_files();
            }
            i
        }));
    }

    // Spawn writer threads. Each thread writes to its own files to
    // avoid racing on the same upper-layer path (which would cause
    // benign I/O errors in the test).
    for i in 0..4 {
        let layer = Arc::clone(&layer);
        handles.push(thread::spawn(move || {
            for j in 0..50 {
                let name = format!("file_{i}_{j}.txt");
                let _ = layer.write_file(Path::new(&name), b"data");
                let _ = layer.delete_file(Path::new(&name));
            }
            i
        }));
    }

    // All threads must complete without deadlock or panic.
    for h in handles {
        h.join().expect("thread panicked");
    }
}
