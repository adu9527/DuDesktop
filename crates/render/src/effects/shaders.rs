//! 图片效果 HLSL。源码内嵌，运行时由系统 `d3dcompiler_47.dll` 编译。
//!
//! 所有像素着色器共用 48 字节常量（3 个 `float4`）：
//! - `p0.x`：效果强度 0..1；`p0.y`：动画时间（秒）；`p0.z` / `p0.w`：输出尺寸；
//! - `p1.x` / `p1.y`：输入纹素尺寸；`p1.z` / `p1.w`：源图 UV 缩放（负值表示 contain 黑边）；
//! - `p2.x` / `p2.y`：可分离模糊方向；`p2.z` / `p2.w`：平铺次数（0 表示不平铺）。
//!
//! `FrontTexture` 是当前主纹理，`SecondaryTexture` 只用于 Bloom 合成。

pub const VS_MAIN: &str = "VSMain";
pub const PS_COLOR: &str = "PSColor";
pub const PS_VIGNETTE: &str = "PSVignette";
pub const PS_BLUR: &str = "PSSeparableBlur";
pub const PS_BLOOM_THRESHOLD: &str = "PSBloomThreshold";
pub const PS_BLOOM_COMPOSITE: &str = "PSBloomComposite";
pub const PS_PARTICLES: &str = "PSParticles";
pub const PS_GRADIENT: &str = "PSGradient";
pub const PS_OUTPUT: &str = "PSOutput";

pub const SOURCE: &str = r#"
Texture2D FrontTexture : register(t0);
Texture2D SecondaryTexture : register(t1);
SamplerState LinearSampler : register(s0);

cbuffer Params : register(b0)
{
    float4 p0;
    float4 p1;
    float4 p2;
};

struct VSOut
{
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
};

VSOut VSMain(uint vertex_id : SV_VertexID)
{
    VSOut output;
    float2 uv = float2((vertex_id << 1) & 2, vertex_id & 2);
    output.uv = uv;
    output.position = float4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    return output;
}

float3 SampleSource(float2 uv)
{
    float2 scale = float2(p1.z, p1.w);
    float2 source_uv = (uv - 0.5) * abs(scale) + 0.5;
    if (p2.z > 0.0)
    {
        // 平铺：按源图原生像素尺寸重复。
        source_uv = frac(uv * float2(p2.z, p2.w));
    }
    else if (scale.x < 0.0 || scale.y < 0.0)
    {
        // 适应：窗口外为黑边。
        if (any(source_uv < 0.0) || any(source_uv > 1.0))
        {
            return float3(0.0, 0.0, 0.0);
        }
    }
    return FrontTexture.Sample(LinearSampler, source_uv).rgb;
}

float3 SampleFront(float2 uv)
{
    return FrontTexture.Sample(LinearSampler, uv).rgb;
}

float Luma(float3 color)
{
    return dot(color, float3(0.2126, 0.7152, 0.0722));
}

float4 PSColor(VSOut input) : SV_Target
{
    float3 color = SampleSource(input.uv);
    float strength = p0.x;
    float gray = Luma(color);
    color = lerp(float3(gray, gray, gray), color, 1.0 + strength * 0.55);
    color = saturate((color - 0.5) * (1.0 + strength * 0.22) + 0.5);
    color += float3(0.045, 0.014, -0.032) * strength;
    return float4(saturate(color), 1.0);
}

float4 PSVignette(VSOut input) : SV_Target
{
    float3 color = SampleFront(input.uv);
    float aspect = p0.z / max(p0.w, 1.0);
    float2 delta = (input.uv - 0.5) * float2(aspect, 1.0);
    float falloff = smoothstep(0.32, 0.95, length(delta) * 1.25);
    return float4(color * (1.0 - p0.x * 0.85 * falloff), 1.0);
}

float4 PSSeparableBlur(VSOut input) : SV_Target
{
    float3 color = SampleFront(input.uv) * 0.227027;
    float2 direction = p2.xy;
    float radius = 1.0 + p0.x * 4.0;
    [unroll]
    for (int index = 1; index < 5; index++)
    {
        float weight = index == 1 ? 0.1945946 : (index == 2 ? 0.1216216 : (index == 3 ? 0.0540540 : 0.0162162));
        float2 offset = direction * radius * index;
        color += SampleFront(input.uv + offset) * weight;
        color += SampleFront(input.uv - offset) * weight;
    }
    return float4(color, 1.0);
}

float4 PSBloomThreshold(VSOut input) : SV_Target
{
    float3 color = SampleFront(input.uv);
    float threshold = lerp(0.80, 0.55, p0.x);
    float3 bright = max(color - threshold, 0.0);
    return float4(bright / max(1.0 - threshold, 0.05), 1.0);
}

float4 PSBloomComposite(VSOut input) : SV_Target
{
    float3 base = SampleFront(input.uv);
    float3 bloom = SecondaryTexture.Sample(LinearSampler, input.uv).rgb;
    return float4(base + bloom * p0.x * 0.9, 1.0);
}

float4 PSParticles(VSOut input) : SV_Target
{
    float3 color = SampleFront(input.uv);
    float aspect = p0.z / max(p0.w, 1.0);
    float time = p0.y;
    [unroll]
    for (int index = 0; index < 10; index++)
    {
        float fi = (float)index;
        float seed_x = frac(sin(fi * 12.9898) * 43758.5453);
        float seed_y = frac(cos(fi * 78.233) * 12345.6789);
        float2 center = float2(frac(seed_x + time * (0.008 + fi * 0.0015)),
                               frac(seed_y + time * (0.005 + fi * 0.0011)));
        float2 delta = (input.uv - center) * float2(aspect, 1.0);
        float radius = 0.008 + 0.006 * frac(sin(fi * 3.7) * 100.0);
        float glow = smoothstep(radius, 0.0, length(delta));
        float twinkle = 0.65 + 0.35 * sin(time * 2.0 + fi * 1.7);
        color += glow * twinkle * float3(1.0, 0.78, 0.45) * p0.x * 1.1;
    }
    return float4(color, 1.0);
}

float4 PSGradient(VSOut input) : SV_Target
{
    float3 base = SampleFront(input.uv);
    float wave = 0.12 * sin(p0.y * 0.25 + input.uv.y * 3.0);
    float blend = saturate(input.uv.x * 0.7 + input.uv.y * 0.3 + wave);
    float3 tint = lerp(float3(0.02, 0.06, 0.16), float3(0.18, 0.05, 0.14), blend);
    return float4(lerp(base, base * 0.55 + tint, p0.x * 0.55), 1.0);
}

float4 PSOutput(VSOut input) : SV_Target
{
    return float4(SampleSource(input.uv), 1.0);
}
"#;
