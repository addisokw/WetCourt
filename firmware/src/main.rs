// Wet Court of Appeals — MCU firmware (M5Stack NanoC6, ESP32-C6).
//
// Transport: plain TCP, \n-delimited ASCII, matching the §5.2 protocol from
// the architecture doc. Host listens on hardware.bind_addr; we dial it.
//
// First-cut hardware: commands are acknowledged but actions are logged only
// — the built-in SK6812 needs an RMT driver whose ecosystem versioning is
// currently a mess. Wiring real GPIO actuation is a follow-up. The BOOT
// button (GPIO9) is the lectern trial-start button and is functional now.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::gpio::{Input, Level, PinDriver, Pull};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use log::{info, warn};

mod wifi_config;
use wifi_config::{ORCH_HOST, ORCH_PORT, WIFI_PASS, WIFI_SSID};

const BUTTON_DEBOUNCE: Duration = Duration::from_millis(50);
const RECONNECT_BACKOFF: Duration = Duration::from_secs(2);
const SOCKET_READ_TIMEOUT: Duration = Duration::from_millis(20);

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    // BOOT button on GPIO9, externally pulled up; press pulls low.
    let mut btn = PinDriver::input(peripherals.pins.gpio9, Pull::Up)?;

    info!("connecting to wifi: {WIFI_SSID}");
    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?,
        sys_loop,
    )?;
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: WIFI_SSID
            .try_into()
            .map_err(|_| anyhow!("WIFI_SSID too long"))?,
        password: WIFI_PASS
            .try_into()
            .map_err(|_| anyhow!("WIFI_PASS too long"))?,
        auth_method: AuthMethod::WPA2Personal,
        ..Default::default()
    }))?;
    wifi.start()?;
    wifi.connect()?;
    wifi.wait_netif_up()?;
    info!("wifi up: {:?}", wifi.wifi().sta_netif().get_ip_info()?.ip);

    loop {
        match run_session(&mut btn) {
            Ok(()) => info!("session closed cleanly; reconnecting"),
            Err(e) => warn!("session error: {e:#}; reconnecting"),
        }
        std::thread::sleep(RECONNECT_BACKOFF);
    }
}

fn run_session(btn: &mut PinDriver<'_, Input>) -> Result<()> {
    info!("connecting to orchestrator: {ORCH_HOST}:{ORCH_PORT}");
    let stream = TcpStream::connect_timeout(
        &format!("{ORCH_HOST}:{ORCH_PORT}")
            .parse()
            .context("parsing host:port")?,
        Duration::from_secs(5),
    )?;
    stream.set_read_timeout(Some(SOCKET_READ_TIMEOUT))?;
    stream.set_nodelay(true)?;
    info!("connected");

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    let mut fire_until: Option<Instant> = None;
    let mut last_btn_level = btn.get_level();
    let mut last_btn_edge = Instant::now() - BUTTON_DEBOUNCE;

    let mut line_buf = Vec::with_capacity(128);

    loop {
        if let Some(until) = fire_until {
            if Instant::now() >= until {
                info!("fire ended");
                fire_until = None;
            }
        }

        let level = btn.get_level();
        if level != last_btn_level && last_btn_edge.elapsed() >= BUTTON_DEBOUNCE {
            last_btn_edge = Instant::now();
            last_btn_level = level;
            if level == Level::Low {
                info!("button pressed -> BUTTON");
                writer.write_all(b"BUTTON\n")?;
            }
        }

        line_buf.clear();
        match reader.read_until(b'\n', &mut line_buf) {
            Ok(0) => return Err(anyhow!("orchestrator closed connection")),
            Ok(_) => {
                let line = std::str::from_utf8(&line_buf)
                    .unwrap_or("")
                    .trim_end_matches(['\r', '\n']);
                if !line.is_empty() {
                    handle_command(line, &mut writer, &mut fire_until)?;
                }
            }
            Err(e)
                if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
            {
                // No data this tick.
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn handle_command(
    line: &str,
    writer: &mut TcpStream,
    fire_until: &mut Option<Instant>,
) -> Result<()> {
    let mut parts = line.split_whitespace();
    let Some(cmd) = parts.next() else { return Ok(()); };

    match cmd {
        "FIRE" => {
            let ms: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(150);
            info!("FIRE {ms}ms");
            *fire_until = Some(Instant::now() + Duration::from_millis(ms));
            writer.write_all(b"OK FIRE\n")?;
        }
        "GAVEL" => {
            info!("GAVEL");
            writer.write_all(b"OK GAVEL\n")?;
        }
        "LIGHTS" => {
            let state = parts.next().unwrap_or("?");
            info!("LIGHTS {state}");
            writer.write_all(b"OK LIGHTS\n")?;
        }
        "PANEL" => {
            let pattern = parts.next().unwrap_or("?");
            info!("PANEL {pattern}");
            writer.write_all(b"OK PANEL\n")?;
        }
        "PING" => {
            writer.write_all(b"PONG\n")?;
        }
        other => {
            warn!("unknown command: {other}");
            let msg = format!("ERR {other} unknown\n");
            writer.write_all(msg.as_bytes())?;
        }
    }
    Ok(())
}
