//! Management-call microbenchmark. This does not measure USB RTT or input latency.
use knightbus_sdk::{Client, Embedded, core::Command, ipc::local::Listener};
use std::time::{Duration, Instant};
fn sample(mode: &str, mut call: impl FnMut()) {
    let mut values = Vec::new();
    for sample in 0..64 {
        // Respect the same bounded management rate as production clients.
        std::thread::sleep(Duration::from_millis(50));
        let start = Instant::now();
        call();
        let ns = start.elapsed().as_nanos();
        values.push(ns);
        println!("control_sample,mode={mode},sample={sample},ns={ns}");
    }
    values.sort_unstable();
    println!(
        "control_summary,mode={mode},samples=64,p50_ns={},p95_ns={},p99_ns={}",
        values[31], values[60], values[63]
    );
}
#[test]
#[ignore = "prints raw management timing; no USB performance claim"]
fn management_latency() {
    let host = crate::tests::runtime();
    let mut embedded = Embedded::connect(&host).unwrap();
    sample("embedded", || {
        embedded.request(Command::Info, 5000).unwrap();
    });
    let (root, endpoint) = crate::tests::location();
    let listener = Listener::bind(&endpoint).unwrap();
    let service = host.clone();
    let worker = std::thread::spawn(move || service.serve(&listener));
    let mut ipc = Client::connect(&endpoint).unwrap();
    sample("local_ipc", || {
        ipc.request(Command::Info, 5000).unwrap();
    });
    ipc.request(Command::Shutdown, 5000).unwrap();
    worker.join().unwrap().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
