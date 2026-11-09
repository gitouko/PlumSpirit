# Font Resource Inspector

Install this folder through Plugins → Lua Plugins → Install. Grant user-selected file
read for the optional selected-font command, and `fonts.list` for the installed-font
command. Grant `ui.panels` for the optional native chooser. Bundled font math itself needs no project
or filesystem permission.

The first two author-written commands inspect either the app's bundled proportional and
monospace faces, or a user-selected TTF/OTF/TTC face0. Real metrics and scalar glyph IDs/
advances are saved as plain `last_report` plugin config data; a notification names the
resource. Missing glyphs remain marked missing. No font bytes, OS paths or live handles
are stored. Opening a file can be canceled without changing the report.

The third command enumerates bounded installed-font metadata, restores a plain saved
family/style choice, loads and inspects that face, and explicitly notifies when the
choice is unavailable in the current scan. It reports truncated inventories. It saves
no paths or live catalog indices. The test uses controlled roots with actual file IO;
a separate Windows probe uses actual host-installed fonts.

The fourth command opens an author-written native chooser with family/style search,
64-face pages, weight/italic/collection metadata and an explicit scalar outline preview.
It uses the public catalog, native list/panel/canvas and geometry services. The helper
`font-picker.lua` can be adapted by another plugin author; it runs only on the Lua
worker. No font scanner, UI input hook or renderer is duplicated.

Choose atomically saves one plain `font_choice` object with schema version1, family,
style, collection index and its metrics/coverage report. The three older inspection
commands retain their `last_report` behavior. Cancel/window close/failure before Choose
leaves those settings unchanged. Save/Load choice preset uses an independent named
plain preference; saving a preset is an explicit action and does not change the active
choice. Missing saved choices are reported, with no silently saved fallback.

Search accepts128UTF-8bytes, at most4scans;64page attempts/job and native scan limits
still apply. Reopen the command for a new allowance. At most31explicit preview loads
leave one native font load for Choose. Handles close immediately after use. Scalar
preview uses supported glyf contours, at most2048control/sample points and32contours;
missing/unsupported/over-budget preview is reported. The canvas fits X/Y independently;
it is an outline inspection, not shaped typography, color emoji or proportional text
layout. Installed shaping uses the separately documented isolated native service.

This resource example creates no notes and receives no project/transport authority.
Fonts influence actual returned metrics; scalar coverage is not grapheme shaping.
Do not bundle proprietary fonts. The bundled options use the app's existing licensed
egui assets. See docs/LUA_API.md for lifetimes, quotas and exact supported formats.
