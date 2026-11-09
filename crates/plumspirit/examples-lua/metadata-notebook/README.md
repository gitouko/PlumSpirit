# Metadata Notebook

Install this folder through Plugins → Lua Plugins → Install, then grant Project Read
and Metadata Edit. Open a project and run Remember Project Attributes from its Plugins
menu. It stores author-written points, a visit count, project name/key rows and an exact
large integer in its own namespace. Inspect prints the current data; Clear removes it.
Each write/removal has one native Undo step; Redo restores it. Save as LUP and reopen
to retain the attributes. MIDI export remains unchanged and carries no notebook.

Schema1 (`old_visits`) is explicitly migrated by this Lua example to schema2 (`visits`)
inside the same transaction, guarded by expect_version. A newer schema is refused and
left untouched. The host does not invent migrations. Removing/reinstalling the plugin
does not delete its project attributes. Other plugins cannot read or overwrite them.
Remember/Clear First Track Attributes demonstrate the same typed data on stable Track
and Subtrack handles. Track moves/renames retain attributes; native deletion removes
them, and Undo restores them with the Track. Independent copies start without attributes.
Remember/Clear First Group Attributes annotate an existing permanent native Group.
Writes respect its and its ancestors' locks; temporary Groups are refused. Linked
Groups keep independent metadata. Native deletion/temporary status clears attributes
with Undo restoration. Clip/Subchannel/per-note attributes remain unsupported. No
IDs are added to notes. The stored points are example data, not a graph UI.

The native headless App integration test installs this actual package, applies Remember,
Undo/Redo, saves through the real background LUP worker, reopens and applies Clear.
This differs from real Windows pointer/dialog acceptance and final release testing.


This package is also embedded in **Plugins > Lua Plugins > Examples**; no download
or source checkout is needed. Installation starts disabled with no capabilities
granted by default. Review the listed permissions and enable it using the normal
plugin controls. Reinstall restores the shipped files and preserves your decisions.
