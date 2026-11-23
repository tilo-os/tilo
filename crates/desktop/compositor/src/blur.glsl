//_DEFINES
precision highp float;
uniform sampler2D tex;
uniform float alpha;
varying vec2 v_coords;

// One pass of a separable Gaussian. The centre texel is one fetch; each tap
// past it is a pair of texels the texture's linear filter averages in one
// fetch, sampled exactly between them, so a pair weighs both texels' share
// of the kernel. `step` is one texel along the axis this pass blurs.
uniform float centre;
uniform float pair1;
uniform float pair2;
uniform float pair3;
uniform float pair4;
uniform float pair5;
uniform float pair6;
uniform float pair7;
uniform float pair8;
uniform vec2 step;

void main() {
    vec2 at = v_coords;
    vec4 sum = texture2D(tex, at) * centre;
    sum += (texture2D(tex, at + step * 1.5) + texture2D(tex, at - step * 1.5)) * pair1;
    sum += (texture2D(tex, at + step * 3.5) + texture2D(tex, at - step * 3.5)) * pair2;
    sum += (texture2D(tex, at + step * 5.5) + texture2D(tex, at - step * 5.5)) * pair3;
    sum += (texture2D(tex, at + step * 7.5) + texture2D(tex, at - step * 7.5)) * pair4;
    sum += (texture2D(tex, at + step * 9.5) + texture2D(tex, at - step * 9.5)) * pair5;
    sum += (texture2D(tex, at + step * 11.5) + texture2D(tex, at - step * 11.5)) * pair6;
    sum += (texture2D(tex, at + step * 13.5) + texture2D(tex, at - step * 13.5)) * pair7;
    sum += (texture2D(tex, at + step * 15.5) + texture2D(tex, at - step * 15.5)) * pair8;
    gl_FragColor = vec4(sum.rgb, 1.0) * alpha;
}
