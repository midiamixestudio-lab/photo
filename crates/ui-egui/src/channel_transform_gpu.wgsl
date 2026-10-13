// Native samples stay immutable. A transformed scalar is merged into just one component.
struct Params {
    h0: vec4<f32>, h1: vec4<f32>, h2: vec4<f32>,
    size: vec4<f32>, rect: vec4<f32>, view: vec4<f32>, opts: vec4<f32>,
}
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var selection: texture_2d<f32>;
@group(1) @binding(0) var moved: texture_2d<f32>;
struct Vertex { @builtin(position) position: vec4<f32>, @location(0) src: vec2<f32> }
@vertex fn vs_full(@builtin(vertex_index) i: u32) -> Vertex {
    let uv = vec2(f32((i << 1u) & 2u), f32(i & 2u));
    return Vertex(vec4(uv * vec2(2.0, -2.0) + vec2(-1.0, 1.0), 0.0, 1.0), uv * p.size.xy);
}
@vertex fn vs_mesh(@location(0) data: vec4<f32>) -> Vertex {
    return Vertex(vec4(data.xy / p.size.xy * vec2(2.0, -2.0) + vec2(-1.0, 1.0), 0.0, 1.0), data.zw);
}
fn quant(x: f32) -> f32 {
    if p.opts.z == 0.0 { return x; }
    return floor(clamp(x, 0.0, 1.0) * p.opts.z + 0.5) / p.opts.z;
}
fn lifted(at: vec2<i32>) -> vec2<f32> {
    let xy = vec2<f32>(at);
    if any(at < vec2(0)) || any(at >= vec2<i32>(p.size.xy)) || any(xy < p.rect.xy) || any(xy >= p.rect.zw) { return vec2(0.0); }
    let alpha = quant(textureLoad(selection, at, 0).r);
    return vec2(textureLoad(source, at, 0)[u32(p.size.z)] * alpha, alpha);
}
fn cubic(t: f32) -> vec4<f32> {
    let t2 = t * t; let t3 = t2 * t;
    // Match transform::bicubic: Keys a = -0.75, including Warp's scalar sampler.
    return vec4(-0.75*t3+1.5*t2-0.75*t, 1.25*t3-2.25*t2+1.0, -1.25*t3+1.5*t2+0.75*t, 0.75*t3-0.75*t2);
}
fn sample_at(at: vec2<f32>) -> vec2<f32> {
    if p.opts.x == 0.0 { return lifted(vec2<i32>(floor(at + 0.5))); }
    let pos = vec2<i32>(floor(at)); let frac = fract(at);
    var value = vec2(0.0);
    if p.opts.x == 1.0 {
        value = mix(mix(lifted(pos), lifted(pos + vec2(1,0)), frac.x), mix(lifted(pos + vec2(0,1)), lifted(pos + vec2(1,1)), frac.x), frac.y);
    } else {
        let wx = cubic(frac.x); let wy = cubic(frac.y);
        for (var j = 0; j < 4; j++) {
            for (var i = 0; i < 4; i++) { value += lifted(pos + vec2(i-1,j-1)) * wx[u32(i)] * wy[u32(j)]; }
        }
    }
    let coverage = clamp(value.y, 0.0, 1.0);
    let a = quant(coverage);
    var gray = 0.0;
    if coverage > 0.000001 { gray = quant(value.x / coverage); }
    return vec2(gray * a, a);
}
fn srgb(c: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(max(c, vec3(0.0)), vec3(1.0/2.4)) - 0.055, c * 12.92, c <= vec3(0.0031308));
}
fn finish(at: vec2<i32>, transformed: vec2<f32>) -> vec4<f32> {
    var rgba = textureLoad(source, at, 0);
    let c = u32(p.size.z); let weight = textureLoad(selection, at, 0).r;
    let rest = quant(rgba[c] * (1.0-weight) + p.size.w * weight);
    if p.opts.w == 0.0 { rgba[c] = quant(transformed.x + rest * (1.0-transformed.y)); }
    rgba.a = clamp(rgba.a * p.view.z, 0.0, 1.0);
    let visible = u32(p.view.x);
    if visible != 7u {
        var rgb = rgba.rgb * rgba.a + vec3(1.0-rgba.a);
        if visible == 0u { return vec4(1.0); }
        if countOneBits(visible) == 1u && p.view.y == 0.0 {
            return vec4(vec3(rgb[firstTrailingBit(visible)]), 1.0);
        }
        for (var i = 0u; i < 3u; i++) { if (visible & (1u << i)) == 0u { rgb[i] = 0.0; } }
        return vec4(clamp(rgb, vec3(0.0), vec3(1.0)), 1.0);
    }
    if p.view.w != 0.0 { rgba = vec4(srgb(rgba.rgb), rgba.a); }
    // Match the CPU display's 8-bit straight samples before premultiplication.
    rgba = floor(clamp(rgba, vec4(0.0), vec4(1.0)) * 255.0 + 0.5) / 255.0;
    return vec4(rgba.rgb * rgba.a, rgba.a);
}
@fragment fn fs_affine(v: Vertex) -> @location(0) vec4<f32> {
    let dst = vec3(v.position.xy, 1.0);
    let denom = dot(p.h2.xyz, dst);
    let src = vec2(dot(p.h0.xyz, dst), dot(p.h1.xyz, dst)) / denom - 0.5;
    return finish(vec2<i32>(v.position.xy), sample_at(src));
}
@fragment fn fs_warp(v: Vertex) -> @location(0) vec4<f32> { return vec4(sample_at(v.src - 0.5), 0.0, 0.0); }
@fragment fn fs_finish(v: Vertex) -> @location(0) vec4<f32> {
    let at = vec2<i32>(v.position.xy);
    return finish(at, textureLoad(moved, at, 0).xy);
}
