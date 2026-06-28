#version 450

// Single-pass CRT post-process for the SDL_GPU presenter (v1: flat geometry —
// scanlines + aperture-grille mask + gamma; no curvature). Presentation-only: it
// reads the already-composited Saturn frame and writes the swapchain. Accuracy is
// untouched (ADR-0019).
//
// SDL_GPU SPIR-V descriptor-set convention (getting this wrong = silent black):
//   fragment sampled textures live in set = 2, fragment uniform buffers in set = 3.
// So the frame sampler is (set=2, binding=0) and the params UBO is (set=3,
// binding=0) — matching bind_fragment_samplers(0, ..) and
// push_fragment_uniform_data(0, ..) on the Rust side.

layout(location = 0) in vec2 v_uv;
layout(location = 0) out vec4 o_color;

layout(set = 2, binding = 0) uniform sampler2D u_frame;

layout(set = 3, binding = 0) uniform Crt {
    vec2 src_size;   // frame texture size in texels (e.g. 320x224)
    vec2 out_size;   // output viewport size in pixels
    float scanline;  // scanline depth, 0 = off .. 1 = strong
    float mask;      // aperture-grille strength, 0 = off .. 1 = strong
    float gamma;     // output gamma (1.0 = none; >1 punchier)
    float _pad;      // std140 padding to a 16-byte boundary
};

void main() {
    vec3 c = texture(u_frame, v_uv).rgb;

    // Scanlines keyed to the SOURCE line, so the count tracks the emulated
    // resolution: darken the gaps between scan lines with a smooth cos profile.
    float line = v_uv.y * src_size.y;
    float s = 0.5 + 0.5 * cos(line * 6.28318530718);
    float scan = mix(1.0, 0.45 + 0.55 * s, scanline);
    c *= scan;

    // Aperture grille (Trinitron-ish): each output column favours one of R/G/B;
    // the other two channels dim by `mask`. mask = 0 leaves the picture untouched.
    int phase = int(mod(v_uv.x * out_size.x, 3.0));
    vec3 m = vec3(1.0 - mask);
    m[phase] = 1.0;
    c *= m;

    // Compensate the average dimming the scanlines + mask introduce, then gamma.
    c *= 1.0 + 0.4 * mask + 0.3 * scanline;
    c = pow(clamp(c, 0.0, 1.0), vec3(1.0 / gamma));

    o_color = vec4(c, 1.0);
}
