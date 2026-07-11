// Nebula background image shader.
//
// The terminal background is drawn as straight-alpha RGBA so it can blend with
// the existing transparent-window path instead of making the whole window
// opaque when a wallpaper is configured.

#if defined(GLES2_RENDERER)
#define float_t mediump float
#define FRAG_COLOR gl_FragColor

uniform sampler2D uTexture;
uniform float_t uOpacity;

varying mediump vec2 uv;
#else
#define float_t float

out vec4 FragColor;
#define FRAG_COLOR FragColor

uniform sampler2D uTexture;
uniform float_t uOpacity;

in vec2 uv;
#endif

void main() {
#if defined(GLES2_RENDERER)
    vec4 col = texture2D(uTexture, uv);
#else
    vec4 col = texture(uTexture, uv);
#endif

    FRAG_COLOR = vec4(col.rgb, col.a * clamp(uOpacity, 0.0, 1.0));
}
