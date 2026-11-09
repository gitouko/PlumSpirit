# Geometry Primitives

Install this folder through Plugins → Lua Plugins → Install and grant Project Read
and Notes Edit. The plugin adds six commands:

- Generate Author Path: an explicitly authored triangle, resampled by native arc length.
- Generate Author Bezier: explicitly authored control points sampled at32 parameter steps.
- Generate Author Mixed Path: authored line/cubic segments, a straight closing edge and
  subsequent arc-length resampling.
- Generate Author Adaptive Mask: tolerance-bounded native cubic flattening, then an
  authored polygon mask reflects pitches and distance from a guide controls velocity.
- Generate Author Adaptive Path: authored mixed lines/cubic, bounded adaptive flattening
  and arc-length resampling into musical positions.
- Generate Author Raster Cells: authored11×3polygon, native compact cell spans and
  author-chosen streaming notes at the top3project pitches.

Each creates33 notes on the first Track with one native Undo step; Redo restores it.
The Lua source defines every artistic choice: path/handles, orientation, timing, pitch
and velocity. The host supplies only bounded mathematical primitives. The test executes
all six real entry functions in128/256-Key projects twice, verifies private Preview/discard and deterministic
Apply output, then checks Undo/Redo. This is not mouse-driven UI acceptance.

The pure API includes cubic_point, cubic_points, sample_cubic and sample_path. Inputs
and output are bounded to4096 points; mixed paths allow at most256 segments. Fixed
curve_steps is not an adaptive error tolerance. Inspect docs/LUA_API.md for complete
schemas, rejection cases, finite/local-coordinate limits and memory/cancellation rules.
Image affine descriptors and sampled point columns compose directly with these calls.
Use checked plum.ticks arithmetic to add large absolute song origins to local offsets.

`flatten_cubic(control,{tolerance=0.001})` is the separate adaptive API. It returns
ordinary count/x/y columns or fails explicitly if tolerance cannot be met within
point/depth/coordinate-precision bounds. `polygon_classify` supports even-odd and nonzero
fill with explicit boundary classification; `nearest_polyline` returns the finite
segment, parameter and distance. These pure services also work without project access.

`flatten_path(path,{tolerance=0.001,max_points=1024})` reuses the mixed descriptor and
native cubic kernel. The point cap is shared by the complete path, including start,
retained joints and conditional straight closure. Empty paths return one point; every
failed/canceled segment discards the entire result. Depth/precision failures remain
explicit. Both adaptive functions coexist with fixed sample_cubic/sample_path.

There is no custom graph window, pointer tool or new host-generated advanced function.
The required Custom Graph Laboratory and Custom Drawing Studio interfaces remain
separate mandatory API02 development work. The native API01 curve and Draw/Paint tools
remain available.


The pure rasterize_polygon/rasterize_polyline APIs return sorted unique row spans with
zero-based first (inclusive)/last (exclusive) cell indices and cell_count. The host
uses native even-odd row-centre fills and existing stroke/rounding rules. Lua authors
choose all musical mapping; spans can also represent masks without creating notes.
Input4096points, grid65536columns/4096rows, max65536raw pieces/default8192 and a
4194304vertex-row work bound prevent unbounded output. Native/VM quotas and cancellation
apply. Inspect docs/LUA_API.md for thin-interval/corner semantics and full grid schema.


This package is also embedded in **Plugins > Lua Plugins > Examples**; no download
or source checkout is needed. Installation starts disabled with no capabilities
granted by default. Review the listed permissions and enable it using the normal
plugin controls. Reinstall restores the shipped files and preserves your decisions.
