# Playback, SoundFonts and MIDI Output

## The transport

| Action | How |
|---|---|
| Play / pause | {cmd:PlayPause} |
| Stop (back to where playback started) | {cmd:Stop} |
| Beginning / end | {cmd:GoToStart} / {cmd:GoToEnd} |
| Previous / next measure | {cmd:PreviousMeasure} / {cmd:NextMeasure} |
| Move the playhead | Click the ruler |
| Loop | {cmd:ToggleLoop}; Shift+drag on the ruler sets the range; {cmd:LoopToSelection} |
| Speed | The transport's Speed field: any positive factor (only how fast PlumSpirit plays — never the tempo or the export) |

{cmd:ToggleAnchorPlayhead} adds a second, stationary playhead that only you move: Play starts
there and Stop returns to it. **Playback → After Pause** chooses whether Pause holds the
place or returns to it. {cmd:ScrubTool} plays the selection or scrubs.

Pausing ends every sound. Resuming does not strike held notes a second time (Audio Settings
can bring the re-strike back); moving the playhead still sounds the notes held at the new
place. Edits made while playing are heard within a fraction of a second.

## Where the sound goes

The **output chip** in the transport row (also Audio Settings → MIDI output) chooses where
tracks play:

* **Internal** — PlumSpirit's SoundFont synthesizer.
* **Windows MIDI devices** — any installed MIDI output.
* **OmniMIDI KDMAPI** — a fast direct connection to OmniMIDI, for very dense songs.

An output that cannot be opened or stops working falls back to the internal synthesizer
with a notice. Track Properties can route a single track to another output.

## SoundFonts

{cmd:ChooseSoundFont} picks an `.sf2` for the project (it also becomes the default). Without
a choice PlumSpirit plays the bundled GeneralUser GS. A project whose SoundFont is missing
still opens and plays with the default.

## Audio Settings

Playback → **Audio Settings…** (also Settings → Playback & External MIDI → Audio Settings…):

* **Output** — device, sample rate and buffer size, with the latency shown.
* **Render mode** — **Realtime** (low latency for editing) or **Pre-Render** (renders ahead
  so every note starts on time, even where a very dense passage takes longer than real time
  to render). MIDI outputs (OmniMIDI / KDMAPI, devices) use their own polyphony.
* **Voices** — up to 4,096. In Realtime, notes beyond a block's budget start a few
  milliseconds later instead of being lost; only notes more than 50 ms late are dropped, and
  counted. Pre-Render has no such budget. **Voice policy** defaults to RustySynth's quietest
  voice first. At a shard's held-note bound, **Keep sounding notes** refuses new notes;
  **Release oldest held key first** sends a Note Off for that key in the shard, including
  its repeated instances. These choices also apply in Pre-Render and are counted in
  Diagnostics. Held notes are a lower bound on actual voices: layered presets, sustain
  and release tails still use RustySynth's internal stealing. Its actual voice and steal
  counts are unavailable. External MIDI outputs manage their own voices.
* **Headroom** — master gain, a safety limiter, peak meters and a clip counter.
* **Event processing (playback only)** — not destructive: these change only what is sent to
  the synthesizer and the MIDI outputs while playing, never the project, the saved file, the
  exported MIDI, the MIDI Player or the Inspector. In this order: **1** only the 88 piano keys
  (21 – 108; the drum channel keeps every key); **2** ignore notes whose velocity is in a range
  (both ends included — for example 1 to 10 leaves out the quietest ghost notes); **3** every
  note lasts a set number of seconds; **4** every Note Off comes a set number of seconds later.
  A note that is not sent never gets a Note Off, so nothing sticks. While 3 or 4 is on a key
  plays one note at a time (a held key struck again ends the held note first). The song's
  **system resets** (GM / GS / XG) are not sent unless you choose **Sent** or **Sent, ignoring
  a repeat of the current mode** (that keeps the instruments set before a repeated reset — a
  song that counts on the reset may sound different). A line counts what each option would
  act on in the open song.
* **SoundFonts** — a list to add, order and pick from; reverb and chorus.
* **Diagnostics** — voices, late and dropped notes, underruns, the MIDI output's state;
  **Copy report** for a bug report.

No audio device? Everything else keeps working; Play tries again.
