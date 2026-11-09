# UI Image Laboratory

Install this folder or use the native Lua Plugins example installer. Grant ui.panels
and file.read_user_choice, then run Open UI Image Laboratory. The initial checker and
alpha gradient are explicitly authored Lua. Drag the marker; change opacity and UV crop.
Choose image opens the existing native dialog and decoder. A canceled dialog preserves
the original. At most eight dialogs are offered per invocation, matching the host limit.

The Image and file are closed after converting to an independent, bounded UI image.
The image is downsampled with the existing native box algorithm to a512side preview;
texture filtering is linear. Repeated panel updates reuse the same prepared pixels and
native texture. A checker background, texture, axes and marker demonstrate ordered
layers/clipping/transparency. The example creates no musical data, modifies no files,
and has no project read/edit authority. It stores no live resource in presets.

Close laboratory closes normally; the native X/Cancel cancels the job and releases
resources. Decoder/format/budget failures end this demonstration with the host's normal
categorized error. Reopen the command for a fresh job after its ten-minute time limit
or eight-dialog quota. Image resources are not permanent project objects or note colors.
