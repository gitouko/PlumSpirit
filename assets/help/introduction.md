# Introduction

PlumSpirit is a MIDI sequencer and editor built for Black MIDI: songs with
thousands to hundreds of millions of notes. It is made to stay fast and small enough
for low-end Windows PCs, while giving you exact control over every note, tempo and
channel.

## The window

* **Menu bar** — File, Edit, Tools, Plugins, Song, View, Playback and Help.
  Every menu entry shows its keyboard shortcut.
* **Toolbar** — the **Track List / Piano Roll** switch, the note tools (Select, Draw,
  Erase), Settings, Mute / Solo, Undo / Redo, Snap, the zoom fields and Onion Skin (its
  **▾** holds every Onion option). Hover over a control for what it does.
* **Track panel** (left) — the Conductor, your tracks, subtracks and folders, with
  Mute and Solo.
* **The central area** — the **Track List** (every track's clips on a timeline) or the
  **Piano Roll** (the notes of one track or clip). {key:SwitchView} switches between
  them.
* **Transport bar and status bar** (bottom) — play, stop, position, tempo, and short
  messages about what just happened.

## How a song is organized

A project has exactly one **Conductor** (tempo, time signatures, markers — tempo is
global, never per track), any number of **tracks** (which can have **subtracks** and be
grouped in **folders**), and on each track **clips** that place notes on the song's
timeline. Each note keeps its own MIDI channel; a track is not a channel. Notes can also
be grouped into **subchannels**: colored note groups inside a track. See the
[Glossary](#glossary) for every term.

## Where to go next

* New here? Follow the [Quick Start](#quick-start): from an empty project to an
  exported MIDI file in nine steps.
* Looking for a key? See [Keyboard Shortcuts](#shortcuts).
* Every window with a **?** button opens the Help page about it; {key:ShowHelp} opens
  the page for what you are doing now.
