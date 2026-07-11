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
in vec4 color0;
in vec4 color1;
#endif

// Signed distance to a rounded box centered at the origin.
float_t sdRoundedBox(vec2 p, vec2 halfSize, float_t radius) {
    vec2 q = abs(p) - halfSize + radius;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - radius;
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
    } else if (quadRadius < 0.0) {
        // Flat fill: the shape comes from the geometry (e.g. slanted powerline
        // parallelograms), so cover every fragment.
        coverage = 1.0;
    } else {
        // Crisp rounded box with 1px antialiasing from the signed distance.
        float_t radius = min(quadRadius, min(halfSize.x, halfSize.y));
        float_t d = sdRoundedBox(p, halfSize, radius);
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
