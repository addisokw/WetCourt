import { createSignal, onCleanup, onMount, Show } from 'solid-js';

// Status shape from counsel's /status, proxied at /lawyer/status.
type LawyerStatus = {
  registered: boolean;
  registrations: Array<{ user: string; destination: string; age_secs: number }>;
  call: {
    active: { kind: string; remote: string; elapsed_secs: number } | null;
    last: { kind: string; remote: string; duration_secs: number } | null;
  };
};

export default function LawyerPanel() {
  const [status, setStatus] = createSignal<LawyerStatus | null>(null);
  const [offline, setOffline] = createSignal(true);
  const [reason, setReason] = createSignal('');
  const [calling, setCalling] = createSignal(false);
  const [outcome, setOutcome] = createSignal('');

  let timer: ReturnType<typeof setInterval> | undefined;

  async function poll() {
    try {
      const r = await fetch('/lawyer/status');
      if (!r.ok) throw new Error(String(r.status));
      setStatus(await r.json());
      setOffline(false);
    } catch {
      setOffline(true);
      setStatus(null);
    }
  }

  onMount(() => {
    poll();
    timer = setInterval(poll, 3000);
  });
  onCleanup(() => timer && clearInterval(timer));

  async function ring() {
    setCalling(true);
    setOutcome('');
    try {
      const r = await fetch('/lawyer/call', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(reason() ? { reason: reason() } : {}),
      });
      const body = await r.json().catch(() => ({}));
      setOutcome(body.outcome ?? body.error ?? `HTTP ${r.status}`);
    } catch (e) {
      setOutcome(String(e));
    } finally {
      setCalling(false);
    }
  }

  return (
    <div class="panel lawyer-panel">
      <h2>Call-your-lawyer phone</h2>

      <p>
        <span class={`device-badge ${!offline() && status()?.registered ? 'up' : 'down'}`}>
          <span class="dot" />{' '}
          {offline()
            ? 'lawyer service offline'
            : status()?.registered
              ? `phone registered (${status()!
                  .registrations.map((r) => r.user)
                  .join(', ')})`
              : 'no phone registered'}
        </span>
      </p>

      <Show when={status()?.call.active}>
        {(c) => (
          <p>
            On a call: {c().kind} with {c().remote} — {c().elapsed_secs}s
          </p>
        )}
      </Show>
      <Show when={!status()?.call.active && status()?.call.last}>
        {(c) => (
          <p class="muted">
            Last call: {c().kind}, {c().duration_secs}s
          </p>
        )}
      </Show>

      <h3>Ring the booth phone</h3>
      <p class="muted">
        "Your lawyer is calling YOU." The lawyer opens the call around the
        reason below; blocks up to ~25s while it rings.
      </p>
      <div>
        <input
          type="text"
          placeholder="reason for the call (e.g. the verdict is in)"
          size={44}
          value={reason()}
          onInput={(e) => setReason(e.currentTarget.value)}
          disabled={calling()}
        />{' '}
        <button
          onClick={ring}
          disabled={calling() || offline() || !!status()?.call.active}
        >
          {calling() ? 'ringing…' : 'Call the defendant’s lawyer'}
        </button>{' '}
        <Show when={outcome()}>
          <span class="ack-chip">{outcome()}</span>
        </Show>
      </div>
    </div>
  );
}
