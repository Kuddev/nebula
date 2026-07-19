// Native math glyph quads. Positions are full-window NDC; UVs address the fixed atlas.

#if defined(GLES2_RENDERER)
attribute vec2 aPos;
attribute vec2 aUv;
varying mediump vec2 uv;
#else
layout(location = 0) in vec2 aPos;
layout(location = 1) in vec2 aUv;
out vec2 uv;
#endif

void main() {
    uv = aUv;
    gl_Position = vec4(aPos, 0.0, 1.0);
}
