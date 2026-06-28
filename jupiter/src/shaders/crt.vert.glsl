#version 450

// Fullscreen-triangle vertex shader for the CRT presenter (SDL_GPU / Vulkan).
// No vertex buffer: three vertices are generated from gl_VertexIndex, covering
// the whole viewport with a single oversized triangle (the [1,2] UV range falls
// outside the screen and is clipped). `draw_primitives(3, 1, 0, 0)` invokes it.
//
// UV convention: v_uv is 0 at the top-left of the frame texture and 1 at the
// bottom-right, matching how the framebuffer is uploaded (row 0 = top). The
// **V is flipped** (`1.0 - xy.y`) because the SDL_GPU swapchain render target is
// Y-down relative to that upload — without it the picture presents upside-down
// (the Vulkan Y-orientation gotcha, ADR-0019; confirmed at runtime).

layout(location = 0) out vec2 v_uv;

void main() {
    // idx 0 -> (0,0)/(-1,-1), idx 1 -> (2,0)/(3,-1), idx 2 -> (0,2)/(-1,3).
    // On-screen xy spans [0,1]; the off-screen [1,2] range is clipped.
    vec2 xy = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
    gl_Position = vec4(xy * 2.0 - 1.0, 0.0, 1.0);
    v_uv = vec2(xy.x, 1.0 - xy.y);
}
