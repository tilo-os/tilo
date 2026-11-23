#include "fullscreen_state.h"
#include <cassert>

int main() {
  tilo::FullscreenState state;
  assert(!state.active());
  assert(state.Set(true));
  assert(state.active());
  assert(!state.Set(true));
  assert(state.Set(false));
  assert(!state.active());
  assert(!state.ResetForRendererTermination());
  assert(state.Set(true));
  assert(state.ResetForRendererTermination());
  assert(!state.active());
}
