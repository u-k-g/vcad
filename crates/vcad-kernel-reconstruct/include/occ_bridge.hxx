#pragma once

#include "rust/cxx.h"

#include <BRep_Builder.hxx>
#include <BRepTools.hxx>
#include <Message_ProgressRange.hxx>
#include <TopoDS_Shape.hxx>

#include <memory>
#include <string>

namespace vcad_reconstruct {

using TopoDS_Shape = ::TopoDS_Shape;

inline std::unique_ptr<TopoDS_Shape> read_brep(rust::Str path) {
  const std::string filename(path.data(), path.size());
  auto shape = std::make_unique<TopoDS_Shape>();
  BRep_Builder builder;
  if (!BRepTools::Read(*shape, filename.c_str(), builder,
                       Message_ProgressRange())) {
    return nullptr;
  }
  return shape;
}

} // namespace vcad_reconstruct
