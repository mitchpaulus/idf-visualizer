# idf-visualizer

Fast, cross-platform 3D viewer for EnergyPlus IDF models. Visualization and
debugging only — no editing. Built with Rust, wgpu, and egui.

## Usage

```sh
idf-visualizer model.idf          # open the viewer (defaults to in.idf)
idf-visualizer model.idf --info   # parse, print surface/geometry warnings, exit
idf-visualizer model.idf --demo DIR   # scripted run that saves feature screenshots
```

## Controls

| Input | Action |
|---|---|
| Left-drag | Orbit |
| Shift-drag / right-drag / middle-drag | Pan |
| Scroll | Zoom |
| Click | Select surface (properties panel opens) |
| `F` | Zoom to fit visible surfaces |
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
reusable from a CLI (`--info`).
