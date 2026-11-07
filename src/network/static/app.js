/**
 * Bardwave Listener вЂ” WebSocket audio player
 *
 * Wire format (server -> browser):
 *   [seq_no: u32 LE][server_ms: u32 LE][one Opus packet, 10 ms, 48 kHz stereo]
 *
 * Playback uses an AudioWorklet with a ring buffer and a drift-correcting
 * jitter buffer:
 *   - wait for ~200 ms before starting
 *   - rebuffer when the queue drops below ~30 ms
 *   - hold the queue at the target by consuming at 1 +/- a small factor
 *     (fractional, Catmull-Rom interpolated read), so two independent audio
 *     clocks never accumulate latency and no PCM is ever hard-dropped in
 *     steady state; the factor follows the *smoothed* queue level and is
 *     capped at +/-0.5 %, so it never audibly wobbles the pitch
 *   - clear-and-rebuffer (a clean resync with fade) when the queue exceeds the
 *     hard cap, instead of skipping every other packet
 *   - fade in/out over ~8 ms to avoid clicks
 *
 * The worklet runs on the audio rendering thread and reports buffer/underrun
 * statistics back to the main thread. No per-callback allocation or slicing.
 *
 * The browser's AudioContext is created at the stream rate (48 kHz) when the
 * browser supports it; if it ends up at a different rate (see initAudio), the
 * worklet resamples the stream to the context rate on the fly through the same
 * fractional reader, so a 48 -> 44.1 kHz context no longer floods the queue.
 */

(function () {
    'use strict';

    // =========================================================================
    // Built-in icons
    //
    // The page used to load the Iconify web component from a CDN with a
    // blocking <script> in <head>. On a LAN without internet (or with the CDN
    // blocked) the page hung until that request timed out and then showed no
    // icons at all — and this page is meant to work with no cloud. The handful
    // of Lucide icons it uses (ISC licence) are inlined here instead, behind
    // the same <iconify-icon icon="lucide:..."> element, so the markup and the
    // `setAttribute('icon', ...)` calls stay as they were.
    // =========================================================================
    const ICON_SVG = {
        'lucide:zap': '<path d="M4 14a1 1 0 0 1-.78-1.63l9.9-10.2a.5.5 0 0 1 .86.46l-1.92 6.02A1 1 0 0 0 13 10h7a1 1 0 0 1 .78 1.63l-9.9 10.2a.5.5 0 0 1-.86-.46l1.92-6.02A1 1 0 0 0 11 14z"/>',
        'lucide:power': '<path d="M12 2v10"/><path d="M18.4 6.6a9 9 0 1 1-12.77.04"/>',
        'lucide:radio': '<path d="M4.9 19.1C1 15.2 1 8.8 4.9 4.9"/><path d="M7.8 16.2c-2.3-2.3-2.3-6.1 0-8.5"/><circle cx="12" cy="12" r="2"/><path d="M16.2 7.8c2.3 2.3 2.3 6.1 0 8.5"/><path d="M19.1 4.9C23 8.8 23 15.1 19.1 19"/>',
        'lucide:radio-tower': '<path d="M4.9 16.1C1 12.2 1 5.8 4.9 1.9"/><path d="M7.8 4.7a6.14 6.14 0 0 0-.8 7.5"/><circle cx="12" cy="9" r="2"/><path d="M16.9 4.7a6.14 6.14 0 0 1 .8 7.5"/><path d="M19.1 1.9a9.96 9.96 0 0 1 0 14.1"/><path d="M9.5 18h5"/><path d="m8 22 4-11 4 11"/>',
        'lucide:check-circle': '<circle cx="12" cy="12" r="10"/><path d="m9 12 2 2 4-4"/>',
        'lucide:alert-triangle': '<path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4"/><path d="M12 17h.01"/>',
        'lucide:info': '<circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/>',
        'lucide:chevron-down': '<path d="m6 9 6 6 6-6"/>'
    };

    if (window.customElements && !window.customElements.get('iconify-icon')) {
        class InlineIcon extends HTMLElement {
            static get observedAttributes() { return ['icon']; }
            connectedCallback() { this.render(); }
            attributeChangedCallback() { this.render(); }
            render() {
                const body = ICON_SVG[this.getAttribute('icon')] || '';
                this.innerHTML = '<svg xmlns="http://www.w3.org/2000/svg" width="1em" height="1em" ' +
                    'viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" ' +
                    'stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" ' +
                    'style="display:block">' + body + '</svg>';
            }
        }
        window.customElements.define('iconify-icon', InlineIcon);
    }

    // =========================================================================
    // Configuration
    // =========================================================================
    const CONFIG = {
        sampleRate: 48000,
        channels: 2,
        // Playback starts once this much audio is queued.
        targetBufferMs: 200,
        // If the queue falls below this, pause and rebuffer.
        lowWaterMs: 30,
        // Hard latency cap. Above this the queue is cleared and rebuffers on
        // fresh audio (a clean resync) instead of hard-dropping every push,
        // which is what produced continuous crackle before.
        maxBufferMs: 1200,
        // Ring buffer capacity, in stream time.
        capacityMs: 2500,
        // Frames this much older than the freshest arrival are dropped at
        // ingress (burst backlog from the server must not flood the queue).
        staleMs: 1200,
        // Largest consumption-speed correction of the drift controller:
        // +/-0.5 % (about 8 cents) is inaudible on ambience and still drains
        // 100 ms of excess latency in ~20 s. The old controller jumped
        // straight between 1.00 and 1.02 whenever the queue crossed a
        // threshold, i.e. on every 10 ms packet; that 100 Hz pitch wobble
        // is what made the stream sound like it was farting.
        catchupLimit: 0.005,
        // UI refresh interval for fallback stats (ms).
        uiUpdateInterval: 100,
        reconnect: {
            initialDelay: 1000,
            maxDelay: 30000,
            factor: 1.8,
            jitter: 0.3,
            maxAttempts: Infinity
        }
    };

    // =========================================================================
    // AudioWorklet processor
    // Injected as a Blob so the server does not need an extra static route.
    // =========================================================================
    const WORKLET_CODE = `
class SoundSharedStreamProcessor extends AudioWorkletProcessor {
    constructor(options) {
        super();
        const opts = (options && options.processorOptions) || {};
        this.channels = opts.channels || 2;
        this.sampleRate = (typeof sampleRate === 'number' && sampleRate > 0)
            ? sampleRate
            : (opts.sampleRate || 48000);

        // The wire stream rate. When the context rate differs the consumer
        // resamples on the fly; the ring is always measured in *stream* frames.
        this.streamRate = opts.streamRate || 48000;
        this.rateRatio = this.sampleRate > 0
            ? this.streamRate / this.sampleRate
            : 1;

        this.capacityFrames = Math.ceil(this.streamRate * (opts.capacitySeconds || 2.5));
        this.capacity = this.capacityFrames * this.channels;
        this.ring = new Float32Array(this.capacity);

        // Write head (integer stream frames) and read head (fractional stream
        // frames). Keeping the read fractional is what allows the consumption
        // speed to be corrected without ever mutating the buffered PCM.
        this.writeFrame = 0;
        this.readFrame = 0;
        this.available = 0;

        this.started = false;
        this.fadeGain = 0;
        this.fadeFrames = Math.max(1, Math.floor(this.sampleRate * 0.008));
        this.fadeStep = 1 / this.fadeFrames;

        this.targetFrames = Math.floor(this.streamRate * (opts.targetMs || 200) / 1000);
        this.lowWaterFrames = Math.floor(this.streamRate * (opts.lowWaterMs || 30) / 1000);
        this.maxFrames = Math.floor(this.streamRate * (opts.maxMs || 1200) / 1000);
        this.catchupLimit = opts.catchupLimit || 0.005;
        // Drift controller state (see updateSpeed()): smoothed queue level
        // and the current, slew-limited consumption speed.
        this.fillAvg = -1;
        this.speed = 1;
        // Last output frame, so a fade over an empty queue decays from the
        // real signal instead of stepping to zero.
        this.lastL = 0;
        this.lastR = 0;

        // End-of-stream detection. The server sends nothing while nothing
        // plays, so a short one-shot (shorter than the target pre-buffer)
        // never filled the queue far enough to start: it sat silent until
        // the *next* sound pushed the queue over the target, and then both
        // played back to back. Once no packet has arrived for idleFrames the
        // stream has paused: whatever is queued is played out right away and
        // to the very end ("draining"), instead of waiting or rebuffering.
        this.idleFrames = Math.floor(this.sampleRate * 0.12);
        this.sincePush = 0;
        this.draining = false;

        this.underruns = 0;
        this.overruns = 0;
        this.resyncs = 0;
        this.playedFrames = 0;
        this.receivedFrames = 0;
        this.lastReportPlayed = 0;
        this.lastReportReceived = 0;
        // Reused per-sample readout (no allocation on the audio thread).
        this.readOut = [0, 0];

        this.port.onmessage = (event) => this.onMessage(event.data);
    }

    onMessage(msg) {
        if (!msg) return;
        if (msg.type === 'push' && msg.samples instanceof Float32Array) {
            this.push(msg.samples);
        } else if (msg.type === 'reset') {
            this.reset();
        } else if (msg.type === 'target' && typeof msg.targetMs === 'number') {
            // Adaptive target (the main thread tracks arrival jitter).
            const frames = Math.floor(this.streamRate * msg.targetMs / 1000);
            this.targetFrames = Math.max(
                this.lowWaterFrames * 2,
                Math.min(this.maxFrames / 2, frames)
            );
        }
    }

    // Drift controller, run once per render quantum (not per sample).
    //
    // The queue level is smoothed over ~0.5 s, so the sawtooth of 10 ms
    // packet arrivals never reaches the speed; the correction is
    // proportional to the relative distance from the target, capped at
    // +/-catchupLimit, and its rate of change is limited too. The result
    // is a speed that wanders by hundredths of a percent instead of
    // jumping, which is inaudible.
    updateSpeed(blockFrames) {
        if (this.fillAvg < 0) this.fillAvg = this.available;
        const a = 1 - Math.exp(-blockFrames / (this.sampleRate * 0.5));
        this.fillAvg += (this.available - this.fillAvg) * a;
        const error = (this.fillAvg - this.targetFrames) / Math.max(1, this.targetFrames);
        const want = 1 + Math.max(-this.catchupLimit, Math.min(this.catchupLimit, error * 0.01));
        const slew = 0.00005;
        this.speed += Math.max(-slew, Math.min(slew, want - this.speed));
        return this.rateRatio * this.speed;
    }

    push(samples) {
        let frames = Math.floor(samples.length / this.channels);
        if (frames <= 0) return;

        this.receivedFrames += frames;
        this.sincePush = 0;
        this.draining = false;

        // Jumbo packet guard: one packet larger than the whole ring would
        // wrap the write past its own head. Drop the oldest part of it.
        if (frames > this.capacityFrames) {
            const skip = frames - this.capacityFrames;
            samples = samples.subarray(skip * this.channels);
            frames = this.capacityFrames;
            this.readFrame = this.writeFrame;
            this.available = 0;
            this.overruns++;
        } else if (this.available + frames > this.maxFrames) {
            // Hard cap: the queue is hopelessly stale. Clear it and rebuffer
            // on the fresh audio (the fade handles the transition, so it
            // stays silent) instead of peeling one packet off the head of
            // every push, which played every other packet for as long as the
            // queue sat at the cap.
            this.clear();
            this.resyncs++;
        } else if (this.available + frames > this.capacityFrames) {
            this.discard(Math.ceil(this.available + frames - this.capacityFrames));
            this.overruns++;
        }

        const first = Math.min(frames, this.capacityFrames - this.writeFrame);
        this.ring.set(
            samples.subarray(0, first * this.channels),
            this.writeFrame * this.channels
        );
        if (first < frames) {
            this.ring.set(
                samples.subarray(first * this.channels, frames * this.channels),
                0
            );
        }
        this.writeFrame = (this.writeFrame + frames) % this.capacityFrames;
        this.available += frames;

        if (!this.started && this.available >= this.targetFrames) {
            this.started = true;
            this.report();
        }

        // Report while the buffer is filling so the UI shows progress even
        // though the audio thread is not consuming frames yet.
        if (this.receivedFrames - this.lastReportReceived >= Math.floor(this.streamRate / 10)) {
            this.lastReportReceived = this.receivedFrames;
            this.report();
        }
    }

    // Remove whole frames from the head of the queue (capacity overrun path).
    discard(frames) {
        frames = Math.min(frames, Math.max(0, this.available));
        this.readFrame = (this.readFrame + frames) % this.capacityFrames;
        this.available -= frames;
    }

    // Clean resync: drop everything and rebuffer on fresh audio.
    clear() {
        this.available = 0;
        this.readFrame = this.writeFrame;
        this.started = false;
        this.fadeGain = 0;
        this.fillAvg = -1;
        this.speed = 1;
    }

    reset() {
        this.writeFrame = 0;
        this.readFrame = 0;
        this.available = 0;
        this.started = false;
        this.fadeGain = 0;
        this.fillAvg = -1;
        this.speed = 1;
        this.lastL = 0;
        this.lastR = 0;
        this.sincePush = 0;
        this.draining = false;
        this.underruns = 0;
        this.overruns = 0;
        this.resyncs = 0;
        this.playedFrames = 0;
        this.receivedFrames = 0;
        this.lastReportPlayed = 0;
        this.lastReportReceived = 0;
        this.report();
    }

    report() {
        this.port.postMessage({
            type: 'stats',
            availableFrames: Math.floor(Math.max(0, this.available)),
            playedFrames: this.playedFrames,
            receivedFrames: this.receivedFrames,
            underruns: this.underruns,
            overruns: this.overruns,
            resyncs: this.resyncs,
            started: this.started,
            sampleRate: this.streamRate,
            targetMs: this.targetFrames * 1000 / this.streamRate
        });
    }

    // Catmull-Rom interpolation of one interleaved frame at a fractional
    // stream-frame position. Requires at least ~4 frames of queue. Results
    // land in a preallocated buffer: no per-sample allocation on the audio
    // rendering thread.
    readCubicPos(pos, out) {
        const c = this.capacityFrames;
        const f = Math.floor(pos);
        const t = pos - f;
        for (let ch = 0; ch < this.channels; ch++) {
            const y0 = this.ring[(((f - 1) % c + c) % c) * this.channels + ch];
            const y1 = this.ring[(f % c) * this.channels + ch];
            const y2 = this.ring[((f + 1) % c) * this.channels + ch];
            const y3 = this.ring[((f + 2) % c) * this.channels + ch];
            out[ch] = y1 + 0.5 * t * (
                y2 - y0 +
                t * (2 * y0 - 5 * y1 + 4 * y2 - y3 +
                t * (3 * (y1 - y2) + y3 - y0))
            );
        }
        return out;
    }

    process(inputs, outputs) {
        const output = outputs[0];
        const left = output[0];
        const right = output.length > 1 ? output[1] : null;
        const frames = left.length;
        const step = this.updateSpeed(frames);

        // The stream went quiet: play out what is queued now.
        if (this.sincePush >= this.idleFrames && this.available >= 3) {
            if (!this.started) {
                this.started = true;
                this.report();
            }
            this.draining = true;
        }

        for (let i = 0; i < frames; i++) {
            // Underrun is detected per sample: as soon as the queue drops
            // to the low-water mark, playback fades out on the audio that is
            // still queued (30 ms is far more than the 8 ms fade), so the
            // stream never stops mid-waveform and never plays zeros at full
            // gain — both of which are loud clicks. While draining the end of
            // a sound, the queue is played down to the last sample instead.
            if (this.started && !this.draining && this.available < this.lowWaterFrames) {
                this.started = false;
                this.underruns++;
                this.report();
            } else if (this.started && this.draining && this.available < 3) {
                // End of the sound: fade out from its last sample.
                this.started = false;
                this.draining = false;
                this.readFrame = this.writeFrame;
                this.available = 0;
                this.report();
            }

            const consuming = this.started || this.fadeGain > 0;
            if (consuming && this.available >= 3) {
                const out = this.readCubicPos(this.readFrame, this.readOut);
                this.lastL = out[0];
                this.lastR = this.channels > 1 ? out[1] : out[0];
                this.readFrame += step;
                if (this.readFrame >= this.capacityFrames) {
                    this.readFrame -= this.capacityFrames;
                }
                this.available = Math.max(0, this.available - step);
                this.playedFrames++;
            } else if (this.available <= 0 && !this.started) {
                // The queue is completely empty. Park the read head exactly
                // at the write frontier so the next packet resumes at fresh
                // audio instead of straddling a gap of stale ring memory.
                this.readFrame = this.writeFrame;
                this.available = 0;
            }

            if (this.started) {
                this.fadeGain = Math.min(1, this.fadeGain + this.fadeStep);
            } else if (this.fadeGain > 0) {
                this.fadeGain = Math.max(0, this.fadeGain - this.fadeStep);
            }

            left[i] = this.lastL * this.fadeGain;
            if (right) {
                right[i] = this.lastR * this.fadeGain;
            }
            if (this.fadeGain === 0 && !this.started) {
                this.lastL = 0;
                this.lastR = 0;
            }
        }

        this.sincePush += frames;

        if (this.playedFrames - this.lastReportPlayed >= Math.floor(this.sampleRate / 10)) {
            this.lastReportPlayed = this.playedFrames;
            this.report();
        }

        return true;
    }
}

registerProcessor('soundshared-stream-processor', SoundSharedStreamProcessor);
`;

    // =========================================================================
    // Main-thread ring buffer (ScriptProcessor fallback only)
    //
    // Mirrors the worklet's semantics: stream-time capacity, a hard cap that
    // clears and rebuffers instead of hard-dropping, and a fractional read
    // head so a context rate other than the stream rate still resamples.
    // =========================================================================
    class RingBuffer {
        constructor(capacityFrames, channels, maxFrames, streamRate, contextRate, targetMs, catchupLimit) {
            this.channels = channels;
            this.capacityFrames = capacityFrames;
            this.capacity = capacityFrames * channels;
            this.maxFrames = maxFrames;
            this.streamRate = streamRate;
            this.rateRatio = contextRate > 0 ? streamRate / contextRate : 1;
            this.targetFrames = Math.floor(streamRate * ((targetMs == null ? 200 : targetMs) / 1000));
            this.catchupLimit = catchupLimit || 0.005;
            this.contextRate = contextRate > 0 ? contextRate : streamRate;
            // Drift control (mirrors the worklet's updateSpeed()).
            this.rebuildFrames = Math.floor(streamRate * 0.04);
            this.fillAvg = -1;
            this.speedFactor = 1;
            this.buffer = new Float32Array(this.capacity);
            this.writeFrame = 0;
            this.readFrame = 0;
            this.available = 0;
            this.overruns = 0;
            this.resyncs = 0;
            // Reused per-sample readout (no allocation in the render loop).
            this.takeOut = [0, 0];
        }

        setTargetMs(targetMs) {
            if (typeof targetMs === 'number' && targetMs > 30) {
                this.targetFrames = Math.max(
                    this.rebuildFrames * 2,
                    Math.min(Math.floor(this.maxFrames / 2), Math.floor(this.streamRate * targetMs / 1000))
                );
            }
        }

        push(samples) {
            let frames = Math.floor(samples.length / this.channels);
            if (frames <= 0) return;

            // Jumbo packet guard: a packet larger than the ring would wrap
            // the write past its own head. Drop the oldest part of it.
            if (frames > this.capacityFrames) {
                const skip = frames - this.capacityFrames;
                samples = samples.subarray(skip * this.channels);
                frames = this.capacityFrames;
                this.available = 0;
                this.readFrame = this.writeFrame;
                this.overruns++;
                return;
            }

            // Same hard-cap resync as the worklet: clear and rebuffer on
            // fresh audio instead of dropping one packet per push.
            if (this.available + frames > this.maxFrames) {
                this.available = 0;
                this.readFrame = this.writeFrame;
                this.resyncs++;
            }

            const first = Math.min(frames, this.capacityFrames - this.writeFrame);
            this.buffer.set(
                samples.subarray(0, first * this.channels),
                this.writeFrame * this.channels
            );
            if (first < frames) {
                this.buffer.set(
                    samples.subarray(first * this.channels, frames * this.channels),
                    0
                );
            }
            this.writeFrame = (this.writeFrame + frames) % this.capacityFrames;
            this.available += frames;
        }

        // Consumption speed for one ScriptProcessor block, mirroring the
        // worklet's updateSpeed(): smoothed level, proportional and capped
        // correction, slew-limited change. Called once per block.
        blockSpeed(blockFrames) {
            if (this.fillAvg < 0) this.fillAvg = this.available;
            const a = 1 - Math.exp(-blockFrames / (this.contextRate * 0.5));
            this.fillAvg += (this.available - this.fillAvg) * a;
            const error = (this.fillAvg - this.targetFrames) / Math.max(1, this.targetFrames);
            const want = 1 + Math.max(-this.catchupLimit, Math.min(this.catchupLimit, error * 0.01));
            // One 2048-frame block is ~16 worklet quanta: same slew per second.
            const slew = 0.00005 * Math.max(1, blockFrames / 128);
            this.speedFactor += Math.max(-slew, Math.min(slew, want - this.speedFactor));
            return this.rateRatio * this.speedFactor;
        }

        // Take one output frame (interleaved) via linear interpolation
        // relative to the stream rate. Returns null when the queue is empty.
        take(step) {
            if (this.available < 2) return null;
            const c = this.capacityFrames;
            const f = Math.floor(this.readFrame);
            const t = this.readFrame - f;
            for (let ch = 0; ch < this.channels; ch++) {
                const y1 = this.buffer[(f % c) * this.channels + ch];
                const y2 = this.buffer[((f + 1) % c) * this.channels + ch];
                this.takeOut[ch] = y1 + t * (y2 - y1);
            }
            this.readFrame += step;
            if (this.readFrame >= c) this.readFrame -= c;
            this.available = Math.max(0, this.available - step);
            return this.takeOut;
        }

        reset() {
            this.writeFrame = 0;
            this.readFrame = 0;
            this.available = 0;
            this.overruns = 0;
            this.resyncs = 0;
            this.fillAvg = -1;
            this.speedFactor = 1;
            this.buffer.fill(0);
        }
    }

    // =========================================================================
    // State
    // =========================================================================
    const state = {
        // Connection
        ws: null,
        wsUrl: '',
        reconnectAttempt: 0,
        reconnectTimer: null,
        isConnecting: false,
        shouldReconnect: false,

        // Audio graph
        audioContext: null,
        workletNode: null,
        analyserNode: null,
        gainNode: null,
        playbackMode: null, // 'worklet' | 'script' | null
        timeData: null,

        // Fallback ScriptProcessor
        scriptProcessor: null,
        scriptRing: null,
        scriptStarted: false,
        scriptFade: 0,
        scriptLastL: 0,
        scriptLastR: 0,
        // End-of-stream draining for the fallback (see the worklet).
        scriptLastPushAt: 0,
        scriptDraining: false,

        // Buffer / playback statistics
        bufferFrames: 0,
        bufferSampleRate: CONFIG.sampleRate,
        workletStarted: false,
        workletTargetMs: CONFIG.targetBufferMs,
        packetsReceived: 0,
        framesReceived: 0,
        framesPlayed: 0,
        underruns: 0,
        overruns: 0,
        resyncs: 0,
        staleDropped: 0,
        missedFrames: 0,

        // Arrival-clock sync (level 3). Anchored on the first packet of a
        // connection; `minResidualMs` is the lowest observed local-vs-server
        // offset (the network's lower bound), so `ageMs` estimates how far
        // behind the freshest arrival a packet is. Grossly stale packets from
        // a server backlog burst are dropped at ingress instead of flooding
        // the jitter queue and blowing past its hard cap.
        anchorLocal: null,
        anchorRemote: null,
        minResidualMs: null,

        // Wire codec and its WASM decoder ('pcm' keeps the legacy f32 path).
        streamCodec: 'opus',
        opusDecoder: null,
        opusDecoderPromise: null,
        // Decoding happens on microtasks, so packets ride a promise chain to
        // stay in arrival order.
        decodeChain: null,

        // Adaptive target (arrival jitter EMA drives the jitter-buffer target).
        lastPacketMs: null,
        jitterMs: 0,
        lastTargetEval: 0,

        // Sequence tracking
        lastSeqNo: null,
        lastPacketFrames: 0,
        lastPacketAt: 0,
        streamActive: false,

        // Meter peak hold
        peakLevel: 0,
        peakHoldUntil: 0,

        // UI loop
        animationFrame: null,
        lastUiUpdate: 0
    };

    // =========================================================================
    // DOM
    // =========================================================================
    const els = {};
    let bufferCells = [];

    function cacheElements() {
        els.volumeSlider = document.getElementById('volumeSlider');
        els.volumeValue = document.getElementById('volumeValue');
        els.statusIndicator = document.getElementById('statusIndicator');
        els.statusText = document.getElementById('statusText');

        els.connectBtn = document.getElementById('connectBtn');
        els.btnText = document.getElementById('btnText');
        els.btnIcon = document.getElementById('btnIcon');
        els.btnLoader = document.getElementById('btnLoader');

        els.audioIndicator = document.getElementById('audioIndicator');
        els.levelFill = document.getElementById('levelFill');
        els.levelPeak = document.getElementById('levelPeak');
        els.meterValue = document.getElementById('meterValue');

        els.bufferGrid = document.getElementById('bufferGrid');
        els.bufferText = document.getElementById('bufferText');
        bufferCells = els.bufferGrid ? Array.from(els.bufferGrid.children) : [];

        els.messagePanel = document.getElementById('messagePanel');
        els.messageIcon = document.getElementById('messageIcon');
        els.messageText = document.getElementById('messageText');
        els.dismissBtn = document.getElementById('dismissBtn');

        els.systemToast = document.getElementById('systemToast');
        els.systemToastIcon = document.getElementById('systemToastIcon');
        els.systemToastText = document.getElementById('systemToastText');

        els.wsUrl = document.getElementById('wsUrl');
        els.infoProtocol = document.getElementById('infoProtocol');
        els.infoBitrate = document.getElementById('infoBitrate');

        els.statsReceived = document.getElementById('statsReceived');
        els.statsPlayed = document.getElementById('statsPlayed');
        els.statsUnderruns = document.getElementById('statsUnderruns');
        els.statsOverruns = document.getElementById('statsOverruns');
        els.statsLost = document.getElementById('statsLost');
    }

    // =========================================================================
    // Logging / formatting
    // =========================================================================
    function log() {
        console.log('[SoundShared]', new Date().toISOString(), ...arguments);
    }

    function error() {
        console.error('[SoundShared]', new Date().toISOString(), ...arguments);
    }

    function warn() {
        console.warn('[SoundShared]', new Date().toISOString(), ...arguments);
    }

    // Switch the wire codec as announced by the server and prime the decoder
    // ahead of the first packet (loading the WASM module is ~100 ms).
    function setStreamCodec(codec) {
        if (state.streamCodec === codec) return;
        state.streamCodec = codec;
        state.opusDecoder = null;
        state.opusDecoderPromise = null;
        if (codec === 'opus') {
            ensureOpusDecoder();
        }
        log('Wire codec set to', codec);
    }

    function formatNumber(value) {
        return Number(value || 0).toLocaleString('en-US');
    }

    // =========================================================================
    // UI state helpers
    // =========================================================================
    function setStatus(status, text) {
        if (els.statusIndicator) {
            els.statusIndicator.className = 'status-dot ' + status;
        }
        if (els.statusText) {
            els.statusText.className = 'status-text ' + status;
            els.statusText.textContent = text;
        }
    }

    function setButtonState(mode) {
        if (!els.connectBtn) return;

        if (mode === 'connecting') {
            els.connectBtn.disabled = true;
            els.btnText.textContent = 'Linking...';
            els.btnIcon.hidden = true;
            els.btnLoader.hidden = false;
            return;
        }

        els.connectBtn.disabled = false;
        els.btnLoader.hidden = true;
        els.btnIcon.hidden = false;

        if (mode === 'connected') {
            els.btnText.textContent = 'Terminate Link';
            els.btnIcon.setAttribute('icon', 'lucide:power');
        } else {
            els.btnText.textContent = 'Establish Link';
            els.btnIcon.setAttribute('icon', 'lucide:zap');
        }
    }

    function setSystemToast(text, active, icon) {
        if (!els.systemToast) return;
        if (els.systemToastText) els.systemToastText.textContent = text;
        els.systemToast.classList.toggle('is-active', !!active);
        if (icon && els.systemToastIcon) {
            els.systemToastIcon.setAttribute('icon', icon);
        }
    }

    function showMessage(message, severity, showDismiss) {
        if (!els.messagePanel) return;

        const kind = severity || 'error';
        const icons = {
            error: 'lucide:alert-triangle',
            info: 'lucide:info',
            success: 'lucide:check-circle'
        };

        els.messagePanel.className = 'toast toast--' + kind;
        els.messagePanel.hidden = false;
        els.messageText.textContent = message;
        els.messageIcon.setAttribute('icon', icons[kind] || icons.info);
        els.dismissBtn.hidden = showDismiss === false;
    }

    function hideMessage() {
        if (els.messagePanel) els.messagePanel.hidden = true;
    }

    function updateWsUrlUI() {
        if (!els.wsUrl) return;
        // Never print the access key on the page.
        const url = (state.wsUrl || buildWsUrl()).split('?')[0];
        els.wsUrl.textContent = url;
        els.wsUrl.title = url;
    }

    function updateInfoUI(format) {
        if (!els.infoProtocol || !els.infoBitrate) return;

        const sampleRate = (format && format.sample_rate) || CONFIG.sampleRate;
        const channels = (format && format.channels) || CONFIG.channels;
        const codec = String((format && format.format) || 'opus');

        els.infoProtocol.textContent =
            codec.toUpperCase() + ' / ' + Math.round(sampleRate / 1000) + 'KHZ';
        // Opus is sent at the server's WIRE_BITRATE (64 kbit/s VBR); PCM would
        // be the raw budget.
        els.infoBitrate.textContent =
            codec === 'opus' ? '~64 KBPS VBR' :
            Math.round((sampleRate * channels * 32) / 1000) + ' KBPS';
    }

    function updateBufferUI() {
        if (!els.bufferText || !els.bufferGrid) return;

        const frames = Math.max(0, state.bufferFrames);
        const sampleRate = state.bufferSampleRate || CONFIG.sampleRate;
        const ms = (frames / sampleRate) * 1000;
        // The scale runs to the hard cap, not the target: at the healthy
        // 200 ms prebuffer the bar used to already look "maxed out", which
        // made real overruns impossible to see. The target is a marker here.
        const target = Math.round(state.workletTargetMs || CONFIG.targetBufferMs);
        const pct = Math.max(0, Math.min(1, ms / CONFIG.maxBufferMs));
        const activeCells = Math.round(pct * bufferCells.length);

        els.bufferText.textContent = Math.round(ms) + 'ms / ' + target + 'ms';

        for (let i = 0; i < bufferCells.length; i++) {
            const active = i < activeCells;
            bufferCells[i].classList.toggle('active', active);
            bufferCells[i].classList.toggle(
                'warn',
                active && ms > target * 1.5
            );
        }

        els.bufferGrid.classList.toggle(
            'is-low',
            state.streamActive && ms < CONFIG.lowWaterMs
        );
        els.bufferGrid.classList.toggle(
            'is-over',
            state.streamActive && ms > target * 1.5
        );
    }

    function updateStatsUI() {
        if (els.statsReceived) {
            els.statsReceived.textContent = formatNumber(state.packetsReceived);
        }
        if (els.statsPlayed) {
            els.statsPlayed.textContent = formatNumber(state.framesPlayed);
        }
        if (els.statsUnderruns) {
            els.statsUnderruns.textContent = formatNumber(state.underruns);
        }
        if (els.statsOverruns) {
            els.statsOverruns.textContent = formatNumber(
                state.overruns + state.staleDropped
            );
        }
        if (els.statsLost) {
            els.statsLost.textContent = formatNumber(state.missedFrames);
        }
    }

    function updateMeter() {
        if (!state.analyserNode || !state.timeData) return;

        state.analyserNode.getFloatTimeDomainData(state.timeData);

        let sum = 0;
        let peak = 0;
        for (let i = 0; i < state.timeData.length; i++) {
            const value = state.timeData[i];
            sum += value * value;
            const abs = Math.abs(value);
            if (abs > peak) peak = abs;
        }

        const rms = Math.sqrt(sum / state.timeData.length);
        const db = rms > 0 ? 20 * Math.log10(rms) : -Infinity;
        const level = db === -Infinity ? 0 : Math.max(0, Math.min(1, (db + 60) / 60));

        if (els.levelFill) {
            els.levelFill.style.width = (level * 100).toFixed(1) + '%';
        }
        if (els.meterValue) {
            els.meterValue.textContent = db === -Infinity ? '-INF dB' : db.toFixed(1) + ' dB';
        }
        if (els.audioIndicator) {
            els.audioIndicator.classList.toggle('is-clipping', level > 0.95);
        }

        const peakDb = peak > 0 ? 20 * Math.log10(peak) : -Infinity;
        const peakLevel =
            peakDb === -Infinity ? 0 : Math.max(0, Math.min(1, (peakDb + 60) / 60));

        // Peak-hold marker: jump to a new peak immediately, then hold it for a
        // moment before the slow decay in updateUI() takes over.
        const now = performance.now();
        if (peakLevel > state.peakLevel) {
            state.peakLevel = peakLevel;
            state.peakHoldUntil = now + 900;
        }

        if (els.levelPeak) {
            els.levelPeak.style.left =
                'calc(' + (state.peakLevel * 100).toFixed(1) + '% - 1px)';
        }
    }

    // =========================================================================
    // Audio graph
    // =========================================================================
    async function initAudio() {
        if (state.audioContext) {
            if (state.audioContext.state === 'suspended') {
                await state.audioContext.resume();
            }
            return;
        }

        const AudioContextCtor = window.AudioContext || window.webkitAudioContext;
        if (!AudioContextCtor) {
            throw new Error('Web Audio API is not supported in this browser');
        }

        let context;
        try {
            context = new AudioContextCtor({
                sampleRate: CONFIG.sampleRate,
                latencyHint: 'interactive'
            });
        } catch (e) {
            // Some browsers reject an explicit sample rate; use the default.
            context = new AudioContextCtor({ latencyHint: 'interactive' });
        }

        state.audioContext = context;
        if (context.sampleRate !== CONFIG.sampleRate) {
            // The stream is 48 kHz; the worklet's fractional reader resamples
            // it to the context rate on the fly, so this mismatch no longer
            // floods the jitter queue (it used to grow without bound here).
            log(
                'AudioContext sample rate is',
                context.sampleRate,
                'Hz; source stream is',
                CONFIG.sampleRate,
                'Hz (resampling in the worklet)'
            );
        }

        state.analyserNode = context.createAnalyser();
        state.analyserNode.fftSize = 1024;
        state.analyserNode.smoothingTimeConstant = 0.5;
        state.timeData = new Float32Array(state.analyserNode.fftSize);

        state.gainNode = context.createGain();
        state.gainNode.gain.value = volumeGain();

        try {
            const blob = new Blob([WORKLET_CODE], { type: 'application/javascript' });
            const blobUrl = URL.createObjectURL(blob);
            
            // Check if AudioWorklet is available before attempting to use it
            // This prevents TypeError: Cannot read properties of undefined (reading 'addModule')
            // when the page is not in a secure context
            if (context.audioWorklet) {
                await context.audioWorklet.addModule(blobUrl);
                URL.revokeObjectURL(blobUrl);

                const node = new AudioWorkletNode(context, 'soundshared-stream-processor', {
                    numberOfInputs: 0,
                    numberOfOutputs: 1,
                    outputChannelCount: [CONFIG.channels],
                    processorOptions: {
                        sampleRate: CONFIG.sampleRate,
                        streamRate: CONFIG.sampleRate,
                        channels: CONFIG.channels,
                        targetMs: CONFIG.targetBufferMs,
                        lowWaterMs: CONFIG.lowWaterMs,
                        maxMs: CONFIG.maxBufferMs,
                        capacitySeconds: CONFIG.capacityMs / 1000,
                        catchupLimit: CONFIG.catchupLimit
                    }
                });

                node.port.onmessage = onWorkletMessage;
                node.connect(state.analyserNode);
                state.analyserNode.connect(state.gainNode);
                state.gainNode.connect(context.destination);

                state.workletNode = node;
                state.playbackMode = 'worklet';
                log('AudioWorklet ready');
            } else {
                // AudioWorklet is not available in this context (usually insecure context)
                // Instead of throwing, gracefully fall back to ScriptProcessorNode
                URL.revokeObjectURL(blobUrl);
                setupScriptProcessor();
                log('AudioWorklet not available; using ScriptProcessor fallback (page is not a secure context)');
                return; // Exit initAudio successfully with fallback
            }
        } catch (workletError) {
            // The usual cause is an insecure context: AudioWorklet (like all
            // worklets) is a secure-context-only API, so plain http:// LAN
            // addresses fall back to the deprecated ScriptProcessor path.
            // This catch block handles actual errors from addModule() calls, not
            // the deliberate fallback when audioWorklet is not available.
            const message = workletError instanceof Error 
                ? workletError.message 
                : String(workletError);
                
            error(
                'AudioWorklet unavailable; using ScriptProcessor fallback:',
                workletError,
                !window.isSecureContext ? '(page is not a secure context)' : ''
            );
            setupScriptProcessor();
        }

        if (state.audioContext && state.audioContext.state === 'suspended') {
            await state.audioContext.resume();
        }
    }

    function setupScriptProcessor() {
        const context = state.audioContext;

        state.scriptRing = new RingBuffer(
            Math.ceil((CONFIG.sampleRate * CONFIG.capacityMs) / 1000),
            CONFIG.channels,
            Math.floor((CONFIG.sampleRate * CONFIG.maxBufferMs) / 1000),
            CONFIG.sampleRate,
            state.audioContext.sampleRate,
            CONFIG.targetBufferMs,
            CONFIG.catchupLimit
        );

        const processor = context.createScriptProcessor(
            2048,
            CONFIG.channels,
            CONFIG.channels
        );
        processor.onaudioprocess = onScriptProcess;

        processor.connect(state.analyserNode);
        state.analyserNode.connect(state.gainNode);
        state.gainNode.connect(context.destination);

        state.scriptProcessor = processor;
        state.playbackMode = 'script';
        log('ScriptProcessorNode fallback ready');
    }

    function onWorkletMessage(event) {
        const msg = event.data;
        if (!msg || msg.type !== 'stats') return;

        state.bufferFrames = msg.availableFrames;
        if (msg.sampleRate) state.bufferSampleRate = msg.sampleRate;
        state.framesPlayed = msg.playedFrames;
        state.underruns = msg.underruns;
        state.overruns = msg.overruns;
        state.resyncs = msg.resyncs || 0;
        state.workletStarted = msg.started;
        if (typeof msg.targetMs === 'number') {
            state.workletTargetMs = msg.targetMs;
        }

        updateBufferUI();
        updateStatsUI();
    }

    function onScriptProcess(event) {
        const ring = state.scriptRing;
        if (!ring) return;

        const outL = event.outputBuffer.getChannelData(0);
        const outR = event.outputBuffer.getChannelData(1);
        const frames = outL.length;

        // Queue levels are measured in stream time (the ring stores stream
        // frames), thresholds are in context frames, so convert on both sides.
        const lowFrames =
            Math.floor((CONFIG.sampleRate * CONFIG.lowWaterMs) / 1000);
        const fadeStep = 1 / Math.max(1, Math.floor(CONFIG.sampleRate * 0.008));
        // The adaptive target may move; feed it to the ring's drift control.
        ring.setTargetMs(state.workletTargetMs);

        if (!state.scriptStarted && ring.available >= ring.targetFrames) {
            state.scriptStarted = true;
        }
        // The stream went quiet: play out a short sound now instead of
        // holding it until the next one arrives (see the worklet).
        if (performance.now() - state.scriptLastPushAt > 120 && ring.available >= 2) {
            state.scriptStarted = true;
            state.scriptDraining = true;
        }
        const step = ring.blockSpeed(frames);

        for (let i = 0; i < frames; i++) {
            // Same per-sample underrun handling as the worklet: fade out on
            // the audio still queued instead of cutting to zeros.
            if (state.scriptStarted && !state.scriptDraining && ring.available < lowFrames) {
                state.scriptStarted = false;
                state.underruns++;
            } else if (state.scriptStarted && state.scriptDraining && ring.available < 2) {
                state.scriptStarted = false;
                state.scriptDraining = false;
                ring.readFrame = ring.writeFrame;
                ring.available = 0;
            }

            const consuming = state.scriptStarted || state.scriptFade > 0;
            if (consuming && ring.available >= 2) {
                const out = ring.take(step);
                state.scriptLastL = out[0];
                state.scriptLastR = out[1];
                state.framesPlayed++;
            } else if (ring.available <= 0 && !state.scriptStarted) {
                // Queue empty: park the read head at the write frontier so a
                // refill resumes at fresh audio (mirror of the worklet).
                ring.readFrame = ring.writeFrame;
                ring.available = 0;
            }

            if (state.scriptStarted) {
                state.scriptFade = Math.min(1, state.scriptFade + fadeStep);
            } else if (state.scriptFade > 0) {
                state.scriptFade = Math.max(0, state.scriptFade - fadeStep);
            }

            outL[i] = state.scriptLastL * state.scriptFade;
            outR[i] = state.scriptLastR * state.scriptFade;
            if (state.scriptFade === 0 && !state.scriptStarted) {
                state.scriptLastL = 0;
                state.scriptLastR = 0;
            }
        }

        state.bufferFrames = Math.floor(ring.available);
        state.bufferSampleRate = CONFIG.sampleRate;
        state.overruns = ring.overruns;
        state.resyncs = ring.resyncs;

        const now = performance.now();
        if (now - state.lastUiUpdate >= CONFIG.uiUpdateInterval) {
            state.lastUiUpdate = now;
            updateBufferUI();
            updateStatsUI();
        }
    }

    function pushSamples(samples) {
        if (!samples || samples.length < CONFIG.channels) return;

        if (state.playbackMode === 'worklet' && state.workletNode) {
            // Transfer ownership of the PCM buffer; no copy on the main thread.
            state.workletNode.port.postMessage(
                { type: 'push', samples: samples },
                [samples.buffer]
            );
        } else if (state.playbackMode === 'script' && state.scriptRing) {
            state.scriptRing.push(samples);
            state.scriptLastPushAt = performance.now();
            state.scriptDraining = false;
        }
    }

    // =========================================================================
    // Audio frame parsing
    //
    // Wire format: [seq_no u32 LE][server_ms u32 LE][payload]
    // The payload is one Opus packet (see broadcast.rs). `server_ms` counts
    // stream time on the server and drives both the staleness check and the
    // adaptive target below. Decoding happens through `opus-decoder` (WASM,
    // served by the server): `decodeFrame` is synchronous, and the result is
    // wrapped in `Promise.resolve()` so packets are pushed onto a promise chain
    // that preserves arrival order.
    // =========================================================================

    // Lazily create the WASM Opus decoder once per session. The library is a
    // UMD bundle served from /opus-decoder.min.js (script tag in index.html);
    // depending on the version it registers as `window['opus-decoder']`,
    // `window.OpusDecoder`, `window.opusDecoder` or similar. We try several
    // names so an incompatible build doesn't silently kill all audio.
    function ensureOpusDecoder() {
        if (state.opusDecoder) return Promise.resolve(state.opusDecoder);
        if (state.opusDecoderPromise) return state.opusDecoderPromise;

        // Try multiple possible global names the library might register under.
        const candidates = [
            window && window['opus-decoder'],
            window && window.OpusDecoder,
            window && window.opusDecoder,
            window && window['OpusDecoder'],
        ].filter(Boolean);

        const lib = candidates[0] || null;
        if (!lib) {
            const tried = candidates.map((c) => (c ? c.name || c.toString().slice(0, 40) : 'undefined')).join(', ');
            const err = new Error('Opus decoder script is not loaded');
            error('Opus decoder unavailable. Tried globals:', tried);
            return Promise.reject(err);
        }

        // The library might export the class directly or under an OpusDecoder property.
        const DecoderClass = lib.OpusDecoder || lib;

        try {
            const decoder = new DecoderClass({
                sampleRate: CONFIG.sampleRate,
                upgradeChannelCount: true,
                channels: CONFIG.channels,
            });
            const ready = decoder.ready;
            state.opusDecoderPromise = Promise.resolve(ready).then(() => {
                state.opusDecoder = decoder;
                log('Opus decoder ready (WASM)');
                return decoder;
            }).catch((e) => {
                error('Opus decoder failed to initialize:', e);
                state.opusDecoderPromise = null;
                throw e;
            });
        } catch (e) {
            error('Failed to create the Opus decoder:', e);
            return Promise.reject(e);
        }
        return state.opusDecoderPromise;
    }

    function handleBinary(arrayBuffer) {
        if (!arrayBuffer || arrayBuffer.byteLength < 12) return;

        const payloadBytes = arrayBuffer.byteLength - 8;
        if (payloadBytes <= 0) {
            log('Dropping malformed frame: missing payload');
            return;
        }

        const view = new DataView(arrayBuffer);
        const seq = view.getUint32(0, true);
        const serverMs = view.getUint32(4, true);
        const now = performance.now();
        // Opus arrives in fixed 10 ms frames (the server announces frame_size);
        // the exact count is known per packet after decoding.
        const framesMs = state.lastPacketFrames
            ? state.lastPacketFrames * 1000 / CONFIG.sampleRate
            : 10;

        // ----- Arrival-clock sync (level 3) -----
        // Anchor every connection on its first packet and keep the lowest
        // local-vs-server offset ever observed: that is the network's floor,
        // so `ageMs` approximates how stale a packet is relative to the
        // freshest arrival without any wall-clock handshake. A long gap in
        // packets invalidates the anchor (the server pauses publishing when
        // nothing plays, and its stream clock freezes), so re-anchor instead
        // of letting every future packet look ancient.
        if (
            state.anchorLocal === null ||
            (state.lastPacketMs !== null && now - state.lastPacketMs > 2000)
        ) {
            state.anchorLocal = now;
            state.anchorRemote = serverMs;
            state.minResidualMs = null;
        }

        const residual = now - (state.anchorLocal + (serverMs - state.anchorRemote));
        if (state.minResidualMs === null || residual < state.minResidualMs) {
            state.minResidualMs = residual;
        }
        const ageMs = residual - state.minResidualMs;

        // ----- Adaptive target (arrival jitter tracking) -----
        if (state.lastPacketMs !== null) {
            const delta = now - state.lastPacketMs;
            if (delta < 2000) {
                const expected = framesMs || 10;
                state.jitterMs = state.jitterMs * 0.9 +
                    Math.abs(delta - expected) * 0.1;
            } else {
                // A long pause is not jitter; let the estimate relax.
                state.jitterMs *= 0.9;
            }
        }

        const targetMs = Math.max(
            CONFIG.targetBufferMs,
            Math.min(400, Math.round(CONFIG.targetBufferMs + state.jitterMs * 1.5))
        );
        if (now - state.lastTargetEval >= 1000) {
            state.lastTargetEval = now;
            if (targetMs !== state.workletTargetMs) {
                state.workletTargetMs = targetMs;
                if (state.workletNode) {
                    state.workletNode.port.postMessage(
                        { type: 'target', targetMs: targetMs }
                    );
                }
            }
        }

        // Grossly stale backlog (a burst queued server-side) is dropped at
        // ingress: feeding it to the jitter queue would blow straight past
        // the hard cap and force a panic resync for audio nobody wants.
        if (ageMs > CONFIG.staleMs) {
            state.staleDropped++;
            state.lastSeqNo = seq;
            state.lastPacketMs = now;
            return;
        }

        const payload = new Uint8Array(arrayBuffer, 8, payloadBytes);

        if (state.streamCodec === 'opus') {            // Push onto the decode chain: order of arrival is preserved.
            // decodeFrame is synchronous but its result is wrapped in a Promise,
            // so the chain preserves arrival order even under load. The chain is
            // never reset mid-session вЂ” each link is tiny, and clearing the tail
            // could break packet ordering under load.
            let runDecode = state.decodeChain
                ? state.decodeChain.then(() => decodeOpusPayload(payload))
                : decodeOpusPayload(payload);
            state.decodeChain = runDecode
                .then((samples) => processAudioFrame(samples, seq, now))
                .catch((e) => {
                    error('Opus decode pipeline error, resetting chain:', e);
                    state.decodeChain = null;
                });
        } else if (payloadBytes % 4 === 0) {
            // Legacy f32 PCM path (no longer produced by the server; kept so
            // mixed-version clients keep playing while testing).
            processAudioFrame(new Float32Array(arrayBuffer, 8), seq, now);
        } else {
            log('Dropping malformed frame: codec unknown', state.streamCodec);
            state.lastSeqNo = seq;
            state.lastPacketMs = now;
        }
    }

    // Decode one Opus packet through the WASM decoder (loading it on first
    // use). `opus-decoder.decodeFrame` is synchronous вЂ” it returns
    // `{errors, channelData, samplesDecoded, sampleRate, bitDepth}` directly,
    // not a Promise. `channelData` is per-channel Float32Arrays (e.g.
    // [left, right]); we interleave them into one Float32Array for the ring
    // buffer and ScriptProcessorNode. We wrap the result in `Promise.resolve()`
    // so the caller's `.then()` chain keeps working. Returns decoded interleaved
    // f32 samples, or an empty buffer on failure so the pipeline keeps moving.
    //
    // The opus-decoder WASM library has shipped multiple incompatible
    // decodeFrame return shapes across versions:
    //   вЂў old:   just a number (samplesDecoded) вЂ” no PCM extractable
    //   вЂў mid:   { channelData: Float32Array[], samplesDecoded, sampleRate }
    //   вЂў new:   { errors, channelData, samplesDecoded, sampleRate, bitDepth }
    //   вЂў alt:   { outputBuffer: Float32Array, ... }  (interleaved)
    // We handle every shape we can recognise and log the rest so an
    // incompatible library version is visible in the console instead of
    // producing silent silence.
    // Extract interleaved f32 PCM from a decodeFrame result object, or empty
    // buffer on failure. Also logs the result shape once so an incompatible
    // opus-decoder version is visible in the console.
    function extractOpusPcm(result) {
        // Old API: decodeFrame returns just a sample count вЂ” no PCM extractable.
        if (typeof result === 'number') {
            if (!state._opusWarnedOldApi) {
                state._opusWarnedOldApi = true;
                warn('opus-decoder: decodeFrame returned a number (old API), no PCM extractable');
            }
            return new Float32Array(0);
        }

        if (!result || typeof result !== 'object') {
            if (!state._opusWarnedNoResult) {
                state._opusWarnedNoResult = true;
                warn('opus-decoder: decodeFrame returned no usable result:', result);
            }
            return new Float32Array(0);
        }

        // Newer API surfaces errors explicitly. Check if errors array has content.
        if (Array.isArray(result.errors) && result.errors.length > 0) {
            if (!state._opusWarnedErrors) {
                state._opusWarnedErrors = true;
                warn('opus-decoder: decodeFrame reported errors:', result.errors);
            }
            return new Float32Array(0);
        }

        // Prefer the interleaved outputBuffer if the library provides it.
        if (result.outputBuffer instanceof Float32Array && result.outputBuffer.length > 0) {
            return result.outputBuffer;
        }

        // Otherwise interleave per-channel channelData into one buffer.
        if (Array.isArray(result.channelData) && result.channelData.length >= CONFIG.channels) {
            const channels = result.channelData;
            const frames = channels[0] instanceof Float32Array ? channels[0].length : 0;
            if (frames > 0) {
                const interleaved = new Float32Array(frames * CONFIG.channels);
                for (let f = 0; f < frames; f++) {
                    for (let c = 0; c < CONFIG.channels; c++) {
                        interleaved[f * CONFIG.channels + c] = channels[c][f];
                    }
                }
                return interleaved;
            }
        }

        // Some versions expose a single interleaved buffer under 'data'.
        if (result.data instanceof Float32Array && result.data.length > 0) {
            return result.data;
        }

        if (!state._opusWarnedUnknownShape) {
            state._opusWarnedUnknownShape = true;
            const keys = Object.keys(result);
            warn('opus-decoder: unknown decodeFrame result shape, keys:', keys);
        }
        return new Float32Array(0);
    }

    function decodeOpusPayload(payload) {
        const decoder = state.opusDecoder;
        const run = (d) => {
            let result;
            try {
                const raw = d.decodeFrame(payload); // synchronous in most versions
                if (raw && typeof raw.then === 'function') {
                    // Newer async API: decodeFrame returns a Promise.
                    return raw.then(extractOpusPcm)
                        .catch((e) => {
                            error('Opus decodeFrame (async) rejected:', e);
                            return new Float32Array(0);
                        });
                }
                result = raw;
            } catch (e) {
                error('Opus decodeFrame threw:', e);
                return Promise.resolve(new Float32Array(0));
            }

            return Promise.resolve(extractOpusPcm(result));
        };
        if (decoder) {
            return run(decoder);
        }
        return ensureOpusDecoder().then((d) => run(d));
    }

    // The shared tail of every frame: sequence-gap silence insertion and
    // buffering. Called with decoded (interleaved f32) samples.
    function processAudioFrame(samples, seq, now) {
        const frames = samples.length / CONFIG.channels;

        // Sequence gap detection.
        //
        // Opus sends every 10 ms frame, so a gap means a packet was lost on
        // the wire. Inserting hard silence here creates a click at every
        // loss вЂ” instead, track the missed frames and let the worklet's
        // underrun fade handle the gap silently. The legacy PCM path (no
        // longer produced by the server) keeps silence insertion so
        // mixed-version clients don't lose timeline alignment.
        if (state.lastSeqNo !== null) {
            const gap = (seq - state.lastSeqNo) >>> 0;

            if (gap === 0) {
                return; // duplicate packet
            }

            if (gap > 1) {
                const missed = gap - 1;
                state.missedFrames += missed;
                if (state.streamCodec !== 'opus') {
                    // Legacy PCM: fill the gap with silence so the playback
                    // timeline stays aligned. Opus does not do this вЂ” hard
                    // silence on packet loss is exactly the "РїРµСЂРґС‘Р¶" click.
                    if (missed <= 50 && state.lastPacketFrames > 0) {
                        const silenceSamples =
                            missed * state.lastPacketFrames * CONFIG.channels;
                        pushSamples(new Float32Array(silenceSamples));
                    } else {
                        log('Large sequence gap; skipping silence insertion:', missed);
                    }
                } else {
                    log('Opus sequence gap, skipping silence (worklet fade covers it):', missed);
                }
            }
        }

        state.lastSeqNo = seq;
        state.lastPacketFrames = frames;
        state.packetsReceived++;
        state.framesReceived += frames;
        state.lastPacketAt = now;
        state.lastPacketMs = now;
        state.streamActive = true;

        pushSamples(samples);

        if (state.packetsReceived === 1) {
            setSystemToast(
                'Stream active. ' +
                (state.streamCodec === 'opus' ? 'Decoding Opus.' : 'PCM 48 kHz.'),
                true, 'lucide:radio'
            );
            hideMessage();
        }

        updateStatsUI();
    }

    function handleControlMessage(msg) {
        if (!msg || typeof msg.type !== 'string') return;

        switch (msg.type) {
            case 'format':
                updateInfoUI(msg.data || {});
                setStreamCodec(
                    msg.data && msg.data.format === 'pcm' ? 'pcm' : 'opus'
                );
                log('Server format:', msg.data);
                break;

            case 'welcome':
                log('Welcome from server:', msg.data);
                break;

            case 'error':
                showMessage(msg.message || 'Server error', 'error');
                break;

            case 'info':
                showMessage(msg.message || 'Info from server', 'info', false);
                break;

            default:
                log('Unknown control message:', msg.type);
        }
    }

    // =========================================================================
    // WebSocket
    // =========================================================================
    // Access key from the shared link (`/?k=...`). The GM's console puts it
    // into every link it copies; the audio stream refuses connections
    // without it.
    const ACCESS_KEY = (function () {
        try {
            return new URLSearchParams(location.search).get('k') || '';
        } catch (e) {
            return '';
        }
    })();

    function keyQuery() {
        return ACCESS_KEY ? '?k=' + encodeURIComponent(ACCESS_KEY) : '';
    }

    function buildWsUrl() {
        const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
        const host = location.host || 'localhost:8080';
        return protocol + '//' + host + '/ws/audio' + keyQuery();
    }

    // The link has no valid key (missing, mistyped, or replaced by the GM):
    // say so and stop, instead of retrying forever.
    function accessDenied() {
        state.shouldReconnect = false;
        state.isConnecting = false;
        if (state.reconnectTimer) {
            clearTimeout(state.reconnectTimer);
            state.reconnectTimer = null;
        }
        setStatus('error', 'Access Denied');
        setButtonState('idle');
        setSystemToast('Access key rejected.', false, 'lucide:alert-triangle');
        showMessage(
            ACCESS_KEY
                ? 'This link is no longer valid: the GM issued a new access key. Ask for the current link.'
                : 'This link has no access key. Open the full link from the GM (it ends with ?k=...).',
            'error',
            true
        );
    }

    function connect() {
        if (
            state.ws &&
            (state.ws.readyState === WebSocket.OPEN ||
                state.ws.readyState === WebSocket.CONNECTING)
        ) {
            return;
        }

        state.wsUrl = buildWsUrl();
        updateWsUrlUI();
        state.shouldReconnect = true;
        state.reconnectAttempt = 0;
        doConnect();
    }

    function doConnect() {
        if (!state.shouldReconnect) return;

        state.isConnecting = true;
        setStatus('connecting', 'Linking...');
        setButtonState('connecting');
        setSystemToast('Negotiating WebSocket handshake...', false, 'lucide:radio-tower');
        hideMessage();

        log('Connecting to', state.wsUrl.split('?')[0]);

        // A browser WebSocket cannot see the HTTP status of a refused
        // upgrade, so the key is checked against the status API first: a 401
        // there means the link is wrong, not that the server is down.
        fetch('/api/status' + keyQuery(), { cache: 'no-store' })
            .then((res) => {
                if (res.status === 401) {
                    accessDenied();
                    return;
                }
                openSocket();
            })
            .catch(() => openSocket());
    }

    function openSocket() {
        if (!state.shouldReconnect) return;
        try {
            const ws = new WebSocket(state.wsUrl);
            ws.binaryType = 'arraybuffer';
            ws.onopen = onWsOpen;
            ws.onmessage = onWsMessage;
            ws.onclose = onWsClose;
            ws.onerror = onWsError;
            state.ws = ws;
        } catch (e) {
            handleConnectionError(e);
        }
    }

    function onWsOpen() {
        log('WebSocket connected');
        state.isConnecting = false;
        state.reconnectAttempt = 0;

        setStatus('connected', 'Node Online');
        setButtonState('connected');
        setSystemToast('Link established. Waiting for audio frames.', true, 'lucide:radio');

        resetAudioPipeline();
    }

    function onWsMessage(event) {
        if (typeof event.data === 'string') {
            try {
                handleControlMessage(JSON.parse(event.data));
            } catch (e) {
                log('Text message from server:', event.data);
            }
            return;
        }

        if (event.data instanceof ArrayBuffer) {
            handleBinary(event.data);
        }
    }

    function onWsClose(event) {
        log('WebSocket closed:', event.code, event.reason);
        state.isConnecting = false;
        state.ws = null;
        state.streamActive = false;

        resetAudioPipeline();
        updateBufferUI();
        updateStatsUI();

        if (event.code === 4001) {
            // The GM issued a new access key; this link is revoked.
            accessDenied();
            return;
        }

        if (state.shouldReconnect) {
            if (event.code === 1000) {
                setStatus('disconnected', 'Node Offline');
                setButtonState('idle');
                setSystemToast('Link closed. Standing by.', false, 'lucide:check-circle');
                showMessage('Disconnected from server.', 'info', false);
            } else {
                setStatus('disconnected', 'Node Offline');
                setButtonState('idle');
                scheduleReconnect(event.reason || 'Code ' + event.code);
            }
        } else {
            setStatus('disconnected', 'Node Offline');
            setButtonState('idle');
            setSystemToast('System ready. Initializing components.', false, 'lucide:check-circle');
        }
    }

    function onWsError(event) {
        error('WebSocket error:', event);
        // `onclose` follows and drives the reconnect/state handling.
    }

    function handleConnectionError(err) {
        state.isConnecting = false;
        state.shouldReconnect = false;
        setStatus('error', 'Link Error');
        setButtonState('idle');
        setSystemToast('Connection failed. Check the server.', false, 'lucide:alert-triangle');
        showMessage(
            'Failed to connect: ' + (err && err.message ? err.message : err),
            'error'
        );
    }

    function scheduleReconnect(reason) {
        if (!state.shouldReconnect) return;

        state.reconnectAttempt++;
        const { initialDelay, maxDelay, factor, jitter, maxAttempts } = CONFIG.reconnect;

        if (state.reconnectAttempt > maxAttempts) {
            setStatus('error', 'Link Error');
            setSystemToast('Reconnection attempts exhausted.', false, 'lucide:alert-triangle');
            showMessage('Max reconnection attempts reached. Refresh the page.', 'error');
            return;
        }

        const base = Math.min(
            initialDelay * Math.pow(factor, state.reconnectAttempt - 1),
            maxDelay
        );
        const delay = Math.floor(base + base * jitter * Math.random());

        log(
            'Reconnect attempt',
            state.reconnectAttempt,
            'in',
            delay + 'ms',
            '(' + reason + ')'
        );

        showMessage(
            'Reconnecting in ' + (delay / 1000).toFixed(1) + 's...',
            'info',
            false
        );
        setSystemToast(
            'Reconnecting in ' + (delay / 1000).toFixed(1) + 's...',
            false,
            'lucide:radio-tower'
        );

        if (state.reconnectTimer) clearTimeout(state.reconnectTimer);
        state.reconnectTimer = setTimeout(function () {
            if (state.shouldReconnect) doConnect();
        }, delay);
    }

    function disconnect() {
        log('Manual disconnect');
        state.shouldReconnect = false;

        if (state.reconnectTimer) {
            clearTimeout(state.reconnectTimer);
            state.reconnectTimer = null;
        }

        if (state.ws) {
            try {
                state.ws.close(1000, 'User disconnected');
            } catch (e) {
                // Ignore close errors.
            }
            state.ws = null;
        }

        if (state.audioContext && state.audioContext.state === 'running') {
            state.audioContext.suspend().catch(function () {});
        }

        state.isConnecting = false;
        state.streamActive = false;
        resetAudioPipeline();
        updateBufferUI();
        updateStatsUI();

        setStatus('disconnected', 'Node Offline');
        setButtonState('idle');
        setSystemToast('System ready. Initializing components.', false, 'lucide:check-circle');
        hideMessage();
    }

    function resetAudioPipeline() {
        state.bufferFrames = 0;
        state.workletStarted = false;
        state.workletTargetMs = CONFIG.targetBufferMs;
        state.packetsReceived = 0;
        state.framesReceived = 0;
        state.framesPlayed = 0;
        state.underruns = 0;
        state.overruns = 0;
        state.resyncs = 0;
        state.staleDropped = 0;
        state.missedFrames = 0;
        state.lastSeqNo = null;
        state.lastPacketFrames = 0;

        // Re-anchor the arrival clock and the adaptive target: they are only
        // meaningful for one continuous connection.
        state.anchorLocal = null;
        state.anchorRemote = null;
        state.minResidualMs = null;
        state.lastPacketMs = null;
        state.jitterMs = 0;
        state.lastTargetEval = 0;

        state.scriptStarted = false;
        state.scriptFade = 0;
        state.peakLevel = 0;
        state.peakHoldUntil = 0;

        if (state.workletNode) {
            state.workletNode.port.postMessage({ type: 'reset' });
        }
        if (state.scriptRing) {
            state.scriptRing.reset();
        }

        if (els.levelFill) els.levelFill.style.width = '0%';
        if (els.levelPeak) els.levelPeak.style.left = '0%';
        if (els.meterValue) els.meterValue.textContent = '-INF dB';
        if (els.audioIndicator) els.audioIndicator.classList.remove('is-clipping');
    }

    // =========================================================================
    // UI loop
    // =========================================================================
    function updateUI(now) {
        const t = typeof now === 'number' ? now : performance.now();

        updateMeter();

        if (state.streamActive && t - state.lastPacketAt > 1000) {
            state.streamActive = false;
            updateBufferUI();
        }

        if (t > state.peakHoldUntil && state.peakLevel > 0) {
            state.peakLevel = Math.max(0, state.peakLevel - 0.01);
            if (els.levelPeak) {
                els.levelPeak.style.left =
                    'calc(' + (state.peakLevel * 100).toFixed(1) + '% - 1px)';
            }
        }

        state.animationFrame = requestAnimationFrame(updateUI);
    }

    // =========================================================================
    // Events
    // =========================================================================
    async function onConnectClick() {
        if (state.isConnecting) return;

        if (state.ws && state.ws.readyState === WebSocket.OPEN) {
            disconnect();
            return;
        }

        try {
            await initAudio();

            if (state.audioContext.state === 'suspended') {
                await state.audioContext.resume();
            }

            connect();
        } catch (e) {
            error('Failed to initialize audio:', e);
            setStatus('error', 'Audio Error');
            setButtonState('idle');
            setSystemToast('Audio initialization failed.', false, 'lucide:alert-triangle');
            showMessage(
                'Failed to initialize audio: ' +
                    (e && e.message ? e.message : e) +
                    '. Check browser audio permissions.',
                'error'
            );
        }
    }

    function onVisibilityChange() {
        // Keep the stream running while hidden. The worklet continues to
        // consume audio; the ring buffer caps latency, so there is no need to
        // drop packets or clear the queue when the tab becomes visible again.
        if (
            !document.hidden &&
            state.audioContext &&
            state.audioContext.state === 'suspended' &&
            state.ws &&
            state.ws.readyState === WebSocket.OPEN
        ) {
            state.audioContext.resume().catch(function () {});
        }
    }

    function onBeforeUnload() {
        disconnect();
    }

    // =========================================================================
    // Init
    // =========================================================================
    // =========================================================================
    // Per-listener volume
    //
    // Only this device: the GM's mix and every other player are unaffected.
    // Remembered in localStorage. The slider is perceptual (squared), so the
    // lower half is usable instead of everything below 50 % sounding loud.
    // =========================================================================
    const VOLUME_KEY = 'bardwave.volume';
    const volume = { percent: 100, muted: false };

    function loadVolume() {
        try {
            const saved = JSON.parse(localStorage.getItem(VOLUME_KEY) || 'null');
            if (saved && typeof saved.percent === 'number') {
                volume.percent = Math.max(0, Math.min(100, Math.round(saved.percent)));
                volume.muted = !!saved.muted;
            }
        } catch (e) {
            // Private mode / blocked storage: just start at 100 %.
        }
    }

    function saveVolume() {
        try {
            localStorage.setItem(VOLUME_KEY, JSON.stringify(volume));
        } catch (e) {
            // Not fatal: the volume simply is not remembered.
        }
    }

    function volumeGain() {
        if (volume.muted) return 0;
        const v = volume.percent / 100;
        return v * v;
    }

    function applyVolume() {
        if (els.volumeSlider) {
            els.volumeSlider.value = String(volume.percent);
            els.volumeSlider.style.setProperty('--fill', volume.percent + '%');
        }
        if (els.volumeValue) {
            els.volumeValue.textContent = volume.muted ? 'MUTED' : volume.percent + '%';
            els.volumeValue.classList.toggle('is-muted', volume.muted);
        }
        if (state.gainNode && state.audioContext) {
            // A short ramp instead of a jump: no zipper noise while dragging.
            state.gainNode.gain.setTargetAtTime(
                volumeGain(),
                state.audioContext.currentTime,
                0.015
            );
        }
    }

    function initVolume() {
        loadVolume();
        applyVolume();
        if (els.volumeSlider) {
            els.volumeSlider.addEventListener('input', () => {
                volume.percent = Number(els.volumeSlider.value) || 0;
                volume.muted = false;
                applyVolume();
            });
            els.volumeSlider.addEventListener('change', saveVolume);
        }
        if (els.volumeValue) {
            els.volumeValue.addEventListener('click', () => {
                volume.muted = !volume.muted;
                applyVolume();
                saveVolume();
            });
        }
    }

    function init() {
        cacheElements();
        initVolume();

        updateWsUrlUI();
        updateInfoUI({
            sample_rate: CONFIG.sampleRate,
            channels: CONFIG.channels,
            format: 'f32'
        });

        setStatus('disconnected', 'Node Offline');
        setButtonState('idle');
        updateBufferUI();
        updateStatsUI();

        els.connectBtn.addEventListener('click', onConnectClick);
        els.dismissBtn.addEventListener('click', hideMessage);
        document.addEventListener('visibilitychange', onVisibilityChange);
        window.addEventListener('beforeunload', onBeforeUnload);

        state.animationFrame = requestAnimationFrame(updateUI);

        log('Bardwave Listener initialized');
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }

    // Expose for debugging.
    window.SoundShared = {
        getState: function () {
            return Object.assign({}, state);
        },
        connect: connect,
        disconnect: disconnect
    };
})();
