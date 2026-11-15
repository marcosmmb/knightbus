//! Exercise collection against the real IPC request limiter.
use knightbus_sdk::{
    cli,
    core::*,
    ipc::{
        local::{Cancellation, Listener},
        private::Directory,
    },
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[test]
fn diagnostics_collects_forty_sessions_and_reports_missing_results() {
    for failed in [false, true] {
        let root = std::env::temp_dir().join(format!(
            "kb-diagnostics-{}-{}-{failed}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        Directory::open(&root).unwrap();
        #[cfg(unix)]
        let endpoint = root.join("socket");
        #[cfg(windows)]
        let endpoint = PathBuf::from(format!(
            r"\\.\pipe\{}",
            root.file_name().unwrap().to_str().unwrap()
        ));
        let output: PathBuf = root.join("report");
        let listener = Listener::bind(&endpoint).unwrap();
        let server = std::thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(5);
            let connection = loop {
                assert!(Instant::now() < end);
                if let Some(connection) = listener.accept().unwrap() {
                    break connection;
                }
            };
            let mut statuses = 0;
            let _ = connection.serve(&Cancellation::default(), |_, command, _| {
                Ok(match command {
                    Command::Info => Reply::Info {
                        version: CONTROL_VERSION,
                        epoch: 1,
                        now: 0,
                    },
                    Command::Sessions => Reply::Sessions(
                        (1..=40)
                            .map(|serial| SessionId { epoch: 1, serial })
                            .collect(),
                    ),
                    Command::Status(id) => {
                        statuses += 1;
                        if failed && id.serial == 20 {
                            return Err(Error::NotFound);
                        }
                        Reply::Session(Session {
                            id,
                            peer: PeerId([0; 32]),
                            devices: vec![],
                            state: SessionState::Closed,
                            error: None,
                        })
                    }
                    _ => panic!("unexpected command"),
                })
            });
            statuses
        });
        let result = cli::run(&[
            "manage".into(),
            endpoint.to_str().unwrap().into(),
            "diagnostics".into(),
            output.to_str().unwrap().into(),
        ]);
        assert_eq!(result.is_err(), failed);
        assert_eq!(server.join().unwrap(), 40);
        let report = std::fs::read_to_string(output).unwrap();
        assert!(report.contains(if failed {
            "complete=false"
        } else {
            "complete=true"
        }));
        assert_eq!(
            report.matches("Ok(Session(").count(),
            if failed { 39 } else { 40 }
        );
        assert_eq!(report.contains("Err(NotFound)"), failed);
        std::fs::remove_dir_all(root).unwrap();
    }
}
