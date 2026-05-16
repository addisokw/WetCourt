import { onMount } from 'solid-js';
import { connect } from './ws';
import JudgeFace from './JudgeFace';

// Standalone face-only view served at /face — meant to be opened fullscreen on
// the dedicated judge monitor. It subscribes to the same WebSocket as the main
// kiosk so the face reacts to trial events in real time, but renders no
// controls, no deliberation text, and no log.
export default function FaceView() {
  onMount(() => {
    connect({ readOnly: true });
  });

  return (
    <div class="face-view">
      <JudgeFace />
    </div>
  );
}
