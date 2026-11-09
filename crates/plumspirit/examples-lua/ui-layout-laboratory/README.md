# UI Layout Laboratory

Install this folder or use Plugins → Lua Plugins → Examples. Grant ui.panels and run
Open UI Layout Laboratory. No project, note-editing or file permission is required.

The Plot tab uses an explicitly authored sine function and an independently draggable
handle. Data contains512options; the native list renders only its viewport rows and
returns a1-based selection. Switching tabs retains the author's selection/plot model.
The disabled tab cannot be selected. Close laboratory closes normally; window X cancels.
No project data is created or modified.

The example publishes its larger list only when Data opens, preserves selection in Lua,
and bounds plot update rate to20Hz. Original coordinates remain authored state rather
than screen pixels. The native host bounds snapshots, rows, events and memory, and
cancels pointer capture when a canvas is hidden. Lists/trees are single-select; image resources are demonstrated separately.
Font selector widgets and editor overlays remain separate required work.

The Controls tab adds an author-defined hierarchical catalog, independent expansion and
selection, a native RGBA picker and a four-corner triangulated gradient. The disabled
item cannot be clicked. Expanding/selecting rows delivers typed events to the Lua worker;
Lua updates its own model before publishing another snapshot. Tree controls virtualize
visible rows, with512items/depth8 maximum across a snapshot. The demo catalog is small.

Save controls/Reset controls/Load controls persist and restore a version1 plain preset
containing color bytes, catalog selection and branch expansion. No resource tokens or
musical data are stored. The author validates the schema before restoring. RGBA is UI
appearance; it does not create unsupported note colors. Changing the Controls color also
changes the Plot handle through the author's explicit model.
