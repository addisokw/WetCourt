# Homelab self-hosting

Run the orchestrator as an always-on service on a homelab box so your team can
reach the operator + persona/crime panels from a browser — no local install, no
Tailscale on their machines. Inference still runs on the remote Spark.

```
  teammates' browsers ──HTTPS──►  cloudflared (host service, already running)
   (no Tailscale needed)                 │  public hostname → http://localhost:26878
                                         ▼
                          orchestrator container  :8080   (hardware.driver = mock)
                          published to host  127.0.0.1:26878
                                         │  shares netns with ↓
                          tailscale sidecar ──tailnet──►  Spark LiteLLM :4000
                          (the ONLY node you add to your tailnet)
```

This is deployment **shape B** from [`orchestrator/README.md`](../../orchestrator/README.md),
made persistent and put behind your existing Cloudflare Tunnel.

## What's here

| File | Purpose |
|---|---|
| `docker-compose.yml` | orchestrator + a Tailscale sidecar; publishes the UI on `127.0.0.1:${HOST_PORT}` (default `26878`) |
| `config.homelab.toml` | inference → Spark over Tailscale, `hardware.driver = mock` |
| `.env.example` | the two secrets + node hostname (copy to `.env`) |

`cloudflared` is intentionally **not** here — you run it as a host service.

## Prerequisites

- Docker + Docker Compose on the homelab box, with `/dev/net/tun` available
  (standard on Linux — the Tailscale sidecar uses kernel networking).
- A **Tailscale auth key** (reusable recommended): <https://login.tailscale.com/admin/settings/keys>.
- The Spark's **`LITELLM_MASTER_KEY`** — copy it here; it lives only on the Spark.
- Your existing host `cloudflared`.

## Bring it up

```bash
cd deploy/homelab
cp .env.example .env          # then fill in TS_AUTHKEY and LITELLM_MASTER_KEY
docker compose up -d --build
```

Verify:

```bash
docker compose logs -f tailscale      # node should authenticate and come up
docker compose logs -f orchestrator   # "display server listening on 0.0.0.0:8080" (container-internal)
curl -fsS http://localhost:26878/health   # → ok   (host side; HOST_PORT)
```

The node should now appear in your Tailscale admin console, and the box can
reach the Spark at `100.86.115.53:4000`.

## Point Cloudflare at it

Since cloudflared already runs as a service, just add one **public hostname**
to the same tunnel (it routes by hostname, so it won't disturb your nginx `:80`):

- **Subdomain/Domain:** e.g. `court.yourdomain.com`
- **Service:** `HTTP` → `localhost:26878` (or your `HOST_PORT`)

Then gate it: Zero Trust → **Access → Applications → Self-hosted** on
`court.yourdomain.com`, scoped to your team by email/SSO. The `/operator/*`
endpoints have no auth of their own, so Access is the gate. WebSockets
(`/ws`, `/ws/view`) and the HTTPS the tunnel provides (needed for the plea mic's
secure-context requirement) both work with no extra config.

## Good to know

- **Persona/crime edits persist.** `personas/` and `crimes/` are bind-mounted
  from the repo checkout, so changes made in the panels land on disk and can be
  committed. (Back up or `git commit` periodically.)
- **One live operator console at a time.** `/ws` is single-client by design (one
  mic, one audio output, one hardware lane); a second person opening the main
  page gets their live socket refused. But the **persona and crime panels are
  plain REST**, so the whole team can curate them concurrently, and read-only
  `/face` + `/case` monitors use the multi-client `/ws/view`.
- **Override any config knob** via `BOOTH__…` env vars in `docker-compose.yml`
  (e.g. `BOOTH__TRIAL__PLEA_WINDOW_SECS=30`). To let a real booth dial in, set
  `BOOTH__HARDWARE__DRIVER=tcp` and make `:8090` reachable to the MCU.
- **Updating:** `git pull && docker compose up -d --build`.
