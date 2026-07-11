import { createSignal, onCleanup, Show, type JSX } from 'solid-js';

// The turret camera image, reverse-proxied by the orchestrator at /vision/*
// (so it stays same-origin and works through the tunnel for remote operators).
//
// Snapshot polling instead of the MJPEG /feed stream: Safari's <img> renders
// single JPEGs everywhere but errors on the endless multipart/x-mixed-replace
// stream when it's proxied through the orchestrator's keep-alive connection.
// We chain requests on each frame's load, so it self-paces to ~15–25 fps.
// A transient error just retries slower; whether the process is offline is the
// parent's call (via /vision/state polling), not a dropped frame.
export default function VisionFeed(props: {
  online: boolean;
  class?: string;
  onFeedClick?: JSX.EventHandler<HTMLImageElement, MouseEvent>;
  /** Rendered inside the offline overlay when `online` is false. */
  children?: JSX.Element;
}) {
  const [snapUrl, setSnapUrl] = createSignal('/vision/snapshot?t=0');
  let img: HTMLImageElement | undefined;
  let snapTimer: number | undefined;
  let stopped = false;
  let seq = 0;

  function nextSnapshot() {
    if (stopped) return;
    // The operator console stays mounted (display: none) behind other tabs —
    // don't keep pulling frames while nobody can see them.
    if (img && img.offsetParent === null) {
      snapTimer = window.setTimeout(nextSnapshot, 500);
      return;
    }
    seq += 1;
    setSnapUrl(`/vision/snapshot?t=${Date.now()}.${seq}`);
  }
  function onSnapLoad() {
    if (!stopped) snapTimer = window.setTimeout(nextSnapshot, 40);
  }
  function onSnapError() {
    if (!stopped) snapTimer = window.setTimeout(nextSnapshot, 500);
  }

  onCleanup(() => {
    stopped = true;
    if (snapTimer) clearTimeout(snapTimer);
  });

  return (
    <div class={`vision-feed ${props.class ?? ''}`}>
      <img
        ref={img}
        src={snapUrl()}
        alt="turret camera"
        onClick={(e) => props.onFeedClick?.(e)}
        onLoad={onSnapLoad}
        onError={onSnapError}
      />
      <Show when={!props.online}>
        <div class="vision-offline">{props.children}</div>
      </Show>
    </div>
  );
}
