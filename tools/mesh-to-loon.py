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
DEFAULT_DECIMAL_PLACES = 4
DEFAULT_SIMPLIFICATION_TOLERANCE = 0.01
MESH_EXTENSIONS = {".stl", ".obj", ".3mf"}
SUPPORTED_EXTENSIONS = MESH_EXTENSIONS | {".brep", ".brp", ".step", ".stp"}


def arguments(raw=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", help="BREP, STEP, STL, OBJ, or 3MF input")
    parser.add_argument("output", help="destination .loon file")
    parser.add_argument("--report", help="optional JSON fidelity report")
    parser.add_argument("--source-name", help="display name written to the Loon header")
    parser.add_argument(
        "--decimal-places",
        type=int,
        default=DEFAULT_DECIMAL_PLACES,
        help="decimal places written to Loon (0-8; default: 4)",
    )
    parser.add_argument(
        "--simplification-tolerance",
        type=float,
        default=DEFAULT_SIMPLIFICATION_TOLERANCE,
        help="maximum contour deviation in model units (default: 0.01)",
    )
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
        "VCAD_MESH_TO_LOON_DECIMAL_PLACES": str(args.decimal_places),
        "VCAD_MESH_TO_LOON_SIMPLIFICATION_TOLERANCE": str(
            args.simplification_tolerance
        ),
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
decimal_places = int(
    os.environ.get("VCAD_MESH_TO_LOON_DECIMAL_PLACES", DEFAULT_DECIMAL_PLACES)
)
simplification_tolerance = float(
    os.environ.get(
        "VCAD_MESH_TO_LOON_SIMPLIFICATION_TOLERANCE",
        DEFAULT_SIMPLIFICATION_TOLERANCE,
    )
)
if decimal_places < 0 or decimal_places > 8:
    raise RuntimeError("decimal places must be between 0 and 8")
if not math.isfinite(simplification_tolerance) or not 1.0e-6 <= simplification_tolerance <= 1.0:
    raise RuntimeError("simplification tolerance must be between 0.000001 and 1.0")
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
    zero_threshold = 0.5 * (10.0 ** -decimal_places) if decimal_places else 0.5
    if abs(value) < zero_threshold:
        value = 0.0
    text = format(value, f".{decimal_places}f")
    if "." in text:
        text = text.rstrip("0").rstrip(".")
    return "0" if text == "-0" else text


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


def circle_from_three(a, b, c):
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
    return (
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


def circle_fit(points):
    """Return (center, radius, polyline-to-circle error) for an arc candidate."""
    if math.hypot(points[0][0] - points[-1][0], points[0][1] - points[-1][1]) <= TOL:
        center = circle_from_three(
            points[0], points[len(points) // 3], points[(2 * len(points)) // 3]
        )
    else:
        center = circle_from_three(points[0], points[len(points) // 2], points[-1])
    if center is None:
        return None
    radii = [math.hypot(point[0] - center[0], point[1] - center[1]) for point in points]
    radius = sum(radii) / len(radii)
    if radius <= TOL:
        return None
    # Checking only vertices would turn any regular polygon into a circle.
    # Include each chord midpoint so the tolerance is a genuine upper bound on
    # how far the emitted analytic arc may move away from the source polyline.
    errors = [abs(value - radius) for value in radii]
    for start, end in zip(points, points[1:]):
        midpoint = ((start[0] + end[0]) / 2.0, (start[1] + end[1]) / 2.0)
        errors.append(abs(math.hypot(midpoint[0] - center[0], midpoint[1] - center[1]) - radius))
    return center, radius, max(errors)


def signed_turn(a, b, c):
    incoming = (b[0] - a[0], b[1] - a[1])
    outgoing = (c[0] - b[0], c[1] - b[1])
    return math.atan2(
        incoming[0] * outgoing[1] - incoming[1] * outgoing[0],
        incoming[0] * outgoing[0] + incoming[1] * outgoing[1],
    )


def recover_dense_circle(segments):
    """Replace a tessellated circular loop with two native arcs."""
    if len(segments) < 8 or any(segment[0] != "line" for segment in segments):
        return None
    points = [segment[1] for segment in segments]
    # Repeat the first point so chord sagitta is included in the fit error.
    fit = circle_fit(points + [points[0]])
    if fit is None:
        return None
    center, _radius, error = fit
    if error > simplification_tolerance:
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


def recover_partial_arcs(segments):
    """Recover analytic arcs embedded in an otherwise polygonal contour."""
    if len(segments) < 6 or any(segment[0] != "line" for segment in segments):
        return segments

    # Start on the straightest vertex. This prevents a rounded corner from
    # wrapping across the list boundary simply because OpenCASCADE chose an
    # arbitrary first edge for the closed wire.
    cyclic_points = [segment[1] for segment in segments]
    turns = [
        abs(
            signed_turn(
                cyclic_points[index - 1],
                cyclic_points[index],
                cyclic_points[(index + 1) % len(cyclic_points)],
            )
        )
        for index in range(len(cyclic_points))
    ]
    rotation = min(range(len(turns)), key=lambda index: turns[index])
    segments = segments[rotation:] + segments[:rotation]
    points = [segment[1] for segment in segments] + [segments[-1][2]]

    interior_turns = [signed_turn(points[i - 1], points[i], points[i + 1]) for i in range(1, len(points) - 1)]
    candidates = []
    run_start = None
    run_sign = 0
    for offset, turn in enumerate(interior_turns, 1):
        sign = 1 if turn > 0.0 else -1
        is_curve = 1.0e-4 <= abs(turn) <= 0.35
        if is_curve and (run_start is None or sign == run_sign):
            if run_start is None:
                run_start = offset
                run_sign = sign
            continue
        if run_start is not None:
            candidates.append((run_start, offset - 1, run_sign))
        run_start = offset if is_curve else None
        run_sign = sign if is_curve else 0
    if run_start is not None:
        candidates.append((run_start, len(points) - 2, run_sign))

    replacements = {}
    for start, end, sign in candidates:
        arc_points = points[start : end + 1]
        if len(arc_points) < 5:
            continue
        total_turn = sum(interior_turns[index - 1] for index in range(start, end + 1))
        if abs(total_turn) < math.radians(8.0):
            continue
        fit = circle_fit(arc_points)
        if fit is None:
            continue
        center, _radius, error = fit
        if error > simplification_tolerance:
            continue
        # Replace the source line segments between the first and last fitted
        # vertices. Endpoint-adjacent lines remain exact.
        replacements[start] = (
            end,
            ("arc", arc_points[0], arc_points[-1], center, sign > 0),
        )

    if not replacements:
        return segments
    result = []
    index = 0
    while index < len(segments):
        replacement = replacements.get(index)
        if replacement is None:
            result.append(segments[index])
            index += 1
        else:
            end, arc = replacement
            result.append(arc)
            index = end
    return result


def merge_near_collinear_lines(segments):
    """Collapse straight-ish line runs without exceeding the chosen tolerance."""
    result = []
    index = 0
    while index < len(segments):
        segment = segments[index]
        if segment[0] != "line":
            result.append(segment)
            index += 1
            continue
        start = segment[1]
        end = segment[2]
        source_points = [start, end]
        next_index = index + 1
        while next_index < len(segments) and segments[next_index][0] == "line":
            candidate_end = segments[next_index][2]
            chord = (candidate_end[0] - start[0], candidate_end[1] - start[1])
            length = math.hypot(chord[0], chord[1])
            if length <= TOL:
                break
            deviation = max(
                abs(
                    chord[0] * (point[1] - start[1])
                    - chord[1] * (point[0] - start[0])
                )
                / length
                for point in source_points[1:]
            )
            previous = (end[0] - start[0], end[1] - start[1])
            if deviation > simplification_tolerance or previous[0] * chord[0] + previous[1] * chord[1] <= 0.0:
                break
            end = candidate_end
            source_points.append(candidate_end)
            next_index += 1
        result.append(("line", start, end, None, None))
        index = next_index
    return result


def emit_sketch(lines, name, depth, wire):
    origin = axis * depth
    lines.append(f"[let {name} [sketch")
    lines.append(f"  {fmt(origin.x)} {fmt(origin.y)} {fmt(origin.z)}")
    lines.append(f"  {fmt(u_dir.x)} {fmt(u_dir.y)} {fmt(u_dir.z)}")
    lines.append(f"  {fmt(v_dir.x)} {fmt(v_dir.y)} {fmt(v_dir.z)}")
    lines.append("  #[")
    segments = curve_segments(wire)
    original_count = len(segments)
    segments = recover_dense_circle(segments) or recover_partial_arcs(segments)
    segments = merge_near_collinear_lines(segments)
    reconstruction_stats["input_segments"] += original_count
    reconstruction_stats["output_segments"] += len(segments)
    reconstruction_stats["arcs"] += sum(segment[0] == "arc" for segment in segments)
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
    f"; Numeric precision: {decimal_places} decimal places",
    f"; Maximum contour simplification deviation: {fmt(simplification_tolerance)} model units",
    "",
]
roots = []
counter = 0
recovered_volume = 0.0
reconstruction_stats = {"input_segments": 0, "output_segments": 0, "arcs": 0}

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

# Pairwise unions keep the CSG tree shallow and publish one VCAD part even
# when reconstruction needed several adjacent extrusion regions.
union_round = list(roots)
union_counter = 0
while len(union_round) > 1:
    next_round = []
    for index in range(0, len(union_round), 2):
        if index + 1 == len(union_round):
            next_round.append(union_round[index])
            continue
        union_counter += 1
        union_name = f"reconstructed-union-{union_counter}"
        lines.append(
            f"[let {union_name} [union {union_round[index + 1]} {union_round[index]}]]"
        )
        next_round.append(union_name)
    union_round = next_round
lines.append("")
lines.append(f"[root {union_round[0]} \"default\"] ; Reconstructed source body")

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
    "output_parts": 1,
    "decimal_places": decimal_places,
    "simplification_tolerance": simplification_tolerance,
    "input_segments": reconstruction_stats["input_segments"],
    "output_segments": reconstruction_stats["output_segments"],
    "recovered_arcs": reconstruction_stats["arcs"],
}
if report_path:
    with open(report_path, "w") as output:
        json.dump(report, output)

print(json.dumps(report))
