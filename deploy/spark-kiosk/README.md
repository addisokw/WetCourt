# Spark kiosk — monitor, speakers, and microphone on the Spark itself

With the orchestrator running on the Spark (shape A) and a monitor + USB
speaker + USB microphone plugged into it, one auto-started Chromium kiosk
makes the Spark the booth's whole A/V head:

```
http://localhost:8080/case?audio=1&mic=1
        │            │        └─ this browser records the plea and uploads it
        │            └─ this browser plays the TTS + theater pad
        └─ the defendant/visitor-facing case display
```

The operator console (opened from any phone/laptop on the LAN) keeps every
control. Division of labor during a trial:

- **Kiosk** (this unit): shows the case, speaks the judge, records the plea.
  The server accepts plea audio only from the *newest* `?mic=1` client, so a
  restarted kiosk cleanly reclaims both audio and mic.
- **Operator console**: all controls. Its plea button/`P` shows
  "— kiosk mic" and closes the plea window early through the server (same
  path as the defendant's done-talking button), which flushes the kiosk's
  recording. If the kiosk dies mid-window, the console automatically takes
  the mic back (`mic_owner` broadcast) so the plea isn't lost — and mute your
  operator device's volume, it also renders the TTS.

## One-time setup on the Spark

1. **Chromium**: `sudo snap install chromium`

2. **Auto-login** (GDM): in `/etc/gdm3/custom.conf` under `[daemon]`:

   ```ini
   AutomaticLoginEnable=true
   AutomaticLogin=<user>
   ```

3. **Never blank/lock/sleep** (as that user, in the graphical session):

   ```sh
   gsettings set org.gnome.desktop.session idle-delay 0
   gsettings set org.gnome.desktop.screensaver lock-enabled false
   gsettings set org.gnome.settings-daemon.plugins.power sleep-inactive-ac-type 'nothing'
   ```

4. **Default audio devices** — point PipeWire at the USB speaker and mic:

   ```sh
   wpctl status                      # find the sink/source IDs
   wpctl set-default <speaker-id>
   wpctl set-default <mic-id>
   wpctl set-volume @DEFAULT_AUDIO_SINK@ 1.0
   ```

   WirePlumber remembers defaults per-device. **Reboot once and re-check** —
   USB audio re-enumeration is the classic event-day failure.

5. **Install + enable the kiosk unit**:

   ```sh
   mkdir -p ~/.config/systemd/user
   cp wetcourt-kiosk.service ~/.config/systemd/user/
   systemctl --user daemon-reload
   systemctl --user enable --now wetcourt-kiosk
   ```

## Verify (mock trial)

1. Kiosk shows the case view full-screen; booth log shows
   `view ws client connected audio=true mic=true`.
2. Start a trial from the operator console: charge audio comes out of the
   Spark's speaker.
3. During the plea window the console banner reads "Recording on the booth
   mic (kiosk)"; press `P` — the window closes and the transcript appears.
4. Pull the kiosk (`systemctl --user stop wetcourt-kiosk`) mid-window: the
   console starts recording on its own mic within a second (banner flips).
   Start it again; it reclaims speakers + mic on the next trial.

## Troubleshooting

- **No sound**: `wpctl status` — is the USB speaker the default sink? Is the
  kiosk actually the newest audio client (reload it)?
- **Plea records silence**: `wpctl status` source; check input volume
  `wpctl set-volume @DEFAULT_AUDIO_SOURCE@ 1.0`.
- **Stop the kiosk from a laptop**: `ssh <spark> systemctl --user stop
  wetcourt-kiosk` (the operator console takes the mic; audio falls back to
  the console device).
- The kiosk needs the orchestrator up (`docker ps` → `orchestrator`); it
  retries on its own (`Restart=always`) and the page's websocket reconnects,
  so order doesn't matter after a reboot.
