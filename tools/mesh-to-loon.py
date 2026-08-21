"""Reconstruct CAD exchange files and watertight meshes as native Loon.

Run it with the system Python; the launcher locates FreeCAD and re-enters the
same file inside FreeCAD's Python runtime:

    python3 tools/mesh-to-loon.py input.step output.loon \
        --report output.json --source-name input.step

Supported containers are BREP/BRP, STEP/STP, STL, OBJ, and 3MF.  The current
recognizer is intentionally fail-closed: every solid must be expressible as
piecewise-constant extrusion layers along one principal axis.  It emits
sketches, extrusions, and hole differences; it never embeds the source mesh.
"""

import argparse
import json
import math
import os
import shutil
import subprocess
import sys


TOL = 1.0e-6
MESH_EXTENSIONS = {".stl", ".obj", ".3mf"}
SUPPORTED_EXTENSIONS = MESH_EXTENSIONS | {".brep", ".brp", ".step", ".stp"}


def arguments(raw=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", help="BREP, STEP, STL, OBJ, or 3MF input")
    parser.add_argument("output", help="destination .loon file")
    parser.add_argument("--report", help="optional JSON fidelity report")
    parser.add_argument("--source-name", help="display name written to the Loon header")
    return parser.parse_args(raw)


try:
    import FreeCAD
    import Part
except ImportError:
    # Standalone launcher mode. The FreeCAD-side invocation uses environment
    # variables so FreeCADCmd does not try to open helper arguments as models.
    args = arguments()
    candidates = [
        os.environ.get("FREECADCMD"),
        shutil.which("FreeCADCmd"),
        shutil.which("freecadcmd"),
        "/Applications/FreeCAD.app/Contents/Resources/bin/freecadcmd",
    ]
    executable = next((path for path in candidates if path and os.path.isfile(path)), None)
    if not executable:
        raise SystemExit("FreeCADCmd was not found; install FreeCAD or set FREECADCMD")
    child_env = os.environ.copy()
    child_env.update({
        "VCAD_MESH_TO_LOON_SOURCE": os.path.abspath(args.source),
        "VCAD_MESH_TO_LOON_OUTPUT": os.path.abspath(args.output),
        "VCAD_MESH_TO_LOON_SOURCE_NAME": args.source_name or os.path.basename(args.source),
    })
    if args.report:
        child_env["VCAD_MESH_TO_LOON_REPORT"] = os.path.abspath(args.report)
    for stale_path in [
        child_env["VCAD_MESH_TO_LOON_OUTPUT"],
        child_env.get("VCAD_MESH_TO_LOON_REPORT"),
    ]:
        if stale_path and os.path.isfile(stale_path):
            os.remove(stale_path)
    completed = subprocess.run([executable, os.path.abspath(__file__)], env=child_env)
    if completed.returncode != 0:
        raise SystemExit(completed.returncode)
    if not os.path.isfile(child_env["VCAD_MESH_TO_LOON_OUTPUT"]):
        # FreeCADCmd can report a Python exception while still returning zero.
        raise SystemExit(2)
    if args.report and not os.path.isfile(child_env["VCAD_MESH_TO_LOON_REPORT"]):
        raise SystemExit(2)
    raise SystemExit(0)


source_path = os.environ.get("VCAD_MESH_TO_LOON_SOURCE")
output_path = os.environ.get("VCAD_MESH_TO_LOON_OUTPUT")
if not source_path or not output_path:
    raise RuntimeError("mesh-to-loon helper must be launched with its standalone Python CLI")
source_path = os.path.abspath(source_path)
output_path = os.path.abspath(output_path)
report_value = os.environ.get("VCAD_MESH_TO_LOON_REPORT")
report_path = os.path.abspath(report_value) if report_value else None
source_name = os.environ.get("VCAD_MESH_TO_LOON_SOURCE_NAME", os.path.basename(source_path))
source_name = source_name.replace("\n", " ").replace("\r", " ")
source_extension = os.path.splitext(source_path)[1].lower()
if source_extension not in SUPPORTED_EXTENSIONS:
    raise RuntimeError("unsupported input format: " + source_extension)

is_mesh_source = source_extension in MESH_EXTENSIONS
if is_mesh_source:
    import Mesh

    mesh = Mesh.Mesh(source_path)
    if mesh.CountFacets == 0:
        raise RuntimeError("mesh contains no triangles")
    if not mesh.isSolid():
        raise RuntimeError("native reconstruction requires a closed, watertight mesh")
    shell_shape = Part.Shape()
    shell_shape.makeShapeFromMesh(mesh.Topology, TOL)
    if not shell_shape.Shells:
        raise RuntimeError("mesh contains no closed shells")
    solids = [Part.makeSolid(shell) for shell in shell_shape.Shells]
    if any(solid.isNull() or not solid.isValid() for solid in solids):
        raise RuntimeError("mesh could not be converted into valid solid shells")
    shape = (solids[0] if len(solids) == 1 else Part.makeCompound(solids)).removeSplitter()
else:
    shape = Part.Shape()
    shape.read(source_path)

if shape.isNull() or len(shape.Solids) == 0:
    raise RuntimeError("source contains no solid bodies")
if not shape.isValid():
    raise RuntimeError("source contains an invalid OpenCASCADE shape")

# A layered extrusion has one common axis. Planes are parallel/perpendicular
# to it and cylinders run along it. Try all principal axes and accept only a
# full match.
candidates = [
    ("x", FreeCAD.Vector(1, 0, 0), FreeCAD.Vector(0, 0, 1), FreeCAD.Vector(0, 1, 0)),
    ("y", FreeCAD.Vector(0, 1, 0), FreeCAD.Vector(1, 0, 0), FreeCAD.Vector(0, 0, 1)),
    ("z", FreeCAD.Vector(0, 0, 1), FreeCAD.Vector(0, 1, 0), FreeCAD.Vector(1, 0, 0)),
]


def supports_axis(axis):
    angular_tolerance = 1.0e-3 if is_mesh_source else TOL
    for face in shape.Faces:
        surface = face.Surface
        kind = type(surface).__name__
        if kind == "Plane":
            dot = abs(surface.Axis.dot(axis))
            if dot > angular_tolerance and abs(dot - 1.0) > angular_tolerance:
                return False
        elif kind == "Cylinder":
            if abs(abs(surface.Axis.dot(axis)) - 1.0) > TOL:
                return False
        else:
            return False
    return True


matches = [candidate for candidate in candidates if supports_axis(candidate[1])]
if not matches:
    kinds = sorted(set(type(face.Surface).__name__ for face in shape.Faces))
    raise RuntimeError(
        "no exact layered-extrusion reconstruction found; surface types: " + ", ".join(kinds)
    )

axis_name, axis, u_dir, v_dir = matches[0]
raw_levels = sorted(vertex.Point.dot(axis) for vertex in shape.Vertexes)
level_tolerance = 1.0e-3 if is_mesh_source else TOL
level_clusters = []
for value in raw_levels:
    if not level_clusters or value - level_clusters[-1][-1] > level_tolerance:
        level_clusters.append([value])
    else:
        level_clusters[-1].append(value)
levels = [sum(cluster) / len(cluster) for cluster in level_clusters]
if len(levels) < 2:
    raise RuntimeError("solid has fewer than two distinct feature depths")


def fmt(value):
    if abs(value) < 5.0e-10:
        value = 0.0
    return format(value, ".10g")


def point2(vector):
    return (vector.dot(u_dir), vector.dot(v_dir))


def ccw_through(start, end, through, center):
    start_angle = math.atan2(start[1] - center[1], start[0] - center[0])
    end_angle = math.atan2(end[1] - center[1], end[0] - center[0])
    through_angle = math.atan2(through[1] - center[1], through[0] - center[0])
    ccw_span = (end_angle - start_angle) % (2.0 * math.pi)
    through_span = (through_angle - start_angle) % (2.0 * math.pi)
    return through_span <= ccw_span + 1.0e-9


def curve_segments(wire):
    result = []
    for edge in wire.OrderedEdges:
        curve = edge.Curve
        kind = type(curve).__name__
        first = edge.FirstParameter
        last = edge.LastParameter
        if edge.Orientation == "Reversed":
            first, last = last, first
        start3 = edge.valueAt(first)
        end3 = edge.valueAt(last)
        start = point2(start3)
        end = point2(end3)
        if kind == "Line":
            result.append(("line", start, end, None, None))
            continue
        if kind != "Circle":
            raise RuntimeError("unsupported cross-section curve: " + kind)

        center = point2(curve.Center)
        if (start3 - end3).Length <= TOL:
            quarter = point2(edge.valueAt(first + (last - first) / 4.0))
            mid = point2(edge.valueAt(first + (last - first) / 2.0))
            three_quarter = point2(edge.valueAt(first + 3.0 * (last - first) / 4.0))
            result.append(("arc", start, mid, center, ccw_through(start, mid, quarter, center)))
            result.append(("arc", mid, end, center, ccw_through(mid, end, three_quarter, center)))
        else:
            through = point2(edge.valueAt(first + (last - first) / 2.0))
            result.append(("arc", start, end, center, ccw_through(start, end, through, center)))

    # Triangle-derived section wires carry tiny independent endpoint noise.
    # Snap adjacent segments to the same 2D points so the native sketch closes
    # without changing the source tolerance.
    if result:
        snapped = []
        first_start = result[0][1]
        previous_end = None
        for index, (kind, start, end, center, ccw) in enumerate(result):
            if previous_end is not None:
                start = previous_end
            if index == len(result) - 1:
                end = first_start
            snapped.append((kind, start, end, center, ccw))
            previous_end = end
        result = snapped
    return result


def containment_depth(index, faces):
    depth = 0
    for other_index, other in enumerate(faces):
        if other_index == index or other.Area <= faces[index].Area + TOL:
            continue
        common = other.common(faces[index])
        allowed = max(TOL, faces[index].Area * 1.0e-7)
        if common.Area >= faces[index].Area - allowed:
            depth += 1
    return depth


def recover_dense_circle(segments):
    """Replace a densely tessellated circular loop with two native arcs."""
    if len(segments) < 32 or any(segment[0] != "line" for segment in segments):
        return None
    points = [segment[1] for segment in segments]
    a = points[0]
    b = points[len(points) // 3]
    c = points[(2 * len(points)) // 3]
    determinant = 2.0 * (
        a[0] * (b[1] - c[1])
        + b[0] * (c[1] - a[1])
        + c[0] * (a[1] - b[1])
    )
    if abs(determinant) <= TOL:
        return None
    a_squared = a[0] * a[0] + a[1] * a[1]
    b_squared = b[0] * b[0] + b[1] * b[1]
    c_squared = c[0] * c[0] + c[1] * c[1]
    center = (
        (
            a_squared * (b[1] - c[1])
            + b_squared * (c[1] - a[1])
            + c_squared * (a[1] - b[1])
        )
        / determinant,
        (
            a_squared * (c[0] - b[0])
            + b_squared * (a[0] - c[0])
            + c_squared * (b[0] - a[0])
        )
        / determinant,
    )
    radii = [math.hypot(point[0] - center[0], point[1] - center[1]) for point in points]
    radius = sum(radii) / len(radii)
    radial_tolerance = max(1.0e-3, radius * 1.0e-4)
    if radius <= TOL or max(abs(value - radius) for value in radii) > radial_tolerance:
        return None

    signed_area_twice = sum(
        point[0] * points[(index + 1) % len(points)][1]
        - points[(index + 1) % len(points)][0] * point[1]
        for index, point in enumerate(points)
    )
    halfway = points[len(points) // 2]
    ccw = signed_area_twice > 0.0
    return [
        ("arc", points[0], halfway, center, ccw),
        ("arc", halfway, points[0], center, ccw),
    ]


def emit_sketch(lines, name, depth, wire):
    origin = axis * depth
    lines.append(f"[let {name} [sketch")
    lines.append(f"  {fmt(origin.x)} {fmt(origin.y)} {fmt(origin.z)}")
    lines.append(f"  {fmt(u_dir.x)} {fmt(u_dir.y)} {fmt(u_dir.z)}")
    lines.append(f"  {fmt(v_dir.x)} {fmt(v_dir.y)} {fmt(v_dir.z)}")
    lines.append("  #[")
    segments = curve_segments(wire)
    segments = recover_dense_circle(segments) or segments
    for kind, start, end, center, ccw in segments:
        if kind == "line":
            lines.append(
                f"    [line {fmt(start[0])} {fmt(start[1])} {fmt(end[0])} {fmt(end[1])}]"
            )
        else:
            lines.append(
                f"    [arc {fmt(start[0])} {fmt(start[1])} {fmt(end[0])} {fmt(end[1])} "
                f"{fmt(center[0])} {fmt(center[1])} {str(ccw).lower()}]"
            )
    lines.append("  ]]]")


lines = [
    "; Reconstructed as native Loon geometry",
    "; Source: " + source_name,
    "; The source CAD file is not embedded or referenced by this document.",
    "",
]
roots = []
counter = 0
recovered_volume = 0.0

for low, high in zip(levels, levels[1:]):
    if high - low <= TOL:
        continue
    midpoint = (low + high) / 2.0
    wires = [wire for wire in shape.slice(axis, midpoint) if wire.isClosed()]
    if not wires:
        continue
    faces = [Part.Face(wire) for wire in wires]
    depths = [containment_depth(index, faces) for index in range(len(faces))]
    section_area = sum(
        face.Area * (1.0 if depths[index] % 2 == 0 else -1.0)
        for index, face in enumerate(faces)
    )
    recovered_volume += section_area * (high - low)

    for outer_index, outer_wire in enumerate(wires):
        if depths[outer_index] % 2 != 0:
            continue
        counter += 1
        sketch_name = f"profile-{counter}"
        body_name = f"extrusion-{counter}"
        emit_sketch(lines, sketch_name, low, outer_wire)
        direction = axis * (high - low)
        lines.append(
            f"[let {body_name} [extrude {fmt(direction.x)} {fmt(direction.y)} "
            f"{fmt(direction.z)} {sketch_name}]]"
        )
        current = body_name

        # Holes are direct odd-depth children of this outer profile. Loon's
        # booleans are subject-last: [difference tool subject].
        for hole_index, hole_wire in enumerate(wires):
            if depths[hole_index] != depths[outer_index] + 1:
                continue
            common = faces[outer_index].common(faces[hole_index])
            allowed = max(TOL, faces[hole_index].Area * 1.0e-7)
            if common.Area < faces[hole_index].Area - allowed:
                continue
            counter += 1
            hole_sketch = f"hole-profile-{counter}"
            hole_body = f"hole-extrusion-{counter}"
            cut_body = f"cut-{counter}"
            emit_sketch(lines, hole_sketch, low, hole_wire)
            lines.append(
                f"[let {hole_body} [extrude {fmt(direction.x)} {fmt(direction.y)} "
                f"{fmt(direction.z)} {hole_sketch}]]"
            )
            lines.append(f"[let {cut_body} [difference {hole_body} {current}]]")
            current = cut_body
        roots.append(current)
        lines.append("")

if not roots:
    raise RuntimeError("no closed cross-section profiles were reconstructed")

relative_error = abs(recovered_volume - shape.Volume) / max(abs(shape.Volume), 1.0)
allowed_relative_error = 1.0e-5 if is_mesh_source else 1.0e-7
if relative_error > allowed_relative_error:
    raise RuntimeError(
        f"reconstruction volume check failed ({relative_error * 100.0:.6f}% error)"
    )

# Touching extrusion slabs remain separate roots. This avoids feeding exactly
# coincident slab faces into a boolean union while retaining the same visible
# and exportable geometry.
for index, root in enumerate(roots, 1):
    lines.append(f"[root {root} \"default\"] ; Recovered layer {index}")

with open(output_path, "w") as output:
    output.write("\n".join(lines) + "\n")

report = {
    "axis": axis_name,
    "layers": len(roots),
    "source_volume": shape.Volume,
    "reconstructed_volume": recovered_volume,
    "relative_volume_error": relative_error,
    "face_count": len(shape.Faces),
    "solid_count": len(shape.Solids),
    "source_format": source_extension[1:],
}
if report_path:
    with open(report_path, "w") as output:
        json.dump(report, output)

print(json.dumps(report))
