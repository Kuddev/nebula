// Nebula background image shader.
//
// The terminal background is drawn as straight-alpha RGBA so it can blend with
// the existing transparent-window path instead of making the whole window
// opaque when a wallpaper is configured.
//
// `uClipRect`/`uClipRadius` clip the wallpaper to the terminal card's ROUNDED
// rect (same SDF as the UI quads): without it the rectangular image painted
// square corners on top of the card and the shell radius disappeared.
// Coordinates are framebuffer px with the GL bottom-left origin; radius 0
// disables rounding (full-window "cover chrome" mode).

#if defined(GLES2_RENDERER)
#define float_t mediump float
#define FRAG_COLOR gl_FragColor

uniform sampler2D uTexture;
uniform float_t uOpacity;
uniform mediump vec4 uClipRect;
uniform float_t uClipRadius;

varying mediump vec2 uv;
#else
#define float_t float
#define vec2_t vec2

out vec4 FragColor;
#define FRAG_COLOR FragColor

uniform sampler2D uTexture;
uniform float_t uOpacity;
uniform vec4 uClipRect;
uniform float_t uClipRadius;

in vec2 uv;
#endif

void main() {
#if defined(GLES2_RENDERER)
    vec4 col = texture2D(uTexture, uv);
#else
    vec4 col = texture(uTexture, uv);
#endif

    float_t coverage = 1.0;
    if (uClipRadius > 0.0) {
        // Signed distance to the rounded rect; 1px feather anti-aliases the
        // clipped edge so the card corner stays as crisp as the UI quads.
        mediump vec2 half_size = uClipRect.zw * 0.5;
        mediump vec2 center = uClipRect.xy + half_size;
        mediump vec2 from_center = abs(gl_FragCoord.xy - center);
        mediump vec2 corner = from_center - (half_size - vec2(uClipRadius, uClipRadius));
        float_t dist = length(max(corner, vec2(0.0, 0.0))) - uClipRadius;
        coverage = clamp(0.5 - dist, 0.0, 1.0);
    }

    FRAG_COLOR = vec4(col.rgb, col.a * clamp(uOpacity, 0.0, 1.0) * coverage);
}
