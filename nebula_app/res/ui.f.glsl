// Nebula chrome UI shader: rounded-rect SDF + linear gradient fill.
//
// Antialiasing is derivative-free: the SDF is expressed in pixels, so a 1px
// edge falloff `clamp(0.5 - d)` gives clean AA on both GLES2 and GLSL3 without
// requiring the standard-derivatives extension.

#if defined(GLES2_RENDERER)
#define float_t mediump float
#define FRAG_COLOR gl_FragColor

varying mediump vec2 uv;
varying mediump vec2 quadSize;
varying mediump float quadRadius;
varying mediump float quadFeather;
varying mediump vec2 grad;
varying mediump vec4 corners;
varying mediump vec4 color0;
varying mediump vec4 color1;
#else
#define float_t float

out vec4 FragColor;
#define FRAG_COLOR FragColor

in vec2 uv;
in vec2 quadSize;
in float quadRadius;
in float quadFeather;
in vec2 grad;
in vec4 corners;
in vec4 color0;
in vec4 color1;
#endif

// Signed distance to a rounded box centered at the origin, with per-corner
// radii. `radii` = [top-left, top-right, bottom-right, bottom-left]; the corner
// facing `p`'s quadrant is selected so join corners can stay square while the
// outer ones curve (the connected top-bar/sidebar L-frame).
float_t sdRoundedBox(vec2 p, vec2 halfSize, vec4 radii) {
    // p.x > 0 -> right half (top-right / bottom-right), else left half.
    float_t r = (p.x > 0.0) ? ((p.y > 0.0) ? radii.z : radii.y)
                            : ((p.y > 0.0) ? radii.w : radii.x);
    vec2 q = abs(p) - halfSize + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

void main() {
    // Pixel-space position within the quad, centered at its middle.
    vec2 p = (uv - 0.5) * quadSize;
    vec2 halfSize = quadSize * 0.5;

    float_t coverage;
    if (quadFeather > 0.5) {
        // Soft radial glow: smooth falloff from the center outward.
        float_t maxr = min(halfSize.x, halfSize.y);
        float_t dc = length(p) / max(maxr, 1.0);
        coverage = clamp(1.0 - dc, 0.0, 1.0);
        coverage = coverage * coverage;
    } else if (quadFeather < -0.5) {
        // Rounded-rectangle outer shadow. The quad is expanded by `blur` on
        // every edge; evaluate the original inset card and fade only through
        // the expansion band. The opaque interior is covered by the card drawn
        // immediately afterwards.
        float_t blur = -quadFeather;
        vec2 innerHalf = max(halfSize - vec2(blur), vec2(1.0));
        float_t lim = min(innerHalf.x, innerHalf.y);
        vec4 radii = min(corners, vec4(lim));
        float_t d = sdRoundedBox(p, innerHalf, radii);
        float_t edge = clamp(max(d, 0.0) / blur, 0.0, 1.0);
        coverage = 1.0 - smoothstep(0.0, 1.0, edge);
    } else if (quadRadius < 0.0) {
        // Flat fill: the shape comes from the geometry (e.g. slanted powerline
        // parallelograms), so cover every fragment.
        coverage = 1.0;
    } else {
        // Crisp rounded box with 1px antialiasing from the signed distance.
        // Each corner is clamped independently so a large outer radius and a
        // square (0) join corner can coexist on the same quad.
        float_t lim = min(halfSize.x, halfSize.y);
        vec4 radii = min(corners, vec4(lim));
        float_t d = sdRoundedBox(p, halfSize, radii);
        coverage = clamp(0.5 - d, 0.0, 1.0);
    }

    if (coverage <= 0.0) {
        discard;
    }

    // Linear gradient along the `grad` axis in uv space.
    float_t t = clamp(dot(uv, grad), 0.0, 1.0);
    vec4 col = mix(color0, color1, t);

    FRAG_COLOR = vec4(col.rgb, col.a * coverage);
}
