# Glossary

| Term | Meaning |
|---|---|
| Black MIDI | Music with a very large number of notes — often millions — usually made to be watched as much as heard. |
| Project (`.lup`) | PlumSpirit's own file. It keeps everything exactly, including tempo ramps, clips, folders, subchannels and the undo history's place. |
| MIDI file (`.mid`) | A Standard MIDI File. PlumSpirit imports it and exports to it; Export checks the file before it replaces anything. |
| PPQN | Ticks per quarter note: the time resolution. A note's start and length are whole ticks. |
| Tick | The smallest step of time in a project (1 / PPQN of a quarter note). |
| Conductor | The single place for tempo, time signatures and markers. Tempo is global: there is no per-track tempo. |
| Tempo ramp | A tempo that changes smoothly to the next tempo point. MIDI has no ramps, so Export writes them as small steps. |
| Track | A row of the Track List with its own notes and events. A track is not a MIDI channel: each note keeps its own channel. |
| Subtrack | A track inside another track (indented under it). |
| Folder | A group of top-level tracks in the track panel. |
| Clip | A piece of a track's timeline that plays notes at a position in the song. Linked clips (∞) share their notes: editing one edits all. |
| Subchannel | A colored group of notes inside a track. It is not a MIDI channel; it only says which MIDI channel its new notes get. |
| Channel | One of the 16 MIDI channels (shown 1 – 16). Channel 10 is the General MIDI drum channel. |
| Velocity | How hard a note is played, 1 – 127. |
| Gate | A note's length in ticks. |
| CC (controller) | A MIDI control change such as Volume (7), Pan (10), Expression (11) or Sustain (64). |
| Edit cursor | Where pasting, adding clips and playback start. Click the ruler to move it. |
| Playhead | Where playback is now. |
| Time selection | A range of time selected by dragging along the ruler. |
| Snap | The grid that drawing, moving and resizing follow. |
| Onion Skin | Other tracks' notes drawn as faint ghosts behind the track you edit. |
| Preview | Many tools show their result in the Piano Roll before you apply it; Apply makes exactly that change, Cancel changes nothing. |
| Progressor | Live readout of notes played, notes per second (NPS) and polyphony at the playhead. |
| Selection Analytics | Exact statistics of the selected notes (count, span, peaks). |
| 256-Key Mode | A project mode with keys 0 – 255. A MIDI file holds only 0 – 127, so Export asks what to do with the rest. |
| SoundFont (`.sf2`) | A file of instrument sounds the built-in synthesizer plays. |
| KDMAPI | A fast MIDI output used by Black MIDI synthesizers such as OmniMIDI. |
| Lua plugin | A community script that adds commands or note tools. It runs in a sandbox and asks before it may read or change anything. |
