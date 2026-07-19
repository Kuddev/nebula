// Straight-alpha coverage from the native math RGBA atlas.

#if defined(GLES2_RENDERER)
#define FRAG_COLOR gl_FragColor
uniform sampler2D uTexture;
uniform lowp vec4 uColor;
varying mediump vec2 uv;
#else
out vec4 FragColor;
#define FRAG_COLOR FragColor
uniform sampler2D uTexture;
uniform vec4 uColor;
in vec2 uv;
#endif

void main() {
#if defined(GLES2_RENDERER)
    lowp float coverage = texture2D(uTexture, uv).a;
#else
    float coverage = texture(uTexture, uv).a;
#endif
    // The atlas stores grayscale coverage rather than premultiplied color.
    // Keep the RGB fully opaque so the existing straight-alpha blend state
    // uses the coverage as the glyph's actual opacity.
    FRAG_COLOR = vec4(uColor.rgb, uColor.a * coverage);
}
