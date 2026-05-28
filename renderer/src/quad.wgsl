struct U {
    res: vec2<f32>,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> u: U;

struct VIn {
    @location(0) rect: vec4<f32>,
    @location(1) color: vec4<f32>,
};

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(in: VIn, @builtin(vertex_index) vid: u32) -> VOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    let corner = corners[vid];
    let px = in.rect.xy + corner * in.rect.zw;
    let ndc = vec2<f32>(
        px.x / u.res.x * 2.0 - 1.0,
        1.0 - px.y / u.res.y * 2.0,
    );
    var out: VOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    return in.color;
}
