# Scenes, music and hotkeys

A **scene** is a saved mix of sounds with their levels and fades. A **playlist** is background music. **Emitters** play random sounds while a scene runs. **Timers** fire actions later. **Hotkeys** trigger these from the keyboard.

This page was written from the source code and its unit tests. Behaviour not checked on real hardware is marked **(not verified)**. For the file format of `config.toml` see [Configuration](./configuration.md); for sounds, stingers and audio formats see [Sound library](./soundboard-library.md).

## Contents

- [Scenes](#scenes)
- [Stopping sound](#stopping-sound)
- [Emitters and scene music](#emitters-and-scene-music)
- [Music playlists](#music-playlists)
- [Timers](#timers)
- [Buses, ducking and normalization](#buses-ducking-and-normalization)
- [Hotkeys](#hotkeys)
- [Configuration reference](#configuration-reference)
- [Troubleshooting](#troubleshooting)

<a id="scenes"></a>
## Scenes

A scene starts a set of sounds with one action. When you activate it, sounds that are not part of the scene fade out, unless **Stop other sounds** is off (`stop_others = false`). Spared in any case: stingers that are playing, the scene's own emitter sounds, the track of the scene's own playlist, and a sound of the scene that is marked **Manual**.

Activating the scene that is already active does not restart anything. A sound of the scene that already plays is only retuned to the scene's level and loop setting, and delayed starts that are still waiting keep their time. A sound that appears twice in one scene counts once (the first item wins). Items whose sound is missing from the library are skipped and reported in the log.

### Save the current mix

1. Start the sounds and set their levels.
2. Press **Save Mix As Scene** in the **Scenes** deck (**Save Mix** on narrow windows, an icon only on the narrowest).
3. Type a name in **Scene name**, up to 64 characters.
4. Press **Save Scene**.

The scene keeps the sounds that play now, at their current levels and loop settings. Stingers are left out. Every item starts automatically with no delay and no pan override. The fades are 2000 ms, the curve is equal power, and `stop_others` is on. With nothing playing, the button is disabled. A duplicate name (compared without case) is refused with `A scene with this name already exists`.

### The deck and scene menu

- The header shows the scene count and the active scene. **Hide** / **Show** collapses the deck. **New** creates an empty scene (opens the editor).
- Clicking a scene card activates it. The active card reads `ACTIVE`, and `ACTIVE · FADING IN n%` while the fade-in runs. A `!` badge on a card lists problems such as a missing sound or file. The pencil on a card opens the editor.
- Right-click a card for **Activate**, **Edit…**, **Duplicate** and **Delete…**. **Duplicate** copies items, emitters, music and fades under the name `X copy`. **Delete…** asks for confirmation (**Delete** / **Keep**), keeps the sounds in the library, and removes hotkeys that start the scene. A timer that points at the scene is not removed; its row shows a problem and it logs a warning when it fires.
- Right-click a sound in the sound list and choose a scene under **Add to scene** to add it at its saved level.

### Scene editor

| Field | Default | Notes |
| --- | --- | --- |
| Scene name | `Scene N` | Up to 64 characters, unique without regard to case. |
| Button colour | none | Eight preset swatches, **None**, or the **Hex** field (`#rgb` / `#rrggbb`; the `#` is added if missing). A dark colour is lightened on the card so it stays visible. |
| Fade in (ms), Fade out (ms) | 2000 | 0 to 60000. Accepts `2000`, `2000 ms`, `2.5 s`. |
| Curve | Equal power | `Equal power` (constant summed power across a crossfade) or `Linear`. |
| Stop other sounds: on / off | on | Turns `stop_others` on or off. |

Choose sounds with **Add sound…**. Each item has a level slider (the item's level replaces the sound's own volume inside the scene), **Loop**, a start mode (**Auto** or **Manual**), a pan slider and a delay field (`0` by default, up to 600000 ms, `1.5 s` accepted) and a remove button. A new item takes the sound's own level and loop flag.

- **Auto** (`on_enter`) starts the item when the scene is activated, after its delay.
- **Manual** (`manual`) lists the item but starts it only when you start it by hand, for example from the sound list. Its delay is ignored and the field is disabled.
- The pan slider goes from L 100 % to R 100 %; double-click centres it. A non-zero item pan replaces the sound's own pan while that scene is active; centre means the sound's own pan.
- Delayed starts are kept by the Director and released on its tick. If you leave the scene before the delay ends, the start is dropped.

Emitters and the scene's music playlist are edited in a separate dialog (see [Emitters and scene music](#emitters-and-scene-music)); saving the scene editor leaves them untouched.

### Transition time

The **Transition** row has an **Auto** toggle and a slider from 0 to 10 s in 100 ms steps, starting at 2.0 s. With **Auto** on, the row reads `Per scene` and each scene uses its own fades. Moving the slider turns **Auto** off and applies that time as both fade-in and fade-out to scene cards, **Stop Scene** and **Fade Out**. The slider is not saved in `config.toml`, and the scenes themselves are not changed.

> **Note:** Hotkeys and timers ignore the slider and use each scene's own fades. This was read from the code and not run.

### Stingers

A stinger is a one-shot sound layered over everything. It does not stop other sounds, and scene changes spare it while it plays. Right-click a sound and choose **Use As Stinger** (or **Remove Stinger Flag**). Each stinger gets a pad under the scene grid, in the **Stingers** row; the pad shows the sound's name and plays it once. Stingers are not part of a saved mix. A stinger plays on the sound's bus at the sound's own level. In `config.toml` the flag is `oneshot_stinger = true`. A hotkey for a stinger sound and the timer action **Play stinger** fire it the same way.

<a id="stopping-sound"></a>
## Stopping sound

| Control | Stops | Fade time |
| --- | --- | --- |
| **STOP ALL** (command bar, the panic button) | Everything at once: sounds, scene, music and the headphone preview. | None (only a short click-protection ramp). |
| **Stop Scene** (deck; disabled with no active scene) | The active scene's sounds that play, its emitters and the music its start brought in. Stingers and other sounds keep playing. | Slider if **Auto** is off, otherwise the scene's `fade_out_ms`. |
| **Fade Out** (deck) | Everything, including stingers, emitters and music. Waiting delayed starts are dropped. | Slider if **Auto** is off, otherwise the active scene's `fade_out_ms`, otherwise 2000 ms. |
| Hotkey `Stop scene` | Same as **Stop Scene**. | The scene's own `fade_out_ms` (the slider is ignored). |
| Hotkey `Stop all` | Everything. The headphone preview stops too. | The active scene's `fade_out_ms`, or 0 ms with no active scene. |
| Timer `Stop all (fade out)` | Everything. The headphone preview stops too. | The action's `fade_ms` (whole seconds, 0 to 120 in the console). |

After **Fade Out**, **STOP ALL** or `Stop all`, no scene is active. **Stop Scene** also leaves no scene active.

<a id="emitters-and-scene-music"></a>
## Emitters and scene music

Open **Music…** (in the music bar at the bottom) and choose the **Scenes** tab. Press **Emitters & music…** on a scene row. The dialog edits the scene's emitters and music; **Save** applies the changes (it is disabled until something changed and while an emitter has no sound) and **Close** leaves.

- **Music playlist** picks a playlist, or `None` (`playlist = "<id>"` in `config.toml`). Activating the scene starts it, fading in over the longer of the scene's fade-in and the playlist's crossfade. Stopping the scene fades it out. Details:
  - A playlist that already plays when the scene is activated is not restarted.
  - A scene without a playlist stops music that an earlier scene brought in, but not a playlist you started by hand.
  - A scene with **Stop other sounds** on fades out a hand-started playlist that is not its own.
  - **Stop Scene**, **Fade Out**, **STOP ALL** and the matching hotkeys end the scene's music with the scene.
  - A missing playlist is reported and the scene's music layer is stopped.
- **Add emitter** adds a random sound source. Each emitter has:

| Setting | Default | Meaning |
| --- | --- | --- |
| Every … to … | 20 s to 60 s | Shortest and longest pause. The console allows 0.1 s to 1 h. A minimum above the maximum drags the maximum along, and the other way round. |
| Volume | 100 % | Base level. |
| Volume jitter | 0 % | Random spread of the level: `volume x (1 + jitter x u)`, `u` random from -1 to 1, kept within 0 to 1. |
| Pan jitter | 0 % | Random stereo position: `jitter x u`; 0 is centre, 100 % can land anywhere. |
| Max overlap | 2 | Simultaneous instances of this emitter, 1 to 16 in the console. |
| Sounds | none | One is picked at random per play, preferring a sound that is not sounding. At least one is needed. |

Behaviour:

- The first play comes after a random shift of up to 5 s (never later than the longest pause), so a scene does not open with a burst.
- Each emitter waits a random time between its two limits. The scheduler never fires more often than every 100 ms.
- A play lasts as long as its sound (5 s is assumed when the length is unknown). If all overlap slots are busy when a play is due, that play is skipped and a new pause is drawn.
- An emitter fires at most once per tick, and the next pause counts from that tick, so a stalled app does not play a burst afterwards.
- Activating the running scene again keeps the emitter timers. Emitters stop with the scene. Instances that still sound are faded out with the scene's fade-out when the scene stops, and spared when the same scene is activated again.
- While the scene runs, each emitter card shows a live countdown (`Next in 12 s`, plus `n playing`). Without a running scene it reads `Scene not active`; with no playable sounds, `No playable sounds`.
- Emitters run on the Director's tick, with no extra thread. The engine keeps one level and pan per sound, so an instance started while the same sound still plays keeps that sound's pan and level.

> **Note:** Each scene start uses a random seed (tests fix the seed). Playback on real hardware is **(not verified)**.

<a id="music-playlists"></a>
## Music playlists

A playlist is an ordered list of library sounds on the **Music** bus. It plays from the music bar or as a scene's music layer. Only one playlist plays at a time; starting another replaces it.

| Setting | Default | Meaning |
| --- | --- | --- |
| Shuffle | off | Each cycle uses every track once before any repeats; a new cycle never starts with the track that ended the last one. |
| Repeat | All | `Off` stops at the end (status `Finished`). `All` restarts the list. `One` loops the current track without a gap. The button cycles Off, All, One. |
| Crossfade | 2.0 s | `Off` (0) or up to 30 s. The next track starts early, `crossfade` before the end, at most half-way through the current one, so the two overlap. It needs a known track length, and does not apply with Repeat `One` or at the last track with Repeat `Off`. |
| Bus (`bus`) | Music | `music`, `ambience` or `sfx`. Set in `config.toml` only; the console has no control for it. |

Shuffle, Repeat and Crossfade are saved with the playlist. Changing Shuffle or Repeat while the playlist plays takes effect at once.

The music bar (bottom of the console) has **Previous track**, a play button (**Play the playlist** / **Pause** / **Resume**, or **Create a playlist** when there are none), **Stop the music**, **Next track**, a seek bar with clocks, **Shuffle**, **Repeat Off / All / One**, the current track name with its position (`Track 3 / 12`) and the **Music…** button. **Previous track** restarts the current track if it has played more than 3 s (or if nothing comes before it); otherwise it goes to the previous track. **Stop the music** fades out over the playlist's crossfade time. Next and Previous crossfade too.

In the **Music…** manager (**Playlists** tab), **New playlist** creates a list. The manager has play, stop, shuffle, repeat and crossfade controls, a rename button, and a delete button (the sounds stay in the library). Under **Add sounds** you filter the library by name or tag and add tracks to the end with the **+** buttons. Reorder tracks by dragging the handle or with **Move up** / **Move down**, and remove a track with **Remove from the playlist**. Deleting a sound from the library also removes it from playlists.

- Seeking works only while a track is playing or paused and its length is known, and never lands within 500 ms of the end.
- Names come from file tags as `Artist - Title`, or `Title` alone, then the sound's name, then `Unknown track`. The album tag is read but the console does not show it.
- Missing or unplayable tracks are skipped and reported. If no track can be played, the playlist stops and says so. A track stopped by someone else (not near its end) stops the playlist instead of jumping to the next track.
- Long files are streamed (see `stream_threshold_secs`). Decoding uses Symphonia with the `vorbis`, `mp3`, `flac`, `wav` and `aac` features; the module comment also names Opus, but no Opus decoder feature is enabled in `Cargo.toml`, so Opus playback is **(not verified)**. See [Sound library](./soundboard-library.md) for the format table.

<a id="timers"></a>
## Timers

Open **Timers** from the Mix section (the **Timers** button under **Duck** and **Hotkeys**; while timers run it shows `Timers MM:SS` for the next one). Timers run only while the app is open and always start stopped; only their definitions are saved.

- **Start** arms every enabled timer. It reads **Resume** after a pause.
- **Pause timers** freezes the countdowns.
- **Reset** disarms everything. The next **Start** begins from zero.
- The status line reads `Stopped`, `Running mm:ss` or `Paused mm:ss`. Each timer row has an enable checkbox, a countdown (`OFF`, `DONE`, `--:--`), and edit and delete buttons. The **Fired** list at the bottom logs what ran and any warnings.

**New timer** opens a form with **Name** (optional, up to 60 characters; a name is generated if left empty), **When**, **Action** and a target. **When** is **Once, after** or **Repeat every** with minutes (0 to 999) and seconds. The interval must be at least 1 second (the config itself accepts 100 ms). A new form starts at **Once, after** 10 min with **Activate scene**. A repeating timer that was late fires once and reports the missed periods.

| Action label | `kind` | Extra field |
| --- | --- | --- |
| Activate scene | `activate_scene` | `scene` |
| Stop scene | `stop_scene` | none |
| Play stinger | `play_stinger` | `sound` (any library sound, played once) |
| Stop all (fade out) | `stop_all` | `fade_ms` |
| Start playlist | `start_playlist` | `playlist` |
| Stop music | `stop_playlist` | none |
| Next track / Pause music / Resume music | `next_track` / `pause_music` / `resume_music` | none |

A timer that points at something missing logs a warning and does not affect the others; its row shows `Cannot run: …` or a line such as `Scene no longer exists in the library`. A disabled timer, or one with an invalid definition, is never armed. Editing timers while they run keeps the countdowns of unchanged ones.

<a id="buses-ducking-and-normalization"></a>
## Buses, ducking and normalization

Every sound plays on one bus: **Music**, **Ambience** or **Sfx**. A sound's bus is `bus` in its `[[library.sounds]]` entry and defaults to `ambience`; a playlist's default is `music`. The console has no control to change either; edit `config.toml`. Each bus has a fader in the Mix section (network column). Faders are not saved and return to 100 % after a restart.

**Duck** lowers the buses listed in `duck_buses` (Music and Ambience by default) while it is active; it reads **Ducked** when it is. **Click** latches it until the next click. **Hold** ducks only while the button is down (a press shorter than 350 ms counts as a click). A latch and a held key do not cancel each other. The collapsed **Duck settings** section sets:

| Setting | Config key | Default | Console range |
| --- | --- | --- | --- |
| Level | `duck_amount` | `0.3` (about -10 dB) | 0 to 100 % |
| Attack | `duck_attack_ms` | `200` | 0 to 10000 ms |
| Release | `duck_release_ms` | `800` | 0 to 30000 ms |
| Ducks | `duck_buses` | `["music", "ambience"]` | any of the three buses |

Ducking does not change the fader value: after the release the bus is back at exactly its fader level. `duck_amount = 1.0` disables ducking and `0.0` mutes the ducked buses. The hotkeys `Duck background (toggle)` and `Duck background (hold)` use the same settings and state as the button.

**NORMALIZE LOUDNESS** (`normalize`, off by default) measures sounds with EBU R128 in the background and stores a `gain_db` correction toward `normalize_target_lufs` (**Target**, default `-18.0`, console range -36 to -8). It measures loaded sounds whose `gain_db` is `0`, once per session; a measured correction is limited to -24 to +12 dB. Changing the target does not rewrite gains already stored, it only lets unmeasured sounds be measured again. Files longer than `stream_threshold_secs` (default 60) are not measured. A sound's level is its volume times the gain, kept within 0 to 1, so a positive gain only helps when the volume is below 1.0. The line under the switch reads `Off: sounds keep their own level`, `On: new sounds are measured and set to -18 LUFS` or `Measuring n sounds...`. Results on real speakers are **(not verified)**.

<a id="hotkeys"></a>
## Hotkeys

Open **Hotkeys** in the Mix section. The dialog lists your bindings (**Bindings (n)**) and has a **New hotkey** row: a menu for the action, a second menu for the target when the action needs one, **Capture key** (press **Esc** to cancel) or a typed combination in the field `or type: Ctrl+Shift+1` (press **Enter** to accept it), and **Add hotkey**. Click a key in the list to change it. The delete button of a row (**Delete this hotkey**) removes a binding. **Done** closes the dialog. While the editor waits for a key, no binding fires. The numeric keypad and the Win/Super key cannot be captured; type them instead.

### Key syntax

Zero or more modifiers and one key, joined by `+`, case-insensitive, in any order. Modifiers: `Ctrl` (`Control`), `Alt` (`Option`), `Shift`, `Cmd` (`Command`, `Super`, `Meta`, `Win`). `CmdOrCtrl` means `Cmd` on macOS and `Ctrl` elsewhere. Keys: `A` to `Z`, `0` to `9`, `F1` to `F24`, `Numpad0` to `Numpad9` (and `NumpadAdd`, `NumpadSubtract`, `NumpadMultiply`, `NumpadDivide`, `NumpadDecimal`, `NumpadEnter`), `Space`, `Enter`, `Escape`, `Tab`, `Backspace`, `Delete`, `Insert`, `Home`, `End`, `PageUp`, `PageDown`, the arrows `Up`, `Down`, `Left`, `Right`, and punctuation such as `Minus`, `Comma` or `Slash`. The console writes the canonical form (`Ctrl+Alt+Shift+Cmd+Key`).

### Actions

| Menu label | `kind` | Target |
| --- | --- | --- |
| Play sound / stinger | `sound` | sound id |
| Activate scene | `scene` | scene id |
| Start playlist | `playlist` | playlist id |
| Stop all | `stop_all` | none |
| Stop scene | `stop_scene` | none |
| Master volume up / down | `master_up` / `master_down` | none (step 0.05) |
| Master mute | `master_mute` | none |
| Duck background (toggle) | `duck_toggle` | none |
| Duck background (hold) | `duck_hold` | none |
| Pause / resume music | `pause_music` | none |
| Next track | `next_track` | none |
| Previous track | `prev_track` | none |

A sound hotkey toggles a normal sound (it starts at its own level, and a second press stops it) and fires a stinger as a one-shot. Master mute restores the previous volume (0.8 if the mute came from somewhere else). A key release is ignored by every action except `Duck background (hold)`; if the release is lost (the window loses focus, or a text field takes the keyboard), the ducking is released by force. Hotkeys for scenes and sounds use the library's current data, and each scene's own fades.

### Local and global

- **Local** hotkeys fire while the Bardwave window has focus.
- **Global** hotkeys also fire in the background. Enable them with `[gui] global_hotkeys = true` or the **GLOBAL HOTKEYS (WHEN THE WINDOW IS NOT FOCUSED)** checkbox. Off by default. The dialog shows the state (`Global hotkeys are off.`, `Global hotkeys active (n).`, or `n could not be registered` when the system refuses a key, for example because another program owns it).
- Only combinations with `Ctrl`, `Alt` or `Cmd`, and `F`-keys, can be global. Plain keys such as `Space`, `A` or `Shift+1` are never global.
- While a text field has focus, plain keys and `Shift+key` are typed into it instead of firing; combinations with `Ctrl`, `Alt` or `Cmd`, and `F`-keys, still fire.
- Keypad keys (`Numpad…`) cannot be seen by the window: they work only as global hotkeys, so they need a modifier and the global option. `Cmd` bindings match locally on macOS only (the window cannot see the Windows/Super key).
- A key that is registered globally is delivered while the window has focus too; the local match is skipped to avoid firing twice.
- macOS may need permission under **System Settings > Privacy & Security** (Accessibility or Input Monitoring); without it, shortcuts only work while the window is focused. The code comments say ordinary keys need no permission, so this is **(not verified)**.
- Windows: **(not verified)**. Linux: X11 only, since Wayland does not allow global hotkeys. **(not verified)**

### Conflicts and MIDI

The dialog shows a message under a row when something is wrong. The first of several bindings with the same key wins.

| Message | Meaning |
| --- | --- |
| `Conflict: the same key is bound in #N, which wins - this one never fires` | An earlier binding wins. Adding a key that is already bound offers **Replace**, which replaces that binding instead of adding a duplicate. |
| `Conflict: the same key is also bound in #N, which never fire` | This binding wins over the listed later ones. |
| `The sound, scene or playlist it points at no longer exists` | The target was deleted. |
| Invalid key text (for example `…has two keys…`) | The key does not parse. |

The console deletes bindings that point at a sound, scene or playlist you delete there. A hotkey that cannot run (for example `Hotkey ignored: the audio engine is not running`) is written to the log, and errors from running an action appear in the dialog.

MIDI bindings use `key = "Midi:Ch1:Note60"` or `key = "Midi:CC7"` and can only be written in `config.toml`. The code for them is built only with the `midi` cargo feature, and even then it is not connected to the console. The console reports them as `MIDI binding: MIDI input is not connected in this build, it will not fire`. Treat MIDI as unavailable **(not verified on hardware)**.

<a id="configuration-reference"></a>
## Configuration reference

Scenes, playlists and sounds live under `[library]`. Hotkeys and timers are top-level. Ids are UUID strings written by the console. A hand-written id that is not a UUID is replaced when the file loads, and references in scenes, emitters, playlists and `[[hotkeys]]` are rewritten; `[[timers]]` are not, so use real UUIDs there. Defaults: scene fades `2000`; scene `curve = "equal_power"`; `stop_others = true`; item `volume = 1.0`, `start = "on_enter"`, `delay_ms = 0`; emitter `min_interval_ms = 20000`, `max_interval_ms = 60000`, `volume = 1.0`, `volume_jitter = 0`, `pan_jitter = 0`, `max_overlap = 2`; playlist `repeat = "all"`, `crossfade_ms = 2000`, `bus = "music"`; sound `bus = "ambience"`; timer `enabled = true`.

```toml
[gui]
global_hotkeys = false

[[library.playlists]]
id = "6a0f3b1e-52c4-4f0e-9a52-0b1f5f3a7a01"
name = "Tavern music"
items = ["2f6c1d8a-7b3e-4c52-8d61-9e0a4b7c1d02", "2f6c1d8a-7b3e-4c52-8d61-9e0a4b7c1d03"]
shuffle = true
repeat = "all"
crossfade_ms = 3000

[[library.scenes]]
id = "8c2d9e44-1f6a-4b7d-a3c5-5e7f0a9b2c04"
name = "Tavern"
fade_in_ms = 2000
fade_out_ms = 2000
curve = "linear"
stop_others = true
playlist = "6a0f3b1e-52c4-4f0e-9a52-0b1f5f3a7a01"

[[library.scenes.items]]
sound = "2f6c1d8a-7b3e-4c52-8d61-9e0a4b7c1d05"
volume = 0.4
looped = true
start = "on_enter"

[[library.scenes.emitters]]
sounds = ["2f6c1d8a-7b3e-4c52-8d61-9e0a4b7c1d06", "2f6c1d8a-7b3e-4c52-8d61-9e0a4b7c1d07"]
min_interval_ms = 20000
max_interval_ms = 60000
max_overlap = 2

[[hotkeys]]
key = "Ctrl+1"
action = { kind = "scene", target = "8c2d9e44-1f6a-4b7d-a3c5-5e7f0a9b2c04" }

[[hotkeys]]
key = "F5"
action = { kind = "stop_all" }

[[timers]]
id = "b13e5f70-3a8d-4e19-b6c2-7d4a9e0f1c08"
name = "Alarm in ten minutes"
trigger = { after_ms = 600000 }
action = { kind = "activate_scene", scene = "8c2d9e44-1f6a-4b7d-a3c5-5e7f0a9b2c04" }
enabled = true

[[timers]]
id = "c24f6a81-4b9e-4f2a-87d3-8e5bab1f2d09"
name = "Bell every half hour"
trigger = { every_ms = 1800000 }
action = { kind = "play_stinger", sound = "2f6c1d8a-7b3e-4c52-8d61-9e0a4b7c1d0a" }
enabled = true
```

Audio settings under `[audio]`: `duck_amount` (0.3), `duck_attack_ms` (200), `duck_release_ms` (800), `duck_buses` (`["music", "ambience"]`), `normalize` (false), `normalize_target_lufs` (-18.0), `stream_threshold_secs` (60, `0` turns streaming off).

> **Warning:** The console rewrites `config.toml` when it saves. Close Bardwave before editing the file by hand.

> **Note:** A timer `stop_all` written without `fade_ms` fades for `0` ms. The console's new-timer default is 2 s.

<a id="troubleshooting"></a>
## Troubleshooting

| Symptom | Likely cause and fix |
| --- | --- |
| **Save Mix As Scene** is disabled | Nothing plays. Start a sound first. |
| **Stop Scene** is disabled | No scene is active. |
| A hotkey does nothing | A conflict, a deleted target or an invalid key. Read the message in the Hotkeys dialog and the log. |
| A plain key works only in the window | Plain keys are never global. Add a modifier. |
| Global hotkeys do nothing | Option is off, permission is missing, or the session is Wayland. **(not verified)** |
| A timer never fires | Timers were not started or are disabled. Press **Start**. |
| An emitter is silent | Overlap limit reached, scene not active, or no sounds. |
| A playlist track is skipped | The file is missing or cannot start. |
| Seek bar does nothing | The track length is unknown, or nothing plays. |
| Fade Out ignores the slider | **Auto** is on. Move the slider. |
| A hotkey or timer fades with the wrong time | They use each scene's own fades, not the Transition slider. |

## Scope

Verified against source and unit tests: scene, emitter, timer, playlist and duck logic, hotkey parsing and dispatch, and console labels. Not verified: audio output on real hardware, global hotkeys on macOS, Windows and Linux, MIDI, Opus playback, and the installers. For the HTTPS setting of the remote page, see [HTTPS](./https.md).
