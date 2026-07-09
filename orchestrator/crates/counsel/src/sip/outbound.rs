//! Ring-out: "your lawyer is calling YOU." Originates an INVITE to the
//! registered ATA (real ring voltage on the analog phone), and when the
//! defendant answers, the agent opens with a reason-seeded line.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rsipstack::dialog::invitation::InviteOption;
use rsipstack::sip as rsip;
use serde::Serialize;

use crate::call::{agent, ActiveCall, CallKind};
use crate::rtp;
use crate::sip::{sdp, SipCtx};

/// How long the phone rings before we give up and CANCEL.
const RING_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RingOutcome {
    /// Answered; the lawyer is now on the line (agent runs in background).
    Answered,
    NoAnswer,
    Rejected,
    Busy,
    NotRegistered,
}

pub async fn ring_out(ctx: Arc<SipCtx>, reason: Option<String>) -> Result<RingOutcome> {
    if ctx.shared.calls.busy() {
        return Ok(RingOutcome::Busy);
    }
    // Prefer the configured ATA user; fall back to any live registration so
    // softphone testing works without config churn.
    let target = match ctx
        .shared
        .registrar
        .get(&ctx.shared.cfg.sip.ata_user)
        .or_else(|| ctx.shared.registrar.any())
    {
        Some(t) => t,
        None => return Ok(RingOutcome::NotRegistered),
    };
    tracing::info!(user = %target.username, dest = %target.destination, ?reason, "ringing out");

    let socket = rtp::bind_socket(&ctx.shared.cfg.rtp).await?;
    let offer = sdp::build(
        &ctx.advertise_ip,
        socket.local_addr()?.port(),
        rand::random::<u16>() as u32,
    );

    let callee = rsip::Uri {
        scheme: Some(rsip::Scheme::Sip),
        auth: Some(rsip::Auth { user: target.username.clone(), password: None }),
        host_with_port: target.destination.addr.clone(),
        params: vec![],
        headers: vec![],
    };
    let opt = InviteOption {
        callee,
        caller: ctx.contact.clone(),
        contact: ctx.contact.clone(),
        destination: Some(target.destination.clone()),
        content_type: Some("application/sdp".into()),
        offer: Some(offer.into_bytes()),
        credential: None,
        ..Default::default()
    };

    let (dialog, handle) = ctx
        .dialog_layer
        .do_invite_async(opt, ctx.state_sender.clone())
        .map_err(|e| anyhow::anyhow!("do_invite_async: {e}"))?;

    let final_resp = tokio::select! {
        r = handle => match r {
            Ok(Ok((_id, resp))) => resp,
            Ok(Err(e)) => {
                tracing::warn!("ring-out invite failed: {e}");
                return Ok(RingOutcome::Rejected);
            }
            Err(e) => return Err(anyhow::anyhow!("invite task panicked: {e}")),
        },
        _ = tokio::time::sleep(RING_TIMEOUT) => {
            tracing::info!("no answer within {RING_TIMEOUT:?}, cancelling");
            dialog.cancel().await.ok();
            return Ok(RingOutcome::NoAnswer);
        }
    };

    let resp = match final_resp {
        Some(r) if r.status_code == rsip::StatusCode::OK => r,
        Some(r) => {
            tracing::info!(status = %r.status_code, "ring-out refused");
            return Ok(RingOutcome::Rejected);
        }
        None => return Ok(RingOutcome::Rejected),
    };

    let answer = String::from_utf8_lossy(resp.body()).to_string();
    let media = sdp::parse(&answer).context("parsing ring-out answer SDP")?;
    let peer: SocketAddr = format!("{}:{}", media.ip, media.port).parse()?;

    let token = dialog.cancel_token().child_token();
    let id = dialog.id();
    ctx.shared.calls.begin(ActiveCall {
        id: id.clone(),
        kind: CallKind::Outbound,
        token: token.clone(),
        started: Instant::now(),
        remote: target.destination.to_string(),
    });

    let recorder = ctx.shared.recording_dir.as_ref().map(|_| {
        Arc::new(crate::recorder::CallRecorder::new(
            "outbound",
            format!("{}:{}", media.ip, media.port),
        ))
    });
    let session = rtp::start(socket, peer, media.dtmf_pt, token.clone(), recorder.clone())?;
    let ctx2 = ctx.clone();
    let note = Some(reason.unwrap_or_else(|| {
        "checking in on your case unprompted, as good lawyers do".to_string()
    }));
    if let (Some(rec), Some(n)) = (&recorder, &note) {
        rec.note("ring_out_reason", n.clone());
    }
    tokio::spawn(async move {
        if let Err(e) = agent::run(&ctx2.shared, session, token.clone(), note).await {
            tracing::warn!("outbound agent failed: {e:#}");
        }
        if !token.is_cancelled() {
            dialog.bye().await.ok();
        }
        ctx2.shared.calls.end(&id);
        if let (Some(rec), Some(dir)) = (recorder, &ctx2.shared.recording_dir) {
            if let Err(e) = rec.finalize(dir) {
                tracing::warn!("recording finalize failed: {e:#}");
            }
        }
    });

    Ok(RingOutcome::Answered)
}
