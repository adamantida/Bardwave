# Sound library

The sound library is the `01 / Library` panel of the console (titled **Sound Library**, with a counter such as `12 Tracks`). It holds every sound the board can play, the groups they belong to, their tags, and the settings each sound carries (volume, repeat, pan). This page explains how to add sounds, organise them, repair missing files, share them as bundles, and tune the related audio settings. The labels and values below are taken from the application code. Some labels are drawn in capitals on screen (for example `FILE NOT FOUND`, `NO MATCHES`); this page uses the spelling from the code. Behaviour that was not checked on real hardware is marked **(not verified)**.

## Contents

- [Adding sounds](#adding-sounds)
- [Supported formats](#supported-formats)
- [Per-sound settings](#per-sound-settings)
- [Groups](#groups)
- [Tags and search](#tags-and-search)
- [Missing files](#missing-files)
- [Bundles](#bundles)
- [Headphone preview](#headphone-preview)
- [Loudness normalization](#loudness-normalization)
- [Streaming long files](#streaming-long-files)
- [Configuration reference](#configuration-reference)
- [Troubleshooting](#troubleshooting)

The library is saved in the `[library]` section of `config.toml`. The sounds you add are kept there, not in the audio files themselves: the library stores the path to each file. Your own audio files are never changed or deleted by the library; removing a sound only removes its entry.

## Adding sounds

The header of the panel has two buttons (**Add Sound** and **YouTube**). **Folder…**, **Export…** and **Import…** are in the search row below it.

A sound that is added while a group is selected in the **Groups** rail (not **All Sounds** and not **Ungrouped**) lands in that group. This applies to **Add Sound** and to YouTube downloads. A folder import creates groups by itself, see below.

A new sound gets the file name without its extension as its name (a YouTube download gets the video title) and a volume of 100%. A file that is already in the library (same path) is not added twice.

### Add Sound

Press **Add Sound** (with a plus icon) in the panel header. The same button is shown in the empty list. The system file chooser opens with the title `Select Audio Files`, with the filter `Audio Files` set to `mp3, wav, ogg, flac, opus`. You can select several files at once. Each file is sent to the audio engine and appears in the list when it is decoded.

> **Note:** A file that cannot be decoded is not added. The reason is written to the log (`Failed to load sound`). The UI does not show a message for this case.

### Folder…

Press **Folder…** in the search row. It has the tooltip `Add every sound in a folder (and its sub-folders)`.

1. Choose a folder in the chooser titled `Choose a folder with sounds`.
2. The **Add Folder** dialog scans the folder recursively (up to 64 levels deep). Entries are visited in sorted order, the folder's own files first, then sub-folders. The scan only looks; it changes nothing in the library.
3. The dialog shows the live progress, then the number of new sounds (`N new sounds found`) and, when there are sub-folders, a **Sub-folders** list with the sound count of each one.
4. Tick **Create a group for each sub-folder** if you want one group per first-level sub-folder (the box is ticked by default when sub-folders were found). Files in deeper levels go to the group of their first-level sub-folder. The dialog lists the groups it will create (up to 4 names) and how many existing groups are reused. A group with the same name, regardless of case, is reused. Group names are cut to 32 characters. Files directly in the chosen folder stay ungrouped.
5. Press **Add N Sounds** (the button is disabled when nothing new was found) to add them. **Cancel** closes the dialog without adding anything.

While the scan runs, **Cancel** closes the dialog and stops the scan. Nothing is added.

When sounds are added, a message such as `Added 12 sounds, 2 new groups` appears under the search row for a few seconds.

The scanner skips:

- files already in the library (by path);
- files with the same content as another file, or as a file already in the library (same size and the same hash of the first and last 64 KiB; files up to 128 KiB are hashed completely);
- empty files and AppleDouble companions (`._name.mp3`);
- symbolic links that point outside the scanned folder, and broken links. Files reached through a link are visited last, so a real file wins over a link to it.

The dialog tells how many were skipped for each reason, for example `3 duplicates skipped (same content as another file)`, and how many entries could not be read.

### YouTube

Press **YouTube** in the panel header, or in the empty list. The header button is dropped first when the panel is narrow. The dialog is **Add From YouTube**, with the field `Video or music link`. Press **Add** or Enter.

Accepted links:

- `https://www.youtube.com/watch?v=<id>` (extra parameters such as `&t=` or `&list=` are ignored: a link with `&list=` adds only that one video);
- `https://youtu.be/<id>`;
- `https://music.youtube.com/watch?v=<id>` (any `youtube.com` sub-domain, and `youtube-nocookie.com`, works);
- `/shorts/<id>`, `/embed/<id>` and `/live/<id>` URLs;
- a bare 11-character video id.

Playlist pages, channel pages and other sites are rejected with `Not a YouTube video link`.

The audio is downloaded once with yt-dlp. The file is saved in the `bardwave-library` folder inside the Bardwave data directory of the active profile, so it survives restarts. Downloads are converted to MP3 (160K) with ffmpeg. The file name is the video title (cut to 80 characters, with characters that are illegal on Windows replaced) followed by the video id in brackets. The row shows the title without the id. A video that was already downloaded is reused (`Already downloaded — reusing`), without any network traffic.

The first use needs helper tools. The dialog states `First use downloads yt-dlp + ffmpeg once (~230 MB), then it just works`. The progress lines quote about 24 MB for yt-dlp and about 100 MB for ffmpeg. While a download runs, the dialog shows these progress lines (`Connecting…`, `Downloading yt-dlp…`, `Fetching video info…`, `Downloading audio…`).

Bardwave looks for each tool next to the executable first, then on `PATH`, then in its own `tools` folder (in the root data directory, shared by all profiles). A copy you installed yourself is used before a downloaded one.

| Tool | When it is downloaded | If it is missing |
|---|---|---|
| yt-dlp | On first use, about 24 MB, from its GitHub release | Downloaded automatically on Windows, macOS and Linux (x86-64 and ARM64). Not verified on macOS and Linux. Other systems get the message to install it manually |
| ffmpeg | On first use, about 100 MB (a Windows build) | Downloaded automatically on Windows only. On other systems the download stops with `ffmpeg not found on this system.` Install it yourself, for example with `brew install ffmpeg` or `apt install ffmpeg` |

Errors from yt-dlp (a removed video, a region lock, no network) are shown in the dialog.

> **Note:** There is one YouTube download at a time. **Close** (shown instead of **Cancel** during a download) and Esc do not cancel it: the dialog is closed, the download continues in the background and the sound appears when it finishes. While it runs, the **YouTube** button does nothing. If the download fails after you closed the dialog, the error is only written to the log.

## Supported formats

Files are decoded with Symphonia. The build enables the `vorbis`, `mp3`, `flac`, `wav` and `aac` features (`Cargo.toml`). Decoded audio is converted to 48 kHz stereo (mono is duplicated to both channels, multichannel files are folded down to two).

| Extension | Status | Notes |
|---|---|---|
| `.wav` | Supported | Add Sound, folder scan, Locate, bundles |
| `.mp3` | Supported | Add Sound, folder scan, Locate, bundles |
| `.ogg` | Supported | Vorbis. Add Sound, folder scan, Locate, bundles |
| `.flac` | Supported | Add Sound, folder scan, Locate, bundles |
| `.aac` | Listed | Accepted by the folder scan, Locate and bundles, but not offered by the Add Sound filter. Playback not verified |
| `.m4a` | Listed | Accepted by the folder scan, Locate and bundles, but not offered by the Add Sound filter. The MP4 container is not enabled in the build, so playback is not verified |
| `.opus` | Not verified | Offered by the Add Sound filter, but not accepted by the folder scan, Locate or bundles, and no Opus decoder is enabled. Treat it as unsupported |

The folder scan, **Locate…** and bundle import use the same extension check (case-insensitive) apart from `.opus`, which is only in the Add Sound filter. If **Locate…** gets a file with another extension, the message `That file is not a supported audio file` appears.

## Per-sound settings

Each sound row has the following controls, from left to right. The row also shows the name, a line such as `48 kHz / 00:42 / WAV` (hidden first on a narrow row) and the tags.

| Control | How to use it | Range | Default | Stored as |
|---|---|---|---|---|
| Play / stop button | Click the round button on the left | — | — | — |
| Repeat | Click the square icon. Tooltip: `Repeat: on` or `Repeat: off`. Takes effect at once, also on a playing sound | on / off | off | `looped` |
| Preview | Headphones icon. See [Headphone preview](#headphone-preview) | — | — | not stored |
| Volume | Click or drag on the slider on the row. The percentage is shown next to it | 0% to 100% | 100% | `volume` (0.0 to 1.0) |
| Group | Click the group name on the row. Tooltip: `Change group` | — | No group | `group` |
| Remove | Click the ✕ icon | — | — | removes the sound |

The slider and the group name are hidden on very narrow rows. Hover over a row to see its full name, path and group. Rows are listed in the order the sounds were loaded; sounds whose file is missing and that are not loaded come last.

**Remove** (✕) on a row deletes the sound from the library at once, without a confirmation. It also removes the sound from every scene, emitter, playlist and hotkey that uses it. The audio file on disk is not touched.

### Pan

Pan is set from the right-click menu of a row, in the **Pan** block.

- The slider runs from `L 100%` to `R 100%`. It reads `Centre` at zero. Values very close to the centre (under 5%) snap to the centre.
- Double-click the slider, or press Home while it has focus, to reset it to the centre.
- With the keyboard, the arrow keys move the slider in steps of 5%. Hold Shift for steps of 25%.
- Moving the slider changes the sound at once, also while it plays.
- A centred pan is not written to the config file.

### Stingers and scenes

The same right-click menu has **Use As Stinger** (or **Remove Stinger Flag**) and **Add to scene** entries. A stinger is a one-shot played over the scene. It does not stop other sounds and gets a pad in the scene deck. Scenes are outside the scope of this page.

> **Note:** The mixer bus of a sound (`bus`: `music`, `ambience` or `sfx`) is a config setting. I did not find a control for it on the sound row or in its menu (not verified).

## Groups

Groups are the categories of the library. They are listed in the **Groups** rail on the left of the panel.

- **All Sounds** shows every sound with its total.
- **Ungrouped** shows sounds without a group.
- The groups follow under the label **Categories**. Each group shows its sound count. The counts follow the current search and tag filter.
- Press the **+** icon in the rail header (tooltip `New group`) to create a group. The new group becomes the selected one.
- Hover over a group to reveal an edit icon (tooltip `Rename group`) and a ✕ icon (tooltip `Delete group`) in place of its count.

Group names are trimmed, limited to 32 characters and unique regardless of case. The dialogs say `Up to 32 characters`. A refused name shows `Enter a group name`, `Maximum 32 characters` or `A group with this name already exists`.

| Action | Dialog | Button | Effect |
|---|---|---|---|
| Create | **New Group** | **Create** | Adds an empty group |
| Rename | **Rename Group** | **Save** | Sounds stay where they are |
| Delete | **Delete Group** | **Delete** (or **Keep** to back out) | Sounds are not deleted. They stay in the library and become ungrouped. The subtitle `This cannot be undone` refers to the group |

To move a sound to another group, click the group name on its row. The **Move Sound** dialog (`Assign to`) lists `Ungrouped` and every group. Click one to assign the sound. If there are no groups, the dialog shows `No groups yet`.

When the window is narrow, the rail narrows and can be hidden. The rows keep working.

## Tags and search

The search field has the placeholder `Search name or tag…`. An icon button (tooltip `Clear search`) appears next to the text.

- A search term matches (as a part of the text) the sound's name, its file name, or any of its tags.
- Matching is case-insensitive for all Unicode letters. Accents are not ignored: `echo` does not find `écho`.
- Several words must all match (AND).
- The search works inside the selected group.

Tags are shown as chips under the search row. Each chip shows the number of sounds with that tag under the current filter. Click a chip to add it to the filter; click it again to switch it off. Sounds must have every selected tag. Selected chips come first, then the most used tags. At most 14 chips are shown, followed by `+N more tags`.

When a filter is active, the line `Showing N of M sounds` appears (M is the whole library). **Clear** removes the search and the tag selection. If nothing matches, the panel shows `NO MATCHES` and **Clear Filters**.

### Editing tags

Click the tags text on a row (tooltip `Edit tags`; the row shows up to four tags as `#tag` and then `+N`). On a row without tags, hover to show `+ TAG`. A row flagged `File not found` shows `LOCATE…` in that place instead. The **Edit Tags** dialog has:

- `Tags on this sound`, with a `Remove this tag` button on each tag;
- `Add tags (separate with commas)`, with an **Add** button (Enter works too);
- `Already used`, which suggests up to 8 existing tags that match what you type and that the sound does not have yet;
- **Done**.

Tags are trimmed, stored in lower case, limited to 32 characters and kept without duplicates. A tag longer than 32 characters is refused with `"…" is too long (at most 32 characters)` and nothing from that input is added.

## Missing files

A sound whose file is gone stays in the library. Its row shows the amber badge `File not found`, an amber border and a `LOCATE…` link (tooltip `Pick the file's new location`). A sound that could not be loaded at all shows a shorter row with the same badge, the path and a **Locate…** button. The sound keeps all its references in scenes, playlists and hotkeys.

Above the list, an amber banner shows `N files not found` with two buttons:

- **Replace Path Prefix…** (use it when a whole folder moved);
- **Recheck** (tooltip `Look for the files again`).

The library also re-checks file presence in the background about every 20 seconds. A file counts as present when it exists as a file at its stored path.

> **Note:** The check only updates the not-found flags. A sound that could not be loaded at startup because its drive was missing is loaded again after **Locate…**, **Replace Path Prefix…** or a bundle import, or when Bardwave is restarted. Whether **Recheck** alone brings such a sound back after you mount the drive is not verified; the code requests a reload only after those three actions.

### Locate

Press **Locate…** on the row or the `LOCATE…` link. The chooser is titled `Locate the sound file` and starts in the nearest folder that still exists above the old path. The chosen file replaces the old path (`Relinked "name"`). The sound keeps its id, so scenes and hotkeys still work. If the new file is already used by another sound, the library refuses with `That file is already used by another sound in the library`.

### Remove

The ✕ icon on a missing row (tooltip `Remove from the library`) deletes the sound. It also removes the sound from scenes, emitters, playlists and hotkeys, and tells how many references were removed.

### Replace Path Prefix

Use this when the whole sound folder moved, for example from `/Volumes/Old/Sounds` to `/Volumes/New/Sounds`.

1. Press **Replace Path Prefix…**. The old start is pre-filled with the folder that the missing files share.
2. Check or type the old start of the path in **Old start of the path**.
3. Type the new start in **New start of the path**. Use **Browse…** next to this field to pick the new folder.
4. The dialog shows how many paths will change (`N paths will change`), how many of them are not found right now, and up to three examples. If none match, it shows `No sound path starts with that prefix.` and **Replace** is disabled.
5. Press **Replace**. A message `Rewrote N paths` appears and the files are checked again.

Matching works on whole path components, so `/music` does not match `/music2/a.wav`. Both `/` and `\` are accepted as separators, and separators are converted when the two prefixes use different styles. The paths are rewritten even when the files do not exist at the new place. A rewrite that would make two sounds share one path is left out.

## Bundles

A bundle is a `.zip` file that carries a sound set to another Bardwave installation. It contains `bundle.toml` and, optionally, the audio files under `audio/`.

### What a bundle contains

| Included | Not included |
|---|---|
| Groups | Hotkeys |
| Sounds: name, group, volume, repeat, tags, gain, bus, stinger flag, pan | Audio device settings (output and preview devices) |
| Scenes and playlists | Normalization and ducking settings |
| Audio files, only when **Include the audio files** is ticked | Profiles |

An export always covers the whole library, not only the sounds you see under a filter. A bundle with no audio shows its sounds as `File not found` until you locate the files.

Sounds are stored under a path relative to the folder that all exported sounds share (so `~/Music/Tavern/a.ogg` and `~/Music/Fx/b.wav` become `Tavern/a.ogg` and `Fx/b.wav`). When the sounds share no folder, only the file names are kept. Characters that are illegal on some systems are replaced.

Example of a minimal manifest (`bundle.toml`):

```toml
format = "bardwave-bundle"
version = 1
name = "Tavern pack"

[[groups]]
id = "3f1c9a52-0d5e-4b8e-9a61-2c7d8e9f0a11"
name = "Tavern"

[[sounds]]
id = "8b2e4d10-7a3c-4f5e-b6d9-1e2f3a4b5c6d"
name = "Crowd"
rel_path = "Tavern/crowd.ogg"
audio = true
volume = 0.8
looped = true
group = "3f1c9a52-0d5e-4b8e-9a61-2c7d8e9f0a11"
tags = ["tavern"]
```

### Export

1. Press **Export…** in the search row (tooltip `Save the library as a shareable .zip bundle`).
2. In the **Export Bundle** dialog, set **Bundle name** (default `Bardwave set`).
3. Leave **Include the audio files** ticked to include the audio, or untick it for a small bundle that carries only the library data.
4. Press **Choose File & Export…**. The save dialog suggests `<name>.zip`; `.zip` is added if you leave it out. The button is disabled when the library is empty.
5. Use **Cancel Export** while it runs (Esc does not stop it). A cancelled export writes nothing and returns to the options. When it finishes, the dialog reports the number of sounds and audio files exported with their size. Press **Done**.

A sound whose file is missing is exported without audio, and the dialog warns about it before and after the export. The archive is written to a temporary file next to the destination and renamed when complete, so a failed export never leaves a broken bundle. An existing file with the same name is replaced.

### Import

1. Press **Import…** and choose a `.zip` in the chooser titled `Choose a bundle to import`.
2. The **Import Bundle** dialog checks the bundle. It shows the bundle name, the counts (`N sounds, N groups, N scenes, N playlists`, with the Bardwave version that made it) and whether audio is inside (`N audio files inside`, with the size) or not.
3. Choose what happens to existing items under **If something already exists**:

| Option | Policy | Behaviour |
|---|---|---|
| **Keep mine** | Skip (default) | Your existing items are kept. The bundle's copies are not imported, and scenes and playlists from the bundle point at your items instead. A sound whose only conflict is an existing file in the target folder is registered at that file, which is not overwritten |
| **Keep both** | Rename | The imported copy gets a free name, such as `Tavern (2)` (a file becomes `hit (2).wav`), and a new id if its id is taken. Importing the same bundle twice creates copies |
| **Use the bundle's** | Replace | The bundle's content replaces yours. The item keeps its own id, so hotkeys and scenes stay valid. Existing audio files in the target folder are overwritten |

4. Set **Sound files go to**, or press **Browse…** (the chooser is titled `Choose the folder for the sounds`). By default the folder is `bardwave-library/imported/<bundle name>` inside the data directory.
5. The dialog shows **This import would**: the new, renamed, replaced and skipped items, and the audio files to write. This preview is a dry run and changes nothing. It updates when you change the policy and when you leave the folder field.
6. Press **Import** (it is disabled while the folder field is empty or the preview shows an error). **Cancel Import** is available while it runs. Press **Done** at the end. A message `Bundle imported` appears under the search row.

Items conflict when they share an id, or when a group, scene or playlist has the same name (case-insensitive), or when a sound has the same path or, for a sound with audio, the target file already exists.

The library changes only after all files are written. If the import fails or is cancelled, the files it created are removed. A file overwritten under **Use the bundle's** cannot be restored.

> **Warning:** **Use the bundle's** overwrites existing audio files in the target folder. Back up that folder first if its files matter to you.

### Safety limits

Bundles may come from anyone, so the importer treats them as untrusted.

- An entry with an absolute path, `..` or `.`, an empty part, a backslash, a colon, a control character or a symbolic link rejects the whole archive. The same strict check applies to the sound paths written inside `bundle.toml`. The message starts with `Import blocked for safety`, and nothing is changed.
- Sound files are written only inside the target folder (the real location of each folder is checked, too). Only `wav`, `flac`, `mp3`, `ogg`, `m4a` and `aac` files are written. Other files are skipped with a warning, and the sound is imported without audio.
- Size limits, checked against both the declared sizes and the bytes actually read. A bundle over a limit is refused with the same `Import blocked for safety` message:

| Limit | Default |
|---|---|
| Entries in the archive | 20,000 |
| Groups, sounds, scenes and playlists together | 100,000 |
| Uncompressed `bundle.toml` | 16 MiB |
| Uncompressed size of one audio file | 2 GiB |
| Total uncompressed size | 8 GiB |

The panel uses these defaults. They are not set in the GUI.

A zip without `bundle.toml` is refused with `This zip is not a Bardwave bundle`. A bundle from a newer format version is refused with a request to update Bardwave.

## Headphone preview

Each row has a headphones button. Press it to play the sound on this computer only. Players do not hear it. Press it again to stop. Pressing the button of another sound switches the preview to that sound.

- The tooltip says `Preview in your headphones (players do not hear it)`.
- For missing files the button is not available. Its tooltip says `Preview is not available for this sound`.
- The tooltip on an active preview says `Stop preview`.
- A preview plays once (it does not repeat), at the volume of the sound including its loudness gain. It ignores the sound's pan and bus.
- **Stop All** stops the preview too, and so do the other stop-all paths (hotkey, timer, remote control).
- If the desktop audio output is not running, a message says that the preview plays on this computer only and names the output state.

The preview output is set in the **AUDIO OUTPUT** section of the right-hand column, in the **Preview** dropdown. The default entry is `Same as output`. In the config, the key is `preview_device` in `[audio]`. Previews never enter the players' stream.

> **Note:** Routing previews to a separate sound card has not been verified on real hardware.

## Loudness normalization

Normalization measures each sound once with EBU R128 and stores a fixed gain that brings it to a target loudness. It is off by default, so existing libraries sound as before.

Turn it on in the **MIX** section of the right-hand column (with the network controls), with the checkbox **NORMALIZE LOUDNESS**. The **Target** field sets the target in LUFS; it accepts -36 to -8 LUFS.

The status line reads one of:

- `Off: sounds keep their own level`
- `On: new sounds are measured and set to -18 LUFS` (the number is your target)
- `Measuring N sounds...`

How it works:

- Integrated loudness and true peak are measured on the decoded audio, on a background thread. Sounds shorter than 400 ms and silent sounds are not measured.
- The gain is limited to -24 dB to +12 dB. A boost is limited so the true peak stays under -1 dBTP.
- The gain is stored in the library as `gain_db`. The sound's level is `volume × 10^(gain_db / 20)`, capped at 1.0. Your volume setting is not rewritten.
- Because the level is capped at 1.0, a positive gain is heard only when the volume is below 1.0. Most loud material needs a negative gain.
- With normalization on, every loaded sound that has no gain yet (`gain_db = 0.0`) is measured, and so is every sound you add later. A sound that has a stored gain is not measured again.
- Changing the target does not re-measure sounds that already have a gain. To measure such a sound again, set its `gain_db` back to `0.0` in `config.toml` and restart (not verified).
- Turning normalization off keeps the gains already stored, and they keep being applied.
- Streamed files (see below) are not measured.

## Streaming long files

A file longer than `stream_threshold_secs` (default 60 seconds) is not loaded into memory. It is decoded on a worker thread and played from a ring buffer of about 3 seconds. This keeps memory use low: an hour of stereo audio would take about 1.4 GB in memory.

- Short files stay in memory, so they start instantly.
- If the worker falls behind, the audio plays silence for the missing part and an underrun is counted. The audio thread does not stop.
- Repeat works with streamed files. The streaming source also supports seeking; the library rows have no seek control.
- Headphone preview works with streamed files.
- Set `stream_threshold_secs = 0` to disable streaming.

When the duration is not known from the file header (some MP3 files), the file size decides: a file bigger than about 16 kB per second of threshold is streamed.

## Configuration reference

Library data lives under `[[library.groups]]` and `[[library.sounds]]`. Audio settings live under `[audio]`. The values below are the defaults from the code. For every other key, see the [configuration reference](./configuration.md).

```toml
[audio]
normalize = false              # measure sounds without a gain_db and store one
normalize_target_lufs = -18.0  # target loudness, in LUFS
stream_threshold_secs = 60     # longer files are streamed; 0 = never
# preview_device = "Headphones"  # absent = previews use the main output

[[library.groups]]
id = "3f1c9a52-0d5e-4b8e-9a61-2c7d8e9f0a11"
name = "Tavern"

[[library.sounds]]
id = "8b2e4d10-7a3c-4f5e-b6d9-1e2f3a4b5c6d"
path = "/Users/me/Music/Tavern/crowd.ogg"
name = "Crowd"
volume = 0.8                   # 0.0 to 1.0
looped = true
group = "3f1c9a52-0d5e-4b8e-9a61-2c7d8e9f0a11"
tags = ["tavern", "crowd"]
pan = -0.25                    # -1.0 (left) to 1.0 (right)
gain_db = 0.0                  # written by normalization
bus = "ambience"               # "music", "ambience" or "sfx"
oneshot_stinger = false
```

| Key | Section | Default | Range or meaning |
|---|---|---|---|
| `normalize` | `[audio]` | `false` | Turns normalization on |
| `normalize_target_lufs` | `[audio]` | `-18.0` | Target loudness. The **Target** field accepts -36 to -8 LUFS |
| `stream_threshold_secs` | `[audio]` | `60` | Seconds; `0` disables streaming |
| `preview_device` | `[audio]` | absent | Device name for previews. Absent means the main output |
| `id` | `[[library.sounds]]` | generated | Stable id. Generated when missing |
| `path` | `[[library.sounds]]` | required | Path to the audio file |
| `name` | `[[library.sounds]]` | file name | Display name. Falls back to the file name when empty |
| `volume` | `[[library.sounds]]` | `1.0` | `0.0` to `1.0` |
| `looped` | `[[library.sounds]]` | `false` | Repeat |
| `group` | `[[library.sounds]]` | absent | Group id |
| `tags` | `[[library.sounds]]` | `[]` | Lower case, up to 32 characters |
| `pan` | `[[library.sounds]]` | `0.0` | `-1.0` to `1.0`; not written when centred |
| `gain_db` | `[[library.sounds]]` | `0.0` | Written by normalization. Values are limited to -48 to +24 dB when the library is loaded |
| `bus` | `[[library.sounds]]` | `"ambience"` | `"music"`, `"ambience"` or `"sfx"` |
| `oneshot_stinger` | `[[library.sounds]]` | `false` | Plays as a one-shot over the scene, without stopping other sounds |

The sound-level `pan` and `volume` values are the ones the GUI writes. The config file is only the storage; use the panel to change them.

For remote access to the players' page, see [HTTPS](./https.md). For more on YouTube download problems, missing files and bundle import security, see [Troubleshooting, FAQ and security](./troubleshooting-security.md).

## Troubleshooting

| Symptom | Likely cause | What to do |
|---|---|---|
| Amber banner `N files not found` | Files moved, or a drive is not mounted | Mount the drive and restart Bardwave (see the note in [Missing files](#missing-files)). For a moved folder, use **Replace Path Prefix…**. For single files, press **Locate…** |
| Sound shows `File not found` straight after import | The bundle had no audio | Press **Locate…** on each row, or import a bundle exported with audio |
| `Not a YouTube video link` | Playlist page, channel or other site | Paste a single video link |
| `ffmpeg not found on this system.` (macOS, Linux) | ffmpeg is not installed | Install ffmpeg with your package manager, then retry |
| YouTube download is slow the first time | yt-dlp and ffmpeg are being downloaded | Wait. Later downloads reuse the tools |
| The **YouTube** button does nothing | A download is still running (one at a time) | Wait until it finishes |
| A file was added but no row appears | The decoder could not read it (for example `.opus` or `.m4a`) | Look for `Failed to load sound` in the log. Convert the file to WAV, FLAC, MP3 or OGG |
| `That file is not a supported audio file` | Wrong extension | Choose a file with a supported extension |
| `That file is already used by another sound in the library` | The chosen file belongs to another entry | Remove the duplicate entry first |
| `No sound path starts with that prefix.` | Old prefix does not match stored paths | Copy the exact start of a path from a sound's tooltip |
| `Import blocked for safety` | The archive has an unsafe entry, or is bigger than the size limits | Do not import it. Ask the sender for a fresh export |
| Preview is not available | The file is missing | Locate the file first |
| Normalization changes nothing for a long file | The file is streamed, so it is not measured | Raise `stream_threshold_secs` if the file must be measured |
| A boosted sound is not louder | Level is capped at 1.0 | Lower the volume first, or accept a negative gain |
| `NO MATCHES` | Search or tag filter excludes everything | Press **Clear Filters** |
