#pragma once

namespace qos {

// Edge filter for CEF's content-fullscreen callback. Renderer termination
// clears native ownership even when CEF cannot publish a matching false edge.
class FullscreenState {
 public:
  bool active() const { return active_; }
  bool Set(bool active) {
    if (active_ == active) return false;
    active_ = active;
    return true;
  }
  bool ResetForRendererTermination() { return Set(false); }

 private:
  bool active_ = false;
};

}  // namespace qos
