//! Inbound call flow: the defendant lifted the handset (HT801 off-hook
//! auto-dial) — parse the offer, stand up RTP, ring briefly for realism,
//! answer, and hand the media session to the call loop.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rsipstack::dialog::server_dialog::ServerInviteDialog;
use rsipstack::sip as rsip;
use rsip::prelude::HeadersExt;

use crate::call::{ActiveCall, CallKind};
use crate::rtp;
use crate::sip::{sdp, SipCtx};

/// Ring for a beat before answering; instant pickup reads as broken.
const RING_BEAT: Duration = Duration::from_millis(600);

pub async fn answer(ctx: Arc<SipCtx>, dialog: ServerInviteDialog) -> Result<()> {
    let id = dialog.id();
    let token = dialog.cancel_token().child_token();
    let remote = dialog
        .initial_request()
        .from_header()
        .ok()
        .and_then(|f| f.uri().ok())
        .map(|u| u.to_string())
        .unwrap_or_else(|| "unknown".into());

    let call = ActiveCall {
        id: id.clone(),
        kind: CallKind::Inbound,
        token: token.clone(),
        started: Instant::now(),
        remote: remote.clone(),
    };
    if !ctx.shared.calls.begin(call) {
        // Raced another call between the busy check and here.
        dialog
            .reject(Some(rsip::StatusCode::BusyHere), Some("Busy Here".into()))
            .ok();
        return Ok(());
    }
    tracing::info!(%remote, "inbound call");

    let result = run_call(&ctx, &dialog, token.clone()).await;

    // If the far side already hung up the token is cancelled and BYE would 481.
    if !token.is_cancelled() {
        dialog.bye().await.ok();
    }
    ctx.shared.calls.end(&id);
    result
}

async fn run_call(
    ctx: &Arc<SipCtx>,
    dialog: &ServerInviteDialog,
    token: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let body = String::from_utf8_lossy(dialog.initial_request().body()).to_string();
    let media = match sdp::parse(&body) {
        Ok(m) => m,
        Err(e) => {
            dialog
                .reject(
                    Some(rsip::StatusCode::NotAcceptableHere),
                    Some("Not Acceptable Here".into()),
                )
                .ok();
            return Err(e).context("parsing INVITE offer SDP");
        }
    };

    let socket = rtp::bind_socket(&ctx.shared.cfg.rtp).await?;
    let answer_sdp = sdp::build(
        &ctx.advertise_ip,
        socket.local_addr()?.port(),
        rand::random::<u16>() as u32,
    );

    let headers = vec![rsip::Header::ContentType("application/sdp".into())];
    dialog
        .ringing(Some(headers.clone()), Some(answer_sdp.clone().into_bytes()))
        .map_err(|e| anyhow::anyhow!("sending 180: {e}"))?;
    tokio::time::sleep(RING_BEAT).await;
    if token.is_cancelled() {
        return Ok(()); // caller hung up while "ringing"
    }
    dialog
        .accept(Some(headers), Some(answer_sdp.into_bytes()))
        .map_err(|e| anyhow::anyhow!("sending 200: {e}"))?;

    let peer: SocketAddr = format!("{}:{}", media.ip, media.port)
        .parse()
        .with_context(|| format!("peer addr {}:{}", media.ip, media.port))?;
    let recorder = ctx.shared.recording_dir.as_ref().map(|_| {
        Arc::new(crate::recorder::CallRecorder::new(
            "inbound",
            format!("{}:{}", media.ip, media.port),
        ))
    });
    let session = rtp::start(socket, peer, media.dtmf_pt, token.clone(), recorder.clone())?;

    let result = crate::call::session_loop(&ctx.shared, session, token).await;
    if let (Some(rec), Some(dir)) = (recorder, &ctx.shared.recording_dir) {
        if let Err(e) = rec.finalize(dir) {
            tracing::warn!("recording finalize failed: {e:#}");
        }
    }
    result
}
