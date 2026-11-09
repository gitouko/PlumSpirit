# Composable Services Laboratory

Install both `note-service-provider` and `service-composer` folders in Lua Plugins.
Grant the provider **services.provide** and enable it. Grant the laboratory Project
read, Note edits and **services.call**, then enable it. Dependencies never install,
enable or grant permissions automatically.

Open a project and run Generate Composed Service Pattern: the provider's explicit Lua
algorithm returns16 deterministic notes from seed12345, committed to the first Track
as one Undo step. Undo/Redo restores the staged result. The second command transposes
up to2,048 selected notes on the first Track by7 semitones, clamps to the project's
128/256 key range and preserves supported packed note metadata. Existing native group
locks apply. All calls must succeed before the transaction stages any change.

The provider has no project World or UI/settings/file-prompt authority. Only validated
plain data crosses the worker boundary. Missing/disabled providers, incompatible
versions, undeclared dependencies, cancellation and quota/time-limit failures produce
errors without partially committed notes. Caller/provider capability intersection is
native. A provider needs services.call plus its own dependencies to call another one;
same-worker and nested/cross-root waiting cycles are refused.

This bounded example has an intentional2,048-note selection limit (64 calls of32
notes). It is a composition demonstration, not a bulk Black MIDI processing benchmark.
The host does not invent either pattern or transformation algorithm. Native integration
tests install both packages and verify the generated result and Undo; supported-target
interactive UI and larger workload acceptance remain release gates.


This package is also embedded in **Plugins > Lua Plugins > Examples**; no download
or source checkout is needed. Installation starts disabled with no capabilities
granted by default. Review the listed permissions and enable it using the normal
plugin controls. Reinstall restores the shipped files and preserves your decisions.
