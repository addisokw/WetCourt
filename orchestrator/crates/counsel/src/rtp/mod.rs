//! RTP media session: one UDP socket, a 20 ms-paced send task and a recv
//! task. Signaling-agnostic — the SIP layer hands us the peer address from
//! SDP and we latch onto the observed source after the first inbound packet
//! (symmetric RTP), which also survives ATA port surprises.

pub mod dtmf;
pub mod g711;
pub mod resample;

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use rtp_rs::{RtpPacketBuilder, RtpReader};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;

use crate::config::RtpConfig;

pub const SAMPLES_PER_FRAME: usize = 160; // 20 ms @ 8 kHz
pub const FRAME_INTERVAL: Duration = Duration::from_millis(20);
pub const PT_PCMU: u8 = 0;

#[derive(Debug)]
pub enum RtpEvent {
    /// One decoded 20 ms frame of 8 kHz linear PCM from the caller.
    Audio(Vec<i16>),
    /// A completed DTMF key press.
    Dtmf(char),
}

/// Bind a socket in the configured port range.
pub async fn bind_socket(cfg: &RtpConfig) -> Result<UdpSocket> {
    for port in cfg.port_min..=cfg.port_max {
        match UdpSocket::bind(("0.0.0.0", port)).await {
            Ok(s) => return Ok(s),
            Err(_) => continue,
        }
    }
    anyhow::bail!(
        "no free RTP port in {}..={}",
        cfg.port_min,
        cfg.port_max
    )
}

/// What the send task plays when the speech queue is empty.
struct CoverState {
    ulaw: Arc<Vec<u8>>,
    cursor: usize,
}

struct MixerInner {
    speech: Mutex<VecDeque<u8>>, // µ-law bytes, drained 160/frame
    speech_len: AtomicUsize,
    cover: Mutex<Option<CoverState>>,
    drained: Notify,
    /// False once the send task exits (call torn down). Unblocks any
    /// `wait_drained` so a hangup mid-speech can't deadlock the agent.
    sending: std::sync::atomic::AtomicBool,
}

/// Control half of the outbound path. Priority per frame:
/// speech queue > cover loop > µ-law silence.
#[derive(Clone)]
pub struct MixerHandle {
    inner: Arc<MixerInner>,
}

impl MixerHandle {
    pub fn queue_speech(&self, ulaw: &[u8]) {
        let mut q = self.inner.speech.lock().unwrap();
        q.extend(ulaw);
        self.inner.speech_len.store(q.len(), Ordering::Relaxed);
    }

    pub fn clear_speech(&self) {
        let mut q = self.inner.speech.lock().unwrap();
        q.clear();
        self.inner.speech_len.store(0, Ordering::Relaxed);
        self.inner.drained.notify_waiters();
    }

    pub fn speech_remaining(&self) -> usize {
        self.inner.speech_len.load(Ordering::Relaxed)
    }

    /// Resolves once the speech queue has fully played out, or immediately if
    /// the send task has stopped (call torn down) — otherwise a hangup mid-
    /// speech would leave the queue undrainable and hang the caller forever.
    pub async fn wait_drained(&self) {
        loop {
            let notified = self.inner.drained.notified();
            if self.speech_remaining() == 0
                || !self.inner.sending.load(Ordering::Relaxed)
            {
                return;
            }
            notified.await;
        }
    }

    /// Set or clear the latency-cover loop (µ-law, played when no speech).
    /// Re-setting the same asset keeps the loop cursor, so back-to-back
    /// phases (IVR grace → hold) don't audibly restart the music.
    pub fn set_cover(&self, ulaw: Option<Arc<Vec<u8>>>) {
        let mut cover = self.inner.cover.lock().unwrap();
        match (ulaw, cover.as_ref()) {
            (Some(new), Some(cur)) if Arc::ptr_eq(&new, &cur.ulaw) => {}
            (new, _) => *cover = new.map(|u| CoverState { ulaw: u, cursor: 0 }),
        }
    }
}

pub struct RtpSession {
    pub mixer: MixerHandle,
    pub events: mpsc::Receiver<RtpEvent>,
    /// Per-call recorder (both legs + annotations), when recording is on.
    pub recorder: Option<Arc<crate::recorder::CallRecorder>>,
}

/// Spawn the send/recv tasks for an answered call. `peer` comes from the
/// remote SDP; `dtmf_pt` is the negotiated telephone-event payload type.
pub fn start(
    socket: UdpSocket,
    peer: SocketAddr,
    dtmf_pt: Option<u8>,
    token: CancellationToken,
    recorder: Option<Arc<crate::recorder::CallRecorder>>,
) -> Result<RtpSession> {
    let socket = Arc::new(socket);
    let mixer = MixerHandle {
        inner: Arc::new(MixerInner {
            speech: Mutex::new(VecDeque::new()),
            speech_len: AtomicUsize::new(0),
            cover: Mutex::new(None),
            drained: Notify::new(),
            sending: std::sync::atomic::AtomicBool::new(true),
        }),
    };
    let (event_tx, event_rx) = mpsc::channel(64);

    // Symmetric-RTP latch: send task reads the current peer, recv task
    // updates it from the first observed source.
    let peer_slot = Arc::new(Mutex::new(peer));

    tokio::spawn(send_task(
        socket.clone(),
        mixer.clone(),
        peer_slot.clone(),
        token.clone(),
        recorder.clone(),
    ));
    tokio::spawn(recv_task(
        socket,
        event_tx,
        peer_slot,
        dtmf_pt,
        token,
        recorder.clone(),
    ));

    Ok(RtpSession { mixer, events: event_rx, recorder })
}

async fn send_task(
    socket: Arc<UdpSocket>,
    mixer: MixerHandle,
    peer_slot: Arc<Mutex<SocketAddr>>,
    token: CancellationToken,
    recorder: Option<Arc<crate::recorder::CallRecorder>>,
) {
    let ssrc: u32 = rand::random();
    let mut seq: u16 = rand::random();
    let mut ts: u32 = rand::random();
    let mut in_talkspurt = false;

    let mut ticker = tokio::time::interval(FRAME_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = ticker.tick() => {}
        }

        let mut frame = [g711::ULAW_SILENCE; SAMPLES_PER_FRAME];
        let mut is_speech = false;
        {
            let mut q = mixer.inner.speech.lock().unwrap();
            if !q.is_empty() {
                is_speech = true;
                for slot in frame.iter_mut() {
                    match q.pop_front() {
                        Some(b) => *slot = b,
                        None => break, // trailing partial frame pads with silence
                    }
                }
                let remaining = q.len();
                mixer.inner.speech_len.store(remaining, Ordering::Relaxed);
                if remaining == 0 {
                    mixer.inner.drained.notify_waiters();
                }
            }
        }
        if !is_speech {
            let mut cover = mixer.inner.cover.lock().unwrap();
            if let Some(c) = cover.as_mut() {
                if !c.ulaw.is_empty() {
                    for slot in frame.iter_mut() {
                        *slot = c.ulaw[c.cursor];
                        c.cursor = (c.cursor + 1) % c.ulaw.len();
                    }
                }
            }
        }

        let marker = is_speech && !in_talkspurt;
        in_talkspurt = is_speech;

        if let Some(rec) = &recorder {
            rec.push_lawyer(&g711::decode(&frame));
        }

        let packet = match RtpPacketBuilder::new()
            .payload_type(PT_PCMU)
            .ssrc(ssrc)
            .sequence(seq.into())
            .timestamp(ts)
            .marked(marker)
            .payload(&frame)
            .build()
        {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("rtp build failed: {e:?}");
                continue;
            }
        };
        seq = seq.wrapping_add(1);
        ts = ts.wrapping_add(SAMPLES_PER_FRAME as u32);

        let peer = *peer_slot.lock().unwrap();
        if let Err(e) = socket.send_to(&packet, peer).await {
            tracing::debug!("rtp send to {peer}: {e}");
        }
    }

    // Torn down: unblock anyone awaiting drain (they'd wait forever now).
    mixer.inner.sending.store(false, Ordering::Relaxed);
    mixer.inner.drained.notify_waiters();
}

async fn recv_task(
    socket: Arc<UdpSocket>,
    events: mpsc::Sender<RtpEvent>,
    peer_slot: Arc<Mutex<SocketAddr>>,
    dtmf_pt: Option<u8>,
    token: CancellationToken,
    recorder: Option<Arc<crate::recorder::CallRecorder>>,
) {
    let mut buf = vec![0u8; 1500];
    let mut dtmf = dtmf::DtmfParser::new();
    let mut latched = false;

    loop {
        let (len, from) = tokio::select! {
            _ = token.cancelled() => return,
            r = socket.recv_from(&mut buf) => match r {
                Ok(v) => v,
                Err(e) => {
                    tracing::debug!("rtp recv: {e}");
                    continue;
                }
            },
        };

        let Ok(reader) = RtpReader::new(&buf[..len]) else {
            continue;
        };

        if !latched {
            let expected = *peer_slot.lock().unwrap();
            if from != expected {
                tracing::info!("rtp: latching to observed peer {from} (sdp said {expected})");
                *peer_slot.lock().unwrap() = from;
            }
            latched = true;
        }

        let pt = reader.payload_type();
        if Some(pt) == dtmf_pt {
            if let Some(digit) = dtmf.push(reader.timestamp(), reader.payload()) {
                let _ = events.try_send(RtpEvent::Dtmf(digit));
            }
        } else if pt == PT_PCMU {
            let samples = g711::decode(reader.payload());
            if let Some(rec) = &recorder {
                rec.push_caller(&samples);
            }
            // try_send: if the consumer stalls we drop frames rather than
            // build latency — this is a phone call, not a recording.
            let _ = events.try_send(RtpEvent::Audio(samples));
        }
    }
}
