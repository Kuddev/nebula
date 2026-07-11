// Nebula chrome UI shader: rounded + gradient quads.
// Positions are already in normalized device coordinates (NDC).

#if defined(GLES2_RENDERER)
attribute vec2 aPos;
attribute vec2 aUv;
attribute vec4 aSizeRadius;
attribute vec2 aGrad;
attribute vec4 aColor0;
attribute vec4 aColor1;

varying mediump vec2 uv;
varying mediump vec2 quadSize;
varying mediump float quadRadius;
varying mediump float quadFeather;
varying mediump vec2 grad;
varying mediump vec4 color0;
varying mediump vec4 color1;
#else
layout(location = 0) in vec2 aPos;
layout(location = 1) in vec2 aUv;
layout(location = 2) in vec4 aSizeRadius;
layout(location = 3) in vec2 aGrad;
layout(location = 4) in vec4 aColor0;
layout(location = 5) in vec4 aColor1;

out vec2 uv;
out vec2 quadSize;
out float quadRadius;
out float quadFeather;
out vec2 grad;
out vec4 color0;
out vec4 color1;
#endif

void main() {
    uv = aUv;
    quadSize = aSizeRadius.xy;
    quadRadius = aSizeRadius.z;
    quadFeather = aSizeRadius.w;
    grad = aGrad;
    color0 = aColor0;
    color1 = aColor1;
    gl_Position = vec4(aPos.x, aPos.y, 0.0, 1.0);
}
