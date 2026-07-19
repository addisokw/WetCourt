use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DisplayEvent {
    Reset,
    Idle,
    /// Connect-time resync: the full current trial view-state, sent as the
    /// first event on every `/ws` and `/ws/view` connection so a client that
    /// (re)connects mid-trial renders the live phase instead of stale idle.
    /// Verdict fields are withheld until `executing_sentence` — during
    /// `pronouncing_verdict` the reveal may not have happened yet.
    Snapshot {
        phase: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        charge: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        plea: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cross_question: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        verdict_guilty: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        verdict_remarks: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        verdict_key_factor: Option<String>,
        /// Remaining ms of the current window (plea/answer/sentence hold).
        /// When `clock_paused` is set this is the frozen remaining time.
        #[serde(skip_serializing_if = "Option::is_none")]
        deadline_ms: Option<u64>,
        /// The window's clock is paused for a lawyer consultation.
        clock_paused: bool,
        maintenance: bool,
        /// A dedicated booth-mic client (`/ws/view?mic=1` kiosk) is live, so
        /// the operator console must keep its own microphone shut.
        mic_owner: bool,
        /// A dedicated booth-speaker client (`/ws/view?audio=1` kiosk) is live,
        /// so the operator console must mute its own TTS playback.
        #[serde(default)]
        audio_owner: bool,
        /// Secret operator macro codes armed for the next trial / latched into
        /// this one (resync for the case monitor's discreet indicator).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        operator_armed: Vec<u16>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        operator_active: Vec<u16>,
    },
    ShowCharge { text: String },
    /// "next binary frame is audio in this format" — emitted before each chunk
    /// so frontend knows how to decode. May appear multiple times per utterance
    /// in the pipelined LLM→TTS path.
    TtsAudio { format: String },
    /// "no more audio chunks for this utterance" — frontend schedules a
    /// `tts_finished` ClientEvent for after the queued audio drains.
    TtsEnd,
    StartPleaRecording {
        deadline_ms: u64,
        /// True when this window is a cross-examination *answer* (the case view
        /// prompts "answer the judge" instead of "begin your defense").
        #[serde(default)]
        cross: bool,
        /// False = the plea window just OPENED (show "press to begin", do NOT
        /// record yet). True = the defendant pressed the button to START
        /// recording — the mic client should begin capturing now. This makes the
        /// first press start the plea and the second press end it, so people who
        /// press to "start" no longer end their plea before speaking.
        #[serde(default)]
        record: bool,
    },
    StopPleaRecording,
    /// Broadcast when the operator's microphone actually starts/stops capturing
    /// — distinct from the plea *window* opening. Drives the case-view prompt
    /// ("press to begin" → "press to end") on read-only monitors.
    PleaRecording { active: bool },
    /// Operator-facing countdown helper. Emitted whenever the state machine
    /// enters a state with a deadline; `deadline_ms` is the duration from
    /// emission until the watchdog/timeout fires. Also re-emitted when a
    /// lawyer-paused clock resumes (with the remaining time).
    PhaseDeadline { phase: String, deadline_ms: u64 },
    /// The plea/answer countdown froze because the defendant picked up the
    /// lawyer phone; `remaining_ms` is the time left on the clock. Cleared by
    /// the next `PhaseDeadline` (resume) or `Reset`.
    ClockPaused { remaining_ms: u64 },
    /// The judge is ringing the defendant's counsel during cross-examination.
    /// `on:true` when the phone starts ringing, `on:false` once they pick up, the
    /// cross window closes, or on reset. Drives the "pick up the phone" overlay.
    LawyerCalling { on: bool },
    /// Header for a burst of lawyer call-audio PCM (F5) pushed to the primary
    /// speaker — like `TtsAudio` but routed to a telephone-band filter instead of
    /// the judge's robot voice. The following binary frame(s) carry the samples.
    LawyerAudio { format: String },
    /// "The court finds the defendant…" preamble is done; pause begins. The
    /// operator console plays an ambient pad and viewers dim until TheaterEnd.
    TheaterStart,
    TheaterEnd,
    Transcribing,
    TranscriptReady { text: String },
    /// The judge's cross-examination follow-up question; shown to viewers while
    /// it's spoken, before the answer-recording window opens.
    CrossQuestion { text: String },
    DeliberationToken { text: String },
    DeliberationComplete,
    Verdict { guilty: bool, remarks: String, #[serde(skip_serializing_if = "Option::is_none")] key_factor: Option<String> },
    ExecuteSentence { guilty: bool },
    /// A device announced itself (`HELLO <role>`) and was accepted; surfaced to
    /// the maintenance console so its tab enables.
    DeviceConnected { role: String, addr: String },
    /// A device's connection dropped.
    DeviceDisconnected { role: String },
    /// A wire `BUTTON` press arrived (the defendant's swear-in button).
    /// Surfaced to the console as a live press indicator — emitted for every
    /// press regardless of trial state, so bringup can see the switch work
    /// even while the FSM ignores the event (e.g. in maintenance mode).
    ButtonPressed { role: String },
    /// Maintenance mode entered/left — confirms the FSM transition to the
    /// console (whose direct-control tabs gate on it).
    Maintenance { active: bool },
    /// A trial `FIRE` was suppressed by the vision eye-safety gate (targeting was
    /// armed but vision had no fresh `fire_ok`). The trial still advances; this
    /// is operator feedback that the shot was *held*, not silently dropped.
    FireHeld { reason: String },
    /// The plea (or cross answer) fell back to "[no defense offered]" for a
    /// technical reason — STT failed or timed out — NOT because the defendant
    /// stayed silent. Operator banner so they can e-stop and retry instead of
    /// letting the defendant be judged on a defense they never got to make.
    PleaFallback { reason: String },
    /// The active persona's robot voice-effect params. Pushed to each audio
    /// client on connect and broadcast whenever the active persona changes or
    /// its robot settings are edited, so playback colour follows the persona.
    RobotParams {
        intensity: f32,
        glitch_rate: f32,
        ring_hz: f32,
        saturation: f32,
        peak_hz: f32,
        gain: f32,
    },
    /// Operator-facing problem banner (e.g. printer not ready / print failed).
    /// The show goes on; this is "something needs a human" feedback.
    Error { message: String },
    /// A dedicated booth-mic client (`/ws/view?mic=1`) connected or dropped.
    /// While present, the operator console suppresses its own microphone; on
    /// `present: false` mid-window the console takes the mic back so the plea
    /// isn't lost to a kiosk crash.
    MicOwner { present: bool },
    /// A dedicated booth-speaker client (`/ws/view?audio=1`) connected or
    /// dropped. While present, the operator console mutes its own TTS
    /// playback; on `present: false` the console resumes rendering so the show
    /// stays audible if the kiosk dies.
    AudioOwner { present: bool },
    /// Secret operator macro state: `armed` applies to the next trial,
    /// `active` is latched into the current one. The case monitor renders
    /// these as a discreet bare-number indicator for operator verification.
    OperatorModes { armed: Vec<u16>, active: Vec<u16> },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    Ready,
    PleaRecordingStarted,
    PleaAudioChunk,
    PleaAudioComplete,
    TtsFinished,
}
