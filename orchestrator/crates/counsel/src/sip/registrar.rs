//! Minimal registrar for one ATA (plus any softphone used in testing).
//! v1 trusts the LAN: no digest challenge. The HT801 is configured with
//! "Allow Incoming SIP Messages from SIP Proxy Only" as the belt to this
//! suspender.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rsipstack::sip as rsip;
use rsip::prelude::{HeadersExt, ToTypedHeader};
use rsipstack::transaction::transaction::Transaction;
use rsipstack::transport::SipAddr;

#[derive(Clone, Debug)]
pub struct RegisteredUser {
    pub username: String,
    /// Where INVITEs for this user go — Contact host:port corrected by Via
    /// received/rport, so NAT'd softphones work too.
    pub destination: SipAddr,
    pub expires: u32,
    pub at: Instant,
}

#[derive(Default)]
pub struct RegistrationStore {
    users: Mutex<HashMap<String, RegisteredUser>>,
}

impl RegistrationStore {
    pub fn get(&self, username: &str) -> Option<RegisteredUser> {
        let users = self.users.lock().unwrap();
        users.get(username).cloned().filter(|u| !u.expired())
    }

    /// Any live registration — the ring-out target when the configured ATA
    /// user isn't registered but a test softphone is.
    pub fn any(&self) -> Option<RegisteredUser> {
        let users = self.users.lock().unwrap();
        users.values().find(|u| !u.expired()).cloned()
    }

    pub fn snapshot(&self) -> Vec<RegisteredUser> {
        let users = self.users.lock().unwrap();
        users.values().filter(|u| !u.expired()).cloned().collect()
    }

    fn insert(&self, user: RegisteredUser) {
        self.users.lock().unwrap().insert(user.username.clone(), user);
    }

    fn remove(&self, username: &str) {
        self.users.lock().unwrap().remove(username);
    }
}

impl RegisteredUser {
    fn expired(&self) -> bool {
        self.at.elapsed() > Duration::from_secs(self.expires as u64 + 15)
    }
}

/// Handle a REGISTER transaction (mirrors rsipstack's proxy example):
/// store Contact corrected by Via received/rport, echo Expires, 200 OK.
pub async fn handle_register(
    store: &RegistrationStore,
    tx: &mut Transaction,
) -> rsipstack::Result<()> {
    let username = tx
        .original
        .from_header()?
        .uri()?
        .user()
        .unwrap_or_default()
        .to_string();

    let contact_uri = match tx.original.typed_contact_headers()?.first().map(|c| c.uri.clone()) {
        Some(u) => u,
        None => {
            tracing::info!(user = %username, "REGISTER without Contact");
            return tx.reply(rsip::StatusCode::BadRequest).await;
        }
    };

    let via = tx.original.via_header()?.typed()?;
    let mut destination = SipAddr {
        r#type: Some(via.transport),
        addr: contact_uri.host_with_port.clone(),
    };
    for param in via.params.iter() {
        match param {
            rsip::Param::Transport(t) => destination.r#type = Some(t.clone()),
            rsip::Param::Received(r) => {
                if let Ok(host) = r.value().try_into() {
                    destination.addr.host = host;
                }
            }
            rsip::Param::Rport(Some(port)) => destination.addr.port = Some((*port).into()),
            _ => {}
        }
    }

    // Requested lifetime: Expires header, else default. (Contact ;expires
    // params are rare from ATAs; skipped.)
    let expires: u32 = tx
        .original
        .expires_header()
        .and_then(|e| e.value().parse().ok())
        .unwrap_or(120);

    if expires == 0 {
        tracing::info!(user = %username, "unregistered");
        store.remove(&username);
        return tx.reply(rsip::StatusCode::OK).await;
    }

    tracing::info!(user = %username, dest = %destination, expires, "registered");
    store.insert(RegisteredUser {
        username,
        destination,
        expires,
        at: Instant::now(),
    });

    let contact = rsip::typed::Contact {
        display_name: None,
        uri: contact_uri,
        params: vec![rsip::Param::Expires(expires.to_string().into())],
    };
    let headers = vec![contact.into(), rsip::Header::Expires(expires.into())];
    tx.reply_with(rsip::StatusCode::OK, headers, None).await
}
