// Wet Court of Appeals — MCU firmware (M5Stack NanoC6, ESP32-C6).
//
// Transport: plain TCP, \n-delimited ASCII, matching the §5.2 protocol from
// the architecture doc. Host listens on hardware.bind_addr; we dial it.
//
// First-cut hardware: built-in SK6812 RGB LED stands in for the squirt valve
// (red during FIRE), built-in BOOT button (GPIO9) is the lectern trial-start
// button.

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::gpio::{Level, PinDriver, Pull};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use log::{info, warn};
use smart_leds::{SmartLedsWrite, RGB8};
use ws2812_esp32_rmt_driver::Ws2812Esp32Rmt;

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

    // NanoC6 RGB LED needs GPIO19 driven high to power the SK6812.
    let mut led_pwr = PinDriver::output(peripherals.pins.gpio19)?;
    led_pwr.set_high()?;
    let mut led = Ws2812Esp32Rmt::new(peripherals.rmt.channel0, peripherals.pins.gpio20)?;
    paint(&mut led, RGB8::new(0, 0, 0))?;

    // BOOT button on GPIO9, externally pulled up; press pulls low.
    let mut btn = PinDriver::input(peripherals.pins.gpio9)?;
    btn.set_pull(Pull::Up)?;

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
    info!(
        "wifi up: {:?}",
        wifi.wifi().sta_netif().get_ip_info()?.ip
    );

    loop {
        match run_session(&mut led, &mut btn) {
            Ok(()) => info!("session closed cleanly; reconnecting"),
            Err(e) => warn!("session error: {e:#}; reconnecting"),
        }
        let _ = paint(&mut led, RGB8::new(0, 0, 0));
        std::thread::sleep(RECONNECT_BACKOFF);
    }
}

fn run_session<BtnPin>(
    led: &mut Ws2812Esp32Rmt,
    btn: &mut PinDriver<'_, BtnPin, esp_idf_svc::hal::gpio::Input>,
) -> Result<()>
where
    BtnPin: esp_idf_svc::hal::gpio::InputPin,
{
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

    // Briefly flash green to signal "linked".
    paint(led, RGB8::new(0, 60, 0))?;
    std::thread::sleep(Duration::from_millis(150));
    paint(led, RGB8::new(0, 0, 0))?;

    let mut fire_until: Option<Instant> = None;
    let mut last_btn_level = btn.get_level();
    let mut last_btn_edge = Instant::now() - BUTTON_DEBOUNCE;

    let mut line_buf = Vec::with_capacity(128);

    loop {
        // Pending FIRE expiration.
        if let Some(until) = fire_until {
            if Instant::now() >= until {
                paint(led, RGB8::new(0, 0, 0))?;
                fire_until = None;
            }
        }

        // Button polling with debounce. Press = level goes Low.
        let level = btn.get_level();
        if level != last_btn_level && last_btn_edge.elapsed() >= BUTTON_DEBOUNCE {
            last_btn_edge = Instant::now();
            last_btn_level = level;
            if level == Level::Low {
                info!("button pressed -> BUTTON");
                writer.write_all(b"BUTTON\n")?;
            }
        }

        // Read one line (blocking up to SOCKET_READ_TIMEOUT).
        line_buf.clear();
        match reader.read_until(b'\n', &mut line_buf) {
            Ok(0) => return Err(anyhow!("orchestrator closed connection")),
            Ok(_) => {
                let line = std::str::from_utf8(&line_buf)
                    .unwrap_or("")
                    .trim_end_matches(['\r', '\n']);
                if !line.is_empty() {
                    handle_command(line, led, &mut writer, &mut fire_until)?;
                }
            }
            Err(e)
                if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
            {
                // No data this tick — fall through to next loop iteration.
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn handle_command(
    line: &str,
    led: &mut Ws2812Esp32Rmt,
    writer: &mut TcpStream,
    fire_until: &mut Option<Instant>,
) -> Result<()> {
    let mut parts = line.split_whitespace();
    let Some(cmd) = parts.next() else { return Ok(()); };

    match cmd {
        "FIRE" => {
            let ms: u64 = parts
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(150);
            paint(led, RGB8::new(120, 0, 0))?;
            *fire_until = Some(Instant::now() + Duration::from_millis(ms));
            writer.write_all(b"OK FIRE\n")?;
        }
        "GAVEL" => {
            // No physical gavel yet — flash white briefly to signal it.
            paint(led, RGB8::new(80, 80, 80))?;
            std::thread::sleep(Duration::from_millis(60));
            paint(led, RGB8::new(0, 0, 0))?;
            writer.write_all(b"OK GAVEL\n")?;
        }
        "LIGHTS" => {
            let state = parts.next().unwrap_or("");
            let color = match state {
                "splash_idle" => RGB8::new(0, 0, 20),
                "splash_arming" => RGB8::new(40, 20, 0),
                "guilty" => RGB8::new(80, 0, 0),
                "not_guilty" => RGB8::new(0, 60, 0),
                _ => RGB8::new(0, 0, 0),
            };
            // Don't stomp an in-progress FIRE.
            if fire_until.is_none() {
                paint(led, color)?;
            }
            writer.write_all(b"OK LIGHTS\n")?;
        }
        "PANEL" => {
            // No status panel hardware yet; ack so the state machine advances.
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

fn paint(led: &mut Ws2812Esp32Rmt, color: RGB8) -> Result<()> {
    led.write(std::iter::once(color))
        .map_err(|e| anyhow!("led write failed: {e:?}"))?;
    Ok(())
}
