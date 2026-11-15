//! Standalone command options; parsed before any network or native access.
use knightbus_core::usbip::WireString;
use std::{
    net::SocketAddr,
    path::PathBuf,
    time::{Duration, Instant},
};

const MAX_DURATION_SECONDS: u64 = 300;

pub(crate) struct Options {
    pub address: SocketAddr,
    pub bus: WireString<32>,
    pub stop: Stop,
    pub security: Security,
}
pub(crate) enum Security {
    Raw,
    Tls {
        control: SocketAddr,
        state: PathBuf,
        peer: knightbus_core::PeerId,
    },
}
pub(crate) enum Stop {
    After(Duration),
    File(PathBuf),
}
impl Stop {
    fn parse(stop: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(if let Some(path) = stop.strip_prefix("until-stop=") {
            let path = PathBuf::from(path);
            if !path.is_absolute() || path.exists() {
                return Err("stop file must be an absolute, initially absent path".into());
            }
            Stop::File(path)
        } else {
            let seconds = stop.parse()?;
            if !(1..=MAX_DURATION_SECONDS).contains(&seconds) {
                return Err("duration must be 1..300 seconds".into());
            }
            Stop::After(Duration::from_secs(seconds))
        })
    }
    pub fn requested(&self, start: Instant) -> bool {
        match self {
            Self::After(duration) => start.elapsed() >= *duration,
            Self::File(path) => path.exists(),
        }
    }
}
impl Options {
    pub fn parse(args: &[String]) -> Result<Self, Box<dyn std::error::Error>> {
        let (address, bus, stop, security) = match args {
            [mode, address, bus, stop] if mode == "--insecure" => {
                let address: SocketAddr = address.parse()?;
                if !address.ip().is_loopback() {
                    return Err("raw compatibility connections require loopback and an authenticated tunnel".into());
                }
                (address, bus, stop, Security::Raw)
            }
            [mode, address, control, state, peer, bus, stop] if mode == "--tls" => (
                address.parse()?,
                bus,
                stop,
                Security::Tls {
                    control: control.parse()?,
                    state: PathBuf::from(state),
                    peer: knightbus_transport::security::parse_fingerprint(peer)?,
                },
            ),
            _ => return Err(USAGE.into()),
        };
        let bus = WireString::new(bus.as_bytes())?;
        let stop = Stop::parse(stop)?;
        Ok(Self {
            address,
            bus,
            stop,
            security,
        })
    }
}
const USAGE: &str = "usage: knightbus-windows-importer --tls DATA_ADDRESS:PORT CONTROL_ADDRESS:PORT STATE_DIR EXPORTER_FINGERPRINT BUS_ID SECONDS|until-stop=ABSOLUTE_PATH; compatibility: --insecure LOOPBACK:PORT BUS_ID STOP";

#[cfg(test)]
mod tests {
    use super::*;
    fn args(stop: &str) -> Vec<String> {
        ["--insecure", "127.0.0.1:43249", "1-1", stop]
            .map(str::to_string)
            .to_vec()
    }
    #[test]
    fn explicit_mode_bounds_and_rejects_extra_arguments() {
        for seconds in ["1", "300"] {
            let mut a = args(seconds);
            let options = Options::parse(&a).unwrap();
            assert_eq!(options.address.port(), 43249);
            assert_eq!(options.bus.as_bytes(), b"1-1");
            assert!(!options.stop.requested(Instant::now()));
            a.push("controller".into());
            assert!(Options::parse(&a).is_err());
        }
        for stop in [
            "0",
            "301",
            "-1",
            "18446744073709551616",
            "",
            "until-stop=relative",
        ] {
            assert!(Options::parse(&args(stop)).is_err(), "{stop}");
        }
        let a = args("1");
        for count in 0..4 {
            assert!(Options::parse(&a[..count]).is_err());
        }
        let mut a = a;
        a[0] = "--authenticated".into();
        assert!(Options::parse(&a).is_err());
    }
    #[test]
    fn tls_identity_is_explicit_and_raw_connections_stay_on_loopback() {
        let a = [
            "--tls",
            "192.0.2.1:3240",
            "192.0.2.1:3241",
            "/state",
            &"01".repeat(32),
            "kb-1",
            "60",
        ]
        .map(str::to_string);
        let options = Options::parse(&a).unwrap();
        let Security::Tls {
            control,
            state,
            peer,
        } = options.security
        else {
            panic!("TLS")
        };
        assert_eq!(control.port(), 3241);
        assert_eq!(state, PathBuf::from("/state"));
        assert_eq!(peer, knightbus_core::PeerId([1; 32]));
        let mut raw = args("10");
        raw[1] = "192.0.2.1:3240".into();
        assert!(Options::parse(&raw).is_err());
        assert!(matches!(
            Options::parse(&args("10")).unwrap().security,
            Security::Raw
        ));
    }
    #[test]
    fn stop_file_is_absolute_initially_absent_and_observed_after_creation() {
        let directory =
            std::env::temp_dir().join(format!("knightbus-options-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let marker = directory.join("stop");
        let a = args(&format!("until-stop={}", marker.display()));
        let options = Options::parse(&a).unwrap();
        assert!(!options.stop.requested(Instant::now()));
        std::fs::write(&marker, []).unwrap();
        assert!(options.stop.requested(Instant::now()));
        assert!(Options::parse(&a).is_err());
        std::fs::remove_file(marker).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
