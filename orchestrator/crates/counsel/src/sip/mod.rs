//! SIP endpoint: registrar for the ATA, UAS for off-hook calls, and (M4)
//! UAC for ring-out. Signaling only — media lives in `crate::rtp`.

pub mod inbound;
pub mod outbound;
pub mod registrar;
pub mod sdp;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use rsipstack::dialog::dialog::{Dialog, DialogState, DialogStateReceiver, DialogStateSender};
use rsipstack::dialog::dialog_layer::DialogLayer;
use rsipstack::sip as rsip;
use rsipstack::transaction::TransactionReceiver;
use rsipstack::transport::{udp::UdpConnection, TransportLayer};
use rsipstack::EndpointBuilder;
use tokio_util::sync::CancellationToken;

use crate::http::Shared;

/// Everything the per-call flows need.
pub struct SipCtx {
    pub shared: Shared,
    pub advertise_ip: String,
    pub contact: rsip::Uri,
    pub dialog_layer: Arc<DialogLayer>,
    pub state_sender: DialogStateSender,
}

/// A ring-out request from the control plane.
pub struct RingRequest {
    pub reason: Option<String>,
    pub reply: tokio::sync::oneshot::Sender<anyhow::Result<outbound::RingOutcome>>,
}

/// Best-effort LAN IP: route lookup via a connected UDP socket (no packets
/// are sent). Good enough on a single-homed booth machine; multi-homed
/// hosts set [sip].advertise_ip explicitly.
pub fn detect_lan_ip() -> Result<IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0")?;
    s.connect("8.8.8.8:80")?;
    Ok(s.local_addr()?.ip())
}

pub async fn run(
    shared: Shared,
    mut ring_rx: tokio::sync::mpsc::Receiver<RingRequest>,
    token: CancellationToken,
) -> Result<()> {
    let bind: SocketAddr = shared
        .cfg
        .sip
        .bind
        .parse()
        .with_context(|| format!("parsing sip.bind {:?}", shared.cfg.sip.bind))?;

    let advertise_ip = if shared.cfg.sip.advertise_ip.is_empty() {
        let ip = detect_lan_ip().context("auto-detecting LAN IP (set sip.advertise_ip)")?;
        tracing::info!(%ip, "auto-detected advertise IP");
        ip.to_string()
    } else {
        shared.cfg.sip.advertise_ip.clone()
    };

    // When bound to 0.0.0.0, tell the transport what to put in Via/Contact.
    let external: Option<SocketAddr> = if bind.ip().is_unspecified() {
        Some(SocketAddr::new(advertise_ip.parse()?, bind.port()))
    } else {
        None
    };

    let transport_layer = TransportLayer::new(token.clone());
    let connection = UdpConnection::create_connection(bind, external, Some(token.child_token()))
        .await
        .map_err(|e| anyhow::anyhow!("binding SIP UDP {bind}: {e}"))?;
    transport_layer.add_transport(connection.into());

    let endpoint = EndpointBuilder::new()
        .with_user_agent("wetcourt-counsel/0.1")
        .with_cancel_token(token.clone())
        .with_transport_layer(transport_layer)
        .build();

    let incoming = endpoint
        .incoming_transactions()
        .map_err(|e| anyhow::anyhow!("incoming_transactions: {e}"))?;
    let dialog_layer = Arc::new(DialogLayer::new(endpoint.inner.clone()));
    let (state_sender, state_receiver) = dialog_layer.new_dialog_state_channel();

    let contact = rsip::Uri {
        scheme: Some(rsip::Scheme::Sip),
        auth: Some(rsip::Auth {
            user: shared.cfg.sip.lawyer_user.clone(),
            password: None,
        }),
        host_with_port: SocketAddr::new(advertise_ip.parse()?, bind.port()).into(),
        params: vec![],
        headers: vec![],
    };

    let ctx = Arc::new(SipCtx {
        shared,
        advertise_ip: advertise_ip.clone(),
        contact,
        dialog_layer: dialog_layer.clone(),
        state_sender: state_sender.clone(),
    });

    tracing::info!(%bind, %advertise_ip, "SIP endpoint up");

    // Control-plane ring-out requests. Sequential by design — one line.
    let ring_ctx = ctx.clone();
    tokio::spawn(async move {
        while let Some(req) = ring_rx.recv().await {
            let outcome = outbound::ring_out(ring_ctx.clone(), req.reason).await;
            req.reply.send(outcome).ok();
        }
    });

    tokio::select! {
        _ = endpoint.serve() => {
            tracing::warn!("SIP endpoint serve() returned");
        }
        r = incoming_loop(ctx.clone(), incoming) => {
            tracing::warn!("SIP incoming loop finished: {r:?}");
        }
        r = dialog_pump(ctx, state_receiver, dialog_layer) => {
            tracing::warn!("SIP dialog pump finished: {r:?}");
        }
        _ = token.cancelled() => {
            tracing::info!("SIP endpoint shutting down");
        }
    }
    Ok(())
}

/// Dispatch raw incoming transactions: in-dialog requests to their dialog,
/// REGISTER to the registrar, new INVITEs toward a server dialog.
async fn incoming_loop(ctx: Arc<SipCtx>, mut incoming: TransactionReceiver) -> Result<()> {
    use rsip::prelude::HeadersExt;

    while let Some(mut tx) = incoming.recv().await {
        tracing::debug!(key = ?tx.key, "incoming transaction");

        // In-dialog request (has a To tag): route to the live dialog.
        if tx
            .original
            .to_header()
            .ok()
            .and_then(|h| h.tag().ok().flatten())
            .is_some()
        {
            match ctx.dialog_layer.match_dialog(&tx) {
                Some(mut d) => {
                    tokio::spawn(async move {
                        if let Err(e) = d.handle(&mut tx).await {
                            tracing::debug!("in-dialog handle: {e}");
                        }
                    });
                }
                None => {
                    tx.reply(rsip::StatusCode::CallTransactionDoesNotExist)
                        .await
                        .ok();
                }
            }
            continue;
        }

        match tx.original.method {
            rsip::Method::Register => {
                if let Err(e) = registrar::handle_register(&ctx.shared.registrar, &mut tx).await {
                    tracing::warn!("REGISTER handling failed: {e}");
                }
            }
            rsip::Method::Invite | rsip::Method::Ack => {
                if tx.original.method == rsip::Method::Invite && ctx.shared.calls.busy() {
                    tracing::info!("INVITE while busy → 486");
                    tx.reply(rsip::StatusCode::BusyHere).await.ok();
                    continue;
                }
                let mut dialog = match ctx.dialog_layer.get_or_create_server_invite(
                    &tx,
                    ctx.state_sender.clone(),
                    None,
                    Some(ctx.contact.clone()),
                ) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::info!("failed to create server dialog: {e}");
                        tx.reply(rsip::StatusCode::CallTransactionDoesNotExist)
                            .await
                            .ok();
                        continue;
                    }
                };
                tokio::spawn(async move {
                    if let Err(e) = dialog.handle(&mut tx).await {
                        tracing::debug!("invite dialog handle: {e}");
                    }
                });
            }
            rsip::Method::Options => {
                tx.reply(rsip::StatusCode::OK).await.ok();
            }
            _ => {
                tx.reply(rsip::StatusCode::MethodNotAllowed).await.ok();
            }
        }
    }
    Ok(())
}

/// React to dialog lifecycle events: new server INVITEs get answered, and
/// terminated dialogs release the line.
async fn dialog_pump(
    ctx: Arc<SipCtx>,
    mut state_receiver: DialogStateReceiver,
    dialog_layer: Arc<DialogLayer>,
) -> Result<()> {
    while let Some(state) = state_receiver.recv().await {
        match state {
            DialogState::Calling(id) => {
                let Some(dialog) = dialog_layer.get_dialog(&id) else {
                    continue;
                };
                match dialog {
                    Dialog::ServerInvite(d) => {
                        let ctx = ctx.clone();
                        tokio::spawn(async move {
                            if let Err(e) = inbound::answer(ctx, d).await {
                                tracing::warn!("inbound call failed: {e:#}");
                            }
                        });
                    }
                    Dialog::ClientInvite(_) => {
                        // Ring-out dialogs are driven by their originator.
                    }
                    _ => {}
                }
            }
            DialogState::Terminated(id, reason) => {
                tracing::info!(%id, ?reason, "dialog terminated");
                ctx.shared.calls.end(&id);
                dialog_layer.remove_dialog(&id);
            }
            other => {
                tracing::debug!("dialog state: {other}");
            }
        }
    }
    Ok(())
}
