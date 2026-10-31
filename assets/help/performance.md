# Performance and Diagnostics

PlumSpirit is built for millions of notes on modest PCs. A few things help on a slow or
low-memory computer.

## Big projects

* The Piano Roll and the Track List never draw note by note when zoomed out: dense areas are
  drawn from pictures built in the background (a gray stand-in shows while they build;
  nothing is ever blank).
* **Settings → Rendering / Onion**: **Rendering Quality** (Fast draws fewer exact
  notes before the dense pictures take over), the **Tile Cache Budget** (0 = automatic: 24 MB
  on PCs with up to 3 GB of RAM) and **Preload Distance**.
* Settings → Rendering / Onion → **Renderer**: **OpenGL** (the default — light, works on older and integrated
  graphics) or **Vulkan** (with a choice of integrated or dedicated GPU). A change applies at
  the next start; if Vulkan cannot start, PlumSpirit falls back to OpenGL and says why.
* Undo never copies the project: edits are stored as small reversible steps within a memory
  budget, and older steps are kept on disk.
* Long jobs (opening, saving, exporting, big tools) run in the background with a progress
  bar and **Cancel**; the status bar says how long the last one took.
* Closing a huge project is immediate; its memory is freed in the background.
* A project takes about 20 bytes of memory per note (a 40-million-note file about 0.8 GB,
  a 300-million-note file about 6 GB). When a file you open would likely need more memory
  than is free (about 3.5 times the file's size), the status bar says so before it is read:
  close other programs, or cancel and open a smaller file. MIDI import, export and project
  saving use several processor cores on large files.
* If memory runs out while a file is being opened, PlumSpirit stops opening it before the
  computer runs out (**Not enough memory to open the file**): nothing is changed and the
  project that was open stays open. Closing that project first frees its memory.
* When a project opens with little memory left, the status bar says **memory nearly full**:
  PlumSpirit keeps working with a smaller Undo history. Save your work; very large edits or
  a second big file may not fit.

## Measuring

* {cmd:ToggleMetrics} shows PlumSpirit's CPU share and RAM in the status bar.
* {key:ToggleDevOverlay} shows the developer overlay: frame times, memory and audio
  diagnostics.
* {cmd:RecentTasks} lists how long the last operations took.
* Audio Settings → **Diagnostics** shows voices, late and dropped notes, underruns and the
  MIDI output's state, with **Copy report**.

## Reporting a problem

{cmd:SaveDiagnostics} writes one text file for a bug report: your system, the project's size
(without its content), audio diagnostics, settings and the end of the log. Nothing is sent
anywhere by PlumSpirit.

See also [Troubleshooting](#troubleshooting).
