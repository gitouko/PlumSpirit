# PlumSpirit

![PlumSpirit logo](assets/brand/wordmark_light_288.png)

**A free, portable MIDI sequencer made specifically for Black MIDI production.**

## Before anything else, how PlumSpirit was made

I want to make this clear from the beginning instead of having people find out about it later. **PlumSpirit was vibe-coded.** AI was used to help write and develop the program, and I'm not going to hide that or pretend the entire codebase was handwritten by me.

But at the same time, PlumSpirit wasn't something I just thought of one afternoon and asked an AI to finish in a single prompt. I spent **33 days over a month** getting it to v1.0, putting dedicated hours into planning the features, making private lists of things I wanted changed, reviewing the development, trying the builds myself, looking through other Black MIDI software for ideas, and working through the problems I kept finding. **This isn't meant to be a flex.** I just want people to know where the project came from and how much I wanted to see it become a real program.

I've tried to get as many bugs, glitches and conflicts fixed as possible, especially the ones that would make an editor frustrating to use in the first place. That doesn't mean I can guarantee everything is perfect. There can absolutely still be problems I haven't found yet, especially with the extremely large MIDIs people in this community make. That's one of the reasons I'm releasing it: so people can try it for themselves and tell me what I should improve next.

Please judge PlumSpirit by how it works for you. If something goes wrong, report it. If there's something it can do better, I'm willing to hear about it.

## What is PlumSpirit?

PlumSpirit is an original MIDI sequencer and editor focused on Black MIDI. I wanted a program where the whole point is working with huge amounts of notes, note art, unusually dense patterns, and experimental transformations, rather than being another general music DAW with a few Black MIDI features attached.

It has a Track List and Piano Roll, so you can build a MIDI from scratch, edit existing files, organize the song, preview it, and export it when you're finished. There's a lot more hidden under the tools, but basic editing shouldn't require learning every advanced feature first.

## Some of the features

- **Piano Roll and Track List editing.** Create Tracks, Subtracks, Clips and logical Subchannels, arrange your notes, and work with entire MIDI projects instead of only one track at a time.
- **An experimental Piano Roll tool collection.** Things like 3D transformations, Warp Transform, Trails, Tiny Planet, Ripple, Displacement Map, Tumours, note outlines, and more. These aren't meant to be ordinary DAW effects pasted onto MIDI. They're ways to manipulate note positions and patterns directly.
- **Note generators.** Draw lines, rectangles, circles, curves and polygons, or go further with custom shapes, equations, fractal/L-system generation, spirograph-style geometry, and procedural growth.
- **Advanced functions and custom curves.** Many tools have their own extra controls, so even tools that seem similar at first can give completely different results once you use their advanced features.
- **Preview before applying changes.** Try a manipulation before committing it. Undo and Redo are there when you need to go back.
- **Onion Skin and note coloring.** Compare notes from other Tracks/Subchannels without permanently merging everything together.
- **Built-in MIDI Player and MIDI Inspector.** View your music as it plays and examine note counts, NPS, polyphony, timing, and other statistics.
- **Tempo changes, time signatures, markers, and MIDI events.** Work with the information that actually makes a MIDI project play correctly.
- **SoundFont playback and Windows MIDI output.** Use an SF2 SoundFont internally or an available external MIDI output, including OmniMIDI/KDMAPI where it is installed and supported.
- **Lua custom tools and plugins.** The public scripting API lets people make their own tools for PlumSpirit. [The API documentation is here](docs/LUA_API.md), with [working example plugin folders](examples-lua/).
- **Large MIDI handling.** The editor has specialized note storage, indexed analysis and dense rendering paths for very demanding projects. Actual performance still depends on the file, settings and hardware.

I know there are some Piano Roll tools that may look like they have a similar purpose. That is intentional in some cases. **The important difference is what each one offers in its own advanced controls.** Try those before deciding two tools do exactly the same thing.

I don't want to claim that every other MIDI editor is missing every one of these ideas. What matters to me is having this particular combination of tools centered around Black MIDI production.

## Download  no building required

Go to the **GitHub Releases (on the official PlumSpirit repository)** page and download the latest **Windows x64 Portable** ZIP. Extract the entire folder somewhere you can keep it, and run `PlumSpirit.exe`.

There is no installer. Don't move the EXE away from its accompanying files unless you're intentionally setting up a custom installation. You **do not need Rust, Visual Studio or any developer tools** to use the portable release.

**SoundFont included:** This Windows ZIP contains **GeneralUser GS v2.0.3** (`SoundFonts/GeneralUser-GS.sf2`), alongside the author's license. PlumSpirit looks for it automatically next to the EXE. You can also select a different `.sf2` in audio settings. External MIDI output such as OmniMIDI requires separate setup on Windows.

Download the official GitHub Release asset rather than an unofficial EXE. Keep the extracted `SoundFonts` folder next to `PlumSpirit.exe`.

## What you'll need

| | Minimum starting point* | Recommended |
|---|---|---|
| Operating system | Windows 10/11, 64-bit | Windows 11, 64-bit |
| CPU | x64 dual-core processor | Modern quad-core or better |
| RAM | 4 GB | 16 GB or more for demanding Black MIDI projects |
| Graphics | Integrated or dedicated GPU with working OpenGL support | Recent GPU with reliable OpenGL drivers; Vulkan is optional |
| Storage | Around 1 GB available for the program and smaller projects | SSD and additional free space for large MIDI/project files |
| Audio | Working Windows audio output + SF2 for internal playback | SoundFont/audio setup suited to your workload; external MIDI output optional |

\*These are **practical starting estimates, not independently certified minimum specifications**. The longer-term goal is to make PlumSpirit useful on very low-end PCs, including machines with about 2 GB RAM, but I cannot promise that every project or Windows configuration will work within that amount. Files with tens or hundreds of millions of notes may require significantly more memory. Windows 11 is the main development/playtest environment.

For reference, my own development/playtest system is a Dell G16 7630 with an Intel Core i9-13900HX, RTX 4070 Laptop GPU, 64 GB RAM and Windows 11. Those are **not** requirements to use PlumSpirit.

## Making Black MIDI with PlumSpirit

**You don't need to credit PlumSpirit** in MIDIs, videos, renders or other music you create with it. Use it to make your own Black MIDI, share your projects, and do whatever you want creatively. PlumSpirit doesn't claim ownership of your original compositions or MIDI files.

Of course, if you used somebody else's song, arrangement, samples or other material, their rights still apply. Using PlumSpirit doesn't automatically give you permission to use third-party music.

## Plugins and the public API

PlumSpirit supports Lua-based community tools. Start with [the Lua API reference](docs/LUA_API.md), which covers API versions, permissions, notes, events, Tracks, Clips, selections, transactions, graphs, custom tools, and example plugins.

There are also examples under [`crates/plumspirit/examples-lua`](examples-lua). Keep an eye on API version compatibility as the program changes.

Only install scripts from sources you trust, and read the permissions a plugin requests.

## Bugs, feedback, and future updates

If the program crashes, misses notes, freezes, exports something incorrectly, or does something that just feels annoying, please open a bug report on the official PlumSpirit GitHub Issues page. Include the version, what you were doing, and how to reproduce it. A screenshot or short recording can help a lot.

Please don't upload private MIDIs or personal files publicly unless you're comfortable sharing them. You can also post suggestions on that same GitHub Issues page, especially if a feature would actually remove a bottleneck for Black MIDI producers.

**Planned platforms:** Windows x64 is the focus for this release. Linux and Android are planned for later months. macOS is currently pushed toward **late 2027**. Those are plans rather than guaranteed release dates.

## Source code, forks and licensing

PlumSpirit's **original source code is released under the [MIT License](LICENSE)**. You're welcome to fork it, change it, experiment with it and make your own version. MIT is permissive, but when redistributing the licensed code, **you must retain the applicable copyright and license notice**. That requirement is separate from the no-credit-needed rule for music made using PlumSpirit.

Third-party libraries, the SoundFont, and other included material have their own notices and licenses. Please read [THIRD_PARTY_NOTICES.md](legal/THIRD_PARTY_NOTICES.md) and [THIRD_PARTY_LICENSES.txt](legal/THIRD_PARTY_LICENSES.txt). The PlumSpirit name/logo are also part of the project's branding; please make your fork's identity clear so people don't confuse it with an official release.

If you want to build the source, see [the build instructions](docs/BUILDING.md). Users who only want to make MIDIs can download the portable EXE instead.

Thanks to everyone in the Black MIDI community who gave feedback, tested things, explained technical problems, or suggested better ways to do something. That input made a difference.

> **Release artifact note:** This executable has passed ZIP integrity and static Windows PE checks, but its runtime behavior has **not** been independently playtested in Windows by the preparer. See `docs/RELEASE_VALIDATION.md`.
