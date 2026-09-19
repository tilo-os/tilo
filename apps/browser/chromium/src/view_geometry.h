#pragma once

#include <cmath>

namespace qos {

enum class ResizeEffect { Changed, Repaint };

// Tracks the physical size requested over IPC separately from CEF's logical
// view rect. During a scale-only resize CEF can still deliver a queued paint
// for the previous scale; callers must not publish that stale buffer.
class ViewGeometry {
 public:
  ResizeEffect ResizePhysical(int physical_width, int physical_height,
                              double scale) {
    const int logical_width = static_cast<int>(std::ceil(physical_width / scale));
    const int logical_height = static_cast<int>(std::ceil(physical_height / scale));
    if (physical_width == requested_width_ && physical_height == requested_height_ &&
        logical_width == logical_width_ && logical_height == logical_height_ &&
        scale == scale_) {
      return ResizeEffect::Repaint;
    }
    requested_width_ = physical_width;
    requested_height_ = physical_height;
    logical_width_ = logical_width;
    logical_height_ = logical_height;
    scale_ = scale;
    // CEF paints an integer DIP view at device scale. When the requested
    // physical size is not divisible by the scale, the returned buffer is
    // rounded outward from that integer logical rect.
    physical_width_ = static_cast<int>(std::ceil(logical_width_ * scale_));
    physical_height_ = static_cast<int>(std::ceil(logical_height_ * scale_));
    return ResizeEffect::Changed;
  }

  bool PaintMatches(int width, int height) const {
    return width == physical_width_ && height == physical_height_;
  }
  int logical_width() const { return logical_width_; }
  int logical_height() const { return logical_height_; }
  double scale() const { return scale_; }

 private:
  int requested_width_ = 1280;
  int requested_height_ = 800;
  int physical_width_ = 1280;
  int physical_height_ = 800;
  int logical_width_ = 1280;
  int logical_height_ = 800;
  double scale_ = 1.0;
};

}  // namespace qos
