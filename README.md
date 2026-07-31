# idf-visualizer

Fast, cross-platform 3D viewer for EnergyPlus IDF models, with an interactive
HVAC loop schematic. Visualization and debugging only — no editing. Built with
Rust, wgpu, and egui.

## Usage

```sh
idf-visualizer model.idf          # open the viewer (defaults to in.idf)
idf-visualizer model.idf --info   # parse, print surface/geometry warnings, exit
idf-visualizer model.idf --demo DIR   # scripted run that saves feature screenshots
idf-visualizer svg model.idf -o model.svg   # headless SVG export for reports
idf-visualizer -h                 # usage (also --help)
```

## SVG export

`idf-visualizer svg` renders the model to a standalone, dependency-free SVG —
no window or GPU needed, so it works over SSH and in CI. The default view is a
true isometric (45° azimuth, 35.264° above the horizon), orthographic and
scaled to fit the content exactly, so the figure has no wasted whitespace.

```sh
idf-visualizer svg model.idf -o iso.svg               # fitted isometric
idf-visualizer svg model.idf -r 135 -o ne.svg         # rotate the view
idf-visualizer svg model.idf -r 0 -e 90 -o plan.svg   # plan view from above
idf-visualizer svg model.idf --hide roof,ceiling --no-cull --legend -o cut.svg
idf-visualizer svg model.idf --zone 'core' -w 600 | display -   # stdout
```

| Option | Meaning |
|---|---|
| `-h, --help` | Show usage and exit (also `--help`) |
| `-o, --out FILE` | Output file (default: stdout) |
| `-r, --rotation DEG` | View azimuth; 0 = from the south, positive swings east (default 45) |
| `-e, --elevation DEG` | Angle above the horizon (default 35.264, true isometric) |
| `-w, --width PX` | Output width (default 1000) |
| `-H, --height PX` | Output height (default: fitted to the content) |
| `--margin PX` | Padding around the drawing (default 24) |
| `--stroke-width N` | Edge width in px (default 0.8) |
| `--zone REGEX` | Only surfaces whose zone matches (case-insensitive) |
| `--name REGEX` | Only surfaces whose name matches (case-insensitive) |
| `--hide TYPES` | Comma-separated types to omit, e.g. `roof,ceiling` |
| `--no-cull` | Also draw surfaces facing away from the viewer |
| `--flat` | Solid type colors instead of angle-based shading |
| `--legend` | Surface-type key below the model |
| `--background CSS` | Canvas fill (default: transparent) |

### Copy CLI from the viewer

Lining up an angle by hand is easier than guessing degrees, so the viewer's
left panel has an **SVG export of this view** section: it shows the
`idf-visualizer svg …` command for the current camera angle and filters and
copies it to the clipboard with **Copy CLI**. Orbit until the model looks
right, toggle the surface types and zone/name filters you want, copy, and
paste the line into a build script — the export then reproduces that view
without the GUI.

The copied command carries the camera rotation and elevation, the hidden
surface types, the zone and name filters, an `-o <model>.svg` next to the
model, and `--no-cull` (the viewport draws both sides of every surface, so
matching it keeps floor plates in the picture). "Show only flagged surfaces"
has no CLI equivalent and is called out in the panel when it is on.

Faces are painted back to front and shaded by their angle to the viewer, using
the same type colors as the interactive viewer. Hidden surfaces come out right
without a z-buffer: the depth sort is followed by Newell's swap pass, so a face
that genuinely occludes another is painted after it even when a large polygon
(a floor plate) reaches nearer the eye than the small ones covering it (the
roof above). Windows and doors are coplanar with their host wall, where no
depth test can separate them, so they ride along immediately after their base
surface.
Back-facing opaque surfaces are culled by default; since an EnergyPlus floor's
outward normal points down, a roof-off cutaway usually wants `--no-cull` so
floor plates show.

## HVAC loop schematic

The **HVAC loops** tab in the left panel lists every `PlantLoop`,
`CondenserLoop`, and `AirLoopHVAC` in the file and draws the selected one as an
interactive circuit diagram: supply side flowing left→right on top, demand side
returning right→left below, dashed runs closing the loop. Series components,
splitter/mixer bars, and parallel branches are laid out from the loop's
`BranchList`/`Branch`/`Connector:Splitter`/`Connector:Mixer` objects; air loop
demand sides are resolved through `AirLoopHVAC:SupplyPath`/`ReturnPath`, air
distribution units, and `ZoneHVAC:EquipmentConnections` node matching. All of
it comes straight from the IDF — no `.bnd` file needed.

- Every connection carries a node dot; hovering it pops the node name in a
  readable callout. Click a component for its nodes, referenced sub-objects
  (fan and coils of a unitary system, controllers of an OA system, a zone's
  equipment list), and raw IDF text; hover for a quick node tooltip.
- Zone boxes have a **Show zone in 3D** button that jumps to the 3D view
  filtered to that zone.
- Node-connection mismatches within a branch and unreferenced/missing branch
  objects are reported as loop warnings (the checks a `.bnd` would give you).

## Controls

| Input | Action |
|---|---|
| Left-drag | Orbit (3D) / pan (loops) |
| Shift-drag / right-drag / middle-drag | Pan |
| Scroll | Zoom |
| Click | Select surface or loop component (properties panel opens) |
| `F` | Zoom to fit |
| `Esc` | Deselect |

## Features

- Surfaces colored by type (walls, roofs, ceilings, floors, windows, doors,
  shading), with per-type show/hide toggles and counts.
- Case-insensitive regex filter on surface names.
- Click selection: highlights the surface, draws its outward normal vector,
  and shows properties — construction, zone, space, boundary condition,
  area (m²/ft²), azimuth, tilt, normal, vertices, and the raw IDF text with
  its source line number.
- Zoom-to-fit and zoom-to-surface.
- "Copy CLI": the `idf-visualizer svg` command for the current view and
  filters, for pasting into a reproducible build script (see below).
- Geometry diagnostics: degenerate (zero-area) and non-planar surfaces are
  flagged and listed.

## Geometry support

`BuildingSurface:Detailed`, `FenestrationSurface:Detailed`, rectangular
`Window`/`Door`/`GlazedDoor`, and `Shading:Site/Building/Zone:Detailed`.
Handles `GlobalGeometryRules` (relative/world coordinates, clockwise or
counterclockwise vertex entry), zone origins, zone "direction of relative
north", and the building north axis. Non-convex polygons are triangulated
with ear clipping.

The IDF parsing and geometry construction live in `src/idf.rs` and
`src/model.rs` with no GPU dependency, so they are unit-testable and
reusable from a CLI (`--info`, `svg`).
