# System requirements and performance notes

**Platform for v1.0:** Windows 10/11 x64. Windows 11 is the owner's principal test environment. Linux, Android and macOS are future plans, not supported v1.0 downloads.

| Component | Practical starting point (not a certified minimum) | Recommended for larger projects |
|---|---|---|
| CPU | x64 dual-core | Modern quad-core or higher |
| RAM | 4 GB | 16 GB+, with 32–64 GB helpful for very large projects |
| GPU | Functional OpenGL support | Recent integrated/dedicated GPU with current drivers; Vulkan optional |
| Disk | ~1 GB free plus room for projects | SSD with significant free space for massive MIDIs and caches |
| Audio | Windows audio device; SF2 for internal sound | SoundFont configuration suited to your hardware; external MIDI optional |

The program was developed on a Dell G16 7630 with Intel Core i9-13900HX, RTX 4070 Laptop GPU, 64 GB RAM, 1 TB NVMe SSD and Windows 11. Those components are **not** the minimum.

PlumSpirit's aim includes useful low-end performance, including 2 GB-class computers, but this has not been certified as a safe Windows 10/11 installation minimum. Gigantic Black MIDI files can demand far more RAM and CPU than small files. Actual outcomes depend on visible density, file structure, synth voices, caching, and other applications running.

The repository contains measured benchmark data in `docs/PERFORMANCE.md` and versioned evidence, but measurements from particular machines must not be presented as guarantees for all users.
