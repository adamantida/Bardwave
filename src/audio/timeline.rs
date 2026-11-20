//! Session timers: "in ten minutes switch to the Alarm scene", "every half hour
//! ring the bell".
//!
//! Three layers, each usable alone:
//!
//! * [`Timeline`] is the pure core. It owns no thread and never reads a clock:
//!   the caller passes a monotonic `now_ms` to [`Timeline::tick`] and gets the
//!   [`TimerEvent`]s that came due. That makes it fully deterministic to test.
//! * [`TimelineRunner`] wraps it in a thread that ticks every
//!   [`TICK_MS`] milliseconds, publishes a [`TimelineSnapshot`] for the GUI and
//!   sends the events over a channel.
//! * `Director::run_timer_action` (see `director_timeline.rs`) turns an event's
//!   action into existing Director commands. The Director lives on the GUI
//!   thread (it owns the audio engine), so the runner never touches it: the
//!   GUI drains the event channel once per frame and runs the actions itself.
//!
//! # Time model
//!
//! The timeline has its own clock, [`Timeline::clock_ms`], that advances only
//! while the status is [`TimelineStatus::Running`]. A pause therefore stops
//! every countdown without accumulating anything, and a resume continues from
//! the same remaining times. Timers always start stopped: the definitions are
//! saved in `[[timers]]`, the running state is not.
//!
//! # Firing
//!
//! * `after_ms`: fires once, `after_ms` after Start, then stays done.
//! * `every_ms`: fires every `every_ms` after Start. When one tick finds
//!   several periods overdue (the process was suspended), it fires once and
//!   reports the skipped periods in [`TimerEvent::missed`] instead of
//!   replaying a burst of sounds.
//! * A disabled timer, or one whose definition is invalid
//!   ([`TimerConfig::problem`]), is never armed.

use crate::config::{TimerAction, TimerConfig};
use crossbeam_channel::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Step of the runner thread.
pub const TICK_MS: u64 = 100;

/// One firing of a timer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerEvent {
    /// Id of the timer that fired.
    pub id: String,
    /// Its display name (for log lines).
    pub name: String,
    /// What it asks for.
    pub action: TimerAction,
    /// How many times this timer has fired since Start, this one included.
    pub run: u32,
    /// Periods of a repeating timer that were skipped because the tick came
    /// late (0 in normal operation).
    pub missed: u32,
    /// Timeline clock when it fired.
    pub at_ms: u64,
}

/// Whether the timers are counting down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimelineStatus {
    /// Nothing armed; the state after a reset and at startup.
    #[default]
    Stopped,
    /// Countdowns are running.
    Running,
    /// Countdowns are frozen at their remaining times.
    Paused,
}

/// Where one timer stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerPhase {
    /// Enabled, but the timeline is stopped.
    Idle,
    /// Counting down.
    Armed,
    /// A one-shot timer that has fired.
    Done,
    /// Switched off by the operator.
    Disabled,
    /// The definition cannot run (see [`TimerConfig::problem`]).
    Invalid,
}

/// A timer as the GUI shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimerView {
    /// Timer id.
    pub id: String,
    /// Phase.
    pub phase: TimerPhase,
    /// Time until it fires, while armed.
    pub remaining_ms: Option<u64>,
    /// Firings since Start.
    pub runs: u32,
}

/// Everything the GUI needs to draw the timers, in one clone.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TimelineSnapshot {
    /// Overall status.
    pub status: TimelineStatus,
    /// Running time since Start (pauses excluded).
    pub clock_ms: u64,
    /// One entry per timer, in definition order.
    pub timers: Vec<TimerView>,
    /// Index into `timers` of the armed timer that fires first.
    pub next: Option<usize>,
}

/// A request to the timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineControl {
    /// Replace the definitions; unchanged timers keep their countdowns.
    SetTimers(Vec<TimerConfig>),
    /// Arm every timer (from stopped) or resume (from paused).
    Start,
    /// Freeze all countdowns.
    Pause,
    /// Disarm everything and go back to stopped.
    Reset,
}

#[derive(Debug, Clone)]
struct Slot {
    cfg: TimerConfig,
    /// Clock value at which it fires next.
    due: Option<u64>,
    runs: u32,
    done: bool,
}

impl Slot {
    fn new(cfg: TimerConfig) -> Self {
        Self {
            cfg,
            due: None,
            runs: 0,
            done: false,
        }
    }

    fn runnable(&self) -> bool {
        self.cfg.enabled && self.cfg.problem().is_none()
    }

    /// Start counting from `clock`.
    fn arm(&mut self, clock: u64) {
        self.runs = 0;
        self.done = false;
        self.due = self.runnable().then(|| clock + self.cfg.trigger.ms());
    }

    fn disarm(&mut self) {
        self.due = None;
        self.runs = 0;
        self.done = false;
    }
}

/// The pure timer core. See the module docs.
#[derive(Debug, Clone, Default)]
pub struct Timeline {
    slots: Vec<Slot>,
    status: TimelineStatus,
    clock_ms: u64,
    last_now: Option<u64>,
}

impl Timeline {
    /// A stopped timeline with these definitions.
    pub fn new(timers: Vec<TimerConfig>) -> Self {
        Self {
            slots: timers.into_iter().map(Slot::new).collect(),
            ..Self::default()
        }
    }

    /// Overall status.
    pub fn status(&self) -> TimelineStatus {
        self.status
    }

    /// Running time since Start, pauses excluded.
    pub fn clock_ms(&self) -> u64 {
        self.clock_ms
    }

    /// Replace the definitions.
    ///
    /// A timer with the same id and the same trigger keeps its countdown (and
    /// its done state); a changed or new one starts counting from now when the
    /// timeline is running or paused; switching one off disarms it.
    pub fn set_timers(&mut self, timers: Vec<TimerConfig>) {
        let live = self.status != TimelineStatus::Stopped;
        let clock = self.clock_ms;
        let mut old = std::mem::take(&mut self.slots);
        self.slots = timers
            .into_iter()
            .map(|cfg| {
                let kept = old
                    .iter()
                    .position(|slot| slot.cfg.id == cfg.id)
                    .map(|index| old.swap_remove(index))
                    .filter(|slot| {
                        slot.cfg.trigger == cfg.trigger
                            && slot.cfg.enabled == cfg.enabled
                            && slot.cfg.problem().is_none() == cfg.problem().is_none()
                    });
                match kept {
                    Some(mut slot) => {
                        slot.cfg = cfg;
                        slot
                    }
                    None => {
                        let mut slot = Slot::new(cfg);
                        if live {
                            slot.arm(clock);
                        }
                        slot
                    }
                }
            })
            .collect();
    }

    /// Arm every timer from zero (stopped) or continue the countdowns
    /// (paused). Does nothing while running.
    pub fn start(&mut self, now_ms: u64) {
        match self.status {
            TimelineStatus::Running => return,
            TimelineStatus::Stopped => {
                self.clock_ms = 0;
                let clock = self.clock_ms;
                for slot in &mut self.slots {
                    slot.arm(clock);
                }
            }
            TimelineStatus::Paused => {}
        }
        self.status = TimelineStatus::Running;
        self.last_now = Some(now_ms);
    }

    /// Freeze the countdowns. Timers that came due up to `now_ms` still fire
    /// and are returned.
    pub fn pause(&mut self, now_ms: u64) -> Vec<TimerEvent> {
        let events = self.tick(now_ms);
        if self.status == TimelineStatus::Running {
            self.status = TimelineStatus::Paused;
        }
        events
    }

    /// Disarm everything and go back to stopped.
    pub fn reset(&mut self) {
        self.status = TimelineStatus::Stopped;
        self.clock_ms = 0;
        self.last_now = None;
        for slot in &mut self.slots {
            slot.disarm();
        }
    }

    /// Run a [`TimelineControl`] at `now_ms`.
    pub fn apply(&mut self, control: TimelineControl, now_ms: u64) -> Vec<TimerEvent> {
        match control {
            TimelineControl::SetTimers(timers) => {
                let events = self.tick(now_ms);
                self.set_timers(timers);
                events
            }
            TimelineControl::Start => {
                self.start(now_ms);
                Vec::new()
            }
            TimelineControl::Pause => self.pause(now_ms),
            TimelineControl::Reset => {
                self.reset();
                Vec::new()
            }
        }
    }

    /// Advance to the monotonic time `now_ms` and return the timers that came
    /// due, earliest first. Time only counts while running; a `now_ms` that
    /// goes backwards counts as no time at all.
    pub fn tick(&mut self, now_ms: u64) -> Vec<TimerEvent> {
        let last = self.last_now.replace(now_ms);
        if self.status != TimelineStatus::Running {
            return Vec::new();
        }
        self.clock_ms += last.map_or(0, |last| now_ms.saturating_sub(last));
        let clock = self.clock_ms;

        let mut fired: Vec<(u64, usize, TimerEvent)> = Vec::new();
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let Some(due) = slot.due.filter(|due| *due <= clock) else {
                continue;
            };
            slot.runs += 1;
            let mut missed = 0;
            if slot.cfg.trigger.repeats() {
                // `runnable()` guarantees a non-zero interval.
                let interval = slot.cfg.trigger.ms();
                missed = u32::try_from((clock - due) / interval).unwrap_or(u32::MAX);
                slot.due = Some(due + (u64::from(missed) + 1) * interval);
            } else {
                slot.due = None;
                slot.done = true;
            }
            fired.push((
                due,
                index,
                TimerEvent {
                    id: slot.cfg.id.clone(),
                    name: slot.cfg.name.clone(),
                    action: slot.cfg.action.clone(),
                    run: slot.runs,
                    missed,
                    at_ms: clock,
                },
            ));
        }
        fired.sort_by_key(|(due, index, _)| (*due, *index));
        fired.into_iter().map(|(_, _, event)| event).collect()
    }

    /// Time until the next firing of `slot`.
    fn remaining(&self, slot: &Slot) -> Option<u64> {
        slot.due.map(|due| due.saturating_sub(self.clock_ms))
    }

    /// Copy of the state for the GUI.
    pub fn snapshot(&self) -> TimelineSnapshot {
        let timers: Vec<TimerView> = self
            .slots
            .iter()
            .map(|slot| TimerView {
                id: slot.cfg.id.clone(),
                phase: self.phase(slot),
                remaining_ms: self.remaining(slot),
                runs: slot.runs,
            })
            .collect();
        let next = timers
            .iter()
            .enumerate()
            .filter_map(|(index, view)| view.remaining_ms.map(|ms| (ms, index)))
            .min()
            .map(|(_, index)| index);
        TimelineSnapshot {
            status: self.status,
            clock_ms: self.clock_ms,
            timers,
            next,
        }
    }

    fn phase(&self, slot: &Slot) -> TimerPhase {
        if !slot.cfg.enabled {
            TimerPhase::Disabled
        } else if slot.cfg.problem().is_some() {
            TimerPhase::Invalid
        } else if self.status == TimelineStatus::Stopped {
            TimerPhase::Idle
        } else if slot.done {
            TimerPhase::Done
        } else {
            TimerPhase::Armed
        }
    }
}

// ---------------------------------------------------------------------------
// Thread wrapper
// ---------------------------------------------------------------------------

/// Called from the runner thread when something the GUI should redraw for
/// happened (an event fired, a control was applied).
pub type Wake = Arc<dyn Fn() + Send + Sync>;

enum Message {
    Control(TimelineControl),
    Shutdown,
}

/// A [`Timeline`] driven by its own thread, ticking every [`TICK_MS`].
///
/// All methods are non-blocking: controls go over a channel, the snapshot is a
/// short mutex-guarded copy and events are drained with `try_recv`.
pub struct TimelineRunner {
    tx: Sender<Message>,
    events: Receiver<TimerEvent>,
    shared: Arc<Mutex<TimelineSnapshot>>,
    handle: Option<JoinHandle<()>>,
}

impl TimelineRunner {
    /// Start the thread with these definitions (stopped).
    pub fn start(timers: Vec<TimerConfig>, wake: Option<Wake>) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<Message>();
        let (event_tx, events) = crossbeam_channel::unbounded::<TimerEvent>();
        let timeline = Timeline::new(timers);
        let shared = Arc::new(Mutex::new(timeline.snapshot()));
        let thread_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("bardwave-timeline".into())
            .spawn(move || run_loop(timeline, rx, event_tx, thread_shared, wake))
            .ok();
        Self {
            tx,
            events,
            shared,
            handle,
        }
    }

    /// Send a control to the thread.
    pub fn send(&self, control: TimelineControl) {
        let _ = self.tx.send(Message::Control(control));
    }

    /// Replace the definitions.
    pub fn set_timers(&self, timers: Vec<TimerConfig>) {
        self.send(TimelineControl::SetTimers(timers));
    }

    /// Arm / resume.
    pub fn start_timers(&self) {
        self.send(TimelineControl::Start);
    }

    /// Freeze.
    pub fn pause(&self) {
        self.send(TimelineControl::Pause);
    }

    /// Disarm everything.
    pub fn reset(&self) {
        self.send(TimelineControl::Reset);
    }

    /// The latest published state.
    pub fn snapshot(&self) -> TimelineSnapshot {
        self.shared
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// The next fired event, if any.
    pub fn try_event(&self) -> Option<TimerEvent> {
        self.events.try_recv().ok()
    }
}

impl Drop for TimelineRunner {
    fn drop(&mut self) {
        let _ = self.tx.send(Message::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn run_loop(
    mut timeline: Timeline,
    rx: Receiver<Message>,
    events: Sender<TimerEvent>,
    shared: Arc<Mutex<TimelineSnapshot>>,
    wake: Option<Wake>,
) {
    let epoch = Instant::now();
    let now = || u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
    loop {
        let mut fired = Vec::new();
        let mut changed = false;
        match rx.recv_timeout(Duration::from_millis(TICK_MS)) {
            Ok(Message::Shutdown) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                return;
            }
            Ok(Message::Control(control)) => {
                fired.extend(timeline.apply(control, now()));
                changed = true;
                while let Ok(message) = rx.try_recv() {
                    match message {
                        Message::Shutdown => return,
                        Message::Control(control) => {
                            fired.extend(timeline.apply(control, now()));
                        }
                    }
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
        }
        fired.extend(timeline.tick(now()));

        let snapshot = timeline.snapshot();
        if let Ok(mut guard) = shared.lock() {
            *guard = snapshot;
        }
        let any = !fired.is_empty();
        for event in fired {
            if events.send(event).is_err() {
                return;
            }
        }
        if any || changed {
            if let Some(wake) = &wake {
                wake();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TimerTrigger;

    fn timer(id: &str, trigger: TimerTrigger) -> TimerConfig {
        TimerConfig {
            id: id.into(),
            name: id.into(),
            trigger,
            action: TimerAction::StopScene,
            enabled: true,
        }
    }

    fn ids(events: &[TimerEvent]) -> Vec<&str> {
        events.iter().map(|e| e.id.as_str()).collect()
    }

    #[test]
    fn after_fires_once() {
        let mut tl = Timeline::new(vec![timer("a", TimerTrigger::AfterMs(1000))]);
        tl.start(0);
        assert!(tl.tick(999).is_empty());
        let events = tl.tick(1000);
        assert_eq!(ids(&events), ["a"]);
        assert_eq!(events[0].run, 1);
        assert!(tl.tick(5000).is_empty());
        assert_eq!(tl.snapshot().timers[0].phase, TimerPhase::Done);
    }

    #[test]
    fn every_repeats() {
        let mut tl = Timeline::new(vec![timer("a", TimerTrigger::EveryMs(500))]);
        tl.start(0);
        let mut runs = Vec::new();
        for now in (100..=1600).step_by(100) {
            runs.extend(tl.tick(now).into_iter().map(|e| (e.at_ms, e.run)));
        }
        assert_eq!(runs, [(500, 1), (1000, 2), (1500, 3)]);
    }

    #[test]
    fn late_tick_reports_missed_periods_once() {
        let mut tl = Timeline::new(vec![timer("a", TimerTrigger::EveryMs(1000))]);
        tl.start(0);
        let events = tl.tick(3500);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].missed, 2);
        // The next period is the one after 3500, not a replay of the old ones.
        assert!(tl.tick(3999).is_empty());
        assert_eq!(tl.tick(4000).len(), 1);
    }

    #[test]
    fn pause_does_not_accumulate_time() {
        let mut tl = Timeline::new(vec![timer("a", TimerTrigger::AfterMs(1000))]);
        tl.start(0);
        assert!(tl.tick(400).is_empty());
        assert!(tl.pause(400).is_empty());
        assert_eq!(tl.status(), TimelineStatus::Paused);
        // An hour passes while paused.
        assert!(tl.tick(3_600_000).is_empty());
        assert_eq!(tl.clock_ms(), 400);
        tl.start(3_600_000);
        assert_eq!(tl.snapshot().timers[0].remaining_ms, Some(600));
        assert!(tl.tick(3_600_500).is_empty());
        assert_eq!(ids(&tl.tick(3_600_600)), ["a"]);
    }

    #[test]
    fn pause_flushes_timers_already_due() {
        let mut tl = Timeline::new(vec![timer("a", TimerTrigger::AfterMs(1000))]);
        tl.start(0);
        assert_eq!(ids(&tl.pause(1200)), ["a"]);
    }

    #[test]
    fn reset_disarms_and_start_rearms_from_zero() {
        let mut tl = Timeline::new(vec![timer("a", TimerTrigger::AfterMs(1000))]);
        tl.start(0);
        tl.tick(600);
        tl.reset();
        assert_eq!(tl.status(), TimelineStatus::Stopped);
        assert_eq!(tl.clock_ms(), 0);
        assert!(tl.tick(5000).is_empty());
        assert_eq!(tl.snapshot().timers[0].phase, TimerPhase::Idle);
        assert_eq!(tl.snapshot().timers[0].remaining_ms, None);
        tl.start(5000);
        assert_eq!(tl.snapshot().timers[0].remaining_ms, Some(1000));
        assert!(tl.tick(5999).is_empty());
        assert_eq!(tl.tick(6000).len(), 1);
    }

    #[test]
    fn reset_lets_a_finished_one_shot_fire_again() {
        let mut tl = Timeline::new(vec![timer("a", TimerTrigger::AfterMs(100))]);
        tl.start(0);
        assert_eq!(tl.tick(100).len(), 1);
        tl.reset();
        tl.start(200);
        assert_eq!(tl.tick(300).len(), 1);
    }

    #[test]
    fn disabled_timer_never_fires() {
        let mut off = timer("off", TimerTrigger::AfterMs(100));
        off.enabled = false;
        let mut tl = Timeline::new(vec![off, timer("on", TimerTrigger::AfterMs(100))]);
        tl.start(0);
        assert_eq!(ids(&tl.tick(1000)), ["on"]);
        let snap = tl.snapshot();
        assert_eq!(snap.timers[0].phase, TimerPhase::Disabled);
        assert_eq!(snap.timers[0].remaining_ms, None);
    }

    #[test]
    fn invalid_timer_is_never_armed() {
        let mut tl = Timeline::new(vec![
            timer("zero", TimerTrigger::EveryMs(0)),
            TimerConfig {
                action: TimerAction::ActivateScene { scene: "  ".into() },
                ..timer("blank", TimerTrigger::AfterMs(100))
            },
        ]);
        tl.start(0);
        assert!(tl.tick(10_000).is_empty());
        assert!(tl
            .snapshot()
            .timers
            .iter()
            .all(|t| t.phase == TimerPhase::Invalid));
    }

    #[test]
    fn enabling_while_running_arms_from_now() {
        let mut off = timer("a", TimerTrigger::AfterMs(1000));
        off.enabled = false;
        let mut tl = Timeline::new(vec![off.clone()]);
        tl.start(0);
        tl.tick(5000);
        off.enabled = true;
        tl.set_timers(vec![off]);
        assert!(tl.tick(5999).is_empty());
        assert_eq!(tl.tick(6000).len(), 1);
    }

    #[test]
    fn disabling_while_running_disarms() {
        let mut cfg = timer("a", TimerTrigger::EveryMs(1000));
        let mut tl = Timeline::new(vec![cfg.clone()]);
        tl.start(0);
        tl.tick(500);
        cfg.enabled = false;
        tl.set_timers(vec![cfg]);
        assert!(tl.tick(5000).is_empty());
    }

    #[test]
    fn editing_the_action_keeps_the_countdown() {
        let mut cfg = timer("a", TimerTrigger::AfterMs(1000));
        let mut tl = Timeline::new(vec![cfg.clone()]);
        tl.start(0);
        tl.tick(600);
        cfg.action = TimerAction::NextTrack;
        tl.set_timers(vec![cfg]);
        let events = tl.tick(1000);
        assert_eq!(events[0].action, TimerAction::NextTrack);
    }

    #[test]
    fn changing_the_trigger_restarts_the_countdown() {
        let mut cfg = timer("a", TimerTrigger::AfterMs(1000));
        let mut tl = Timeline::new(vec![cfg.clone()]);
        tl.start(0);
        tl.tick(600);
        cfg.trigger = TimerTrigger::AfterMs(2000);
        tl.set_timers(vec![cfg]);
        assert_eq!(tl.snapshot().timers[0].remaining_ms, Some(2000));
    }

    #[test]
    fn new_timer_added_while_stopped_waits_for_start() {
        let mut tl = Timeline::new(Vec::new());
        tl.set_timers(vec![timer("a", TimerTrigger::AfterMs(100))]);
        assert!(tl.tick(10_000).is_empty());
        tl.start(10_000);
        assert_eq!(tl.tick(10_100).len(), 1);
    }

    #[test]
    fn simultaneous_events_come_earliest_first() {
        let mut tl = Timeline::new(vec![
            timer("late", TimerTrigger::AfterMs(900)),
            timer("early", TimerTrigger::AfterMs(300)),
        ]);
        tl.start(0);
        assert_eq!(ids(&tl.tick(1000)), ["early", "late"]);
    }

    #[test]
    fn snapshot_marks_the_nearest_timer() {
        let mut tl = Timeline::new(vec![
            timer("far", TimerTrigger::AfterMs(9000)),
            timer("near", TimerTrigger::AfterMs(3000)),
        ]);
        assert_eq!(tl.snapshot().next, None);
        tl.start(0);
        tl.tick(1000);
        let snap = tl.snapshot();
        assert_eq!(snap.next, Some(1));
        assert_eq!(snap.timers[1].remaining_ms, Some(2000));
    }

    #[test]
    fn backwards_clock_counts_as_no_time() {
        let mut tl = Timeline::new(vec![timer("a", TimerTrigger::AfterMs(1000))]);
        tl.start(5000);
        tl.tick(5500);
        tl.tick(5000);
        assert_eq!(tl.clock_ms(), 500);
    }

    #[test]
    fn start_while_running_is_a_no_op() {
        let mut tl = Timeline::new(vec![timer("a", TimerTrigger::AfterMs(1000))]);
        tl.start(0);
        tl.tick(600);
        tl.start(600);
        assert_eq!(tl.snapshot().timers[0].remaining_ms, Some(400));
    }

    #[test]
    fn runner_fires_and_pauses() {
        let runner = TimelineRunner::start(vec![timer("a", TimerTrigger::AfterMs(200))], None);
        assert_eq!(runner.snapshot().status, TimelineStatus::Stopped);
        runner.start_timers();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = None;
        while got.is_none() && Instant::now() < deadline {
            got = runner.try_event();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(got.map(|e| e.id), Some("a".to_string()));
        runner.pause();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(runner.snapshot().status, TimelineStatus::Paused);
        runner.reset();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(runner.snapshot().status, TimelineStatus::Stopped);
    }
}
