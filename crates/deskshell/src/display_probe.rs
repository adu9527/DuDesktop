use std::fmt::Write as _;

use render::AdapterVendor;

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let topology = render::probe_displays()?;
    let mut report = String::new();
    let _ = writeln!(
        report,
        "显示拓扑：{} 个适配器，{} 个输出",
        topology.adapters.len(),
        topology.output_count()
    );
    for adapter in &topology.adapters {
        let memory = format_memory(adapter.dedicated_video_memory);
        let _ = writeln!(
            report,
            "适配器 {}：{}（{}，vendor 0x{:04x} device 0x{:04x}，LUID 0x{:016x}）",
            adapter.index,
            adapter.name,
            vendor_label(adapter.vendor),
            adapter.vendor_id,
            adapter.device_id,
            adapter.luid
        );
        let _ = writeln!(
            report,
            "  专用显存 {memory}，共享内存 {}，软件适配器：{}",
            format_memory(adapter.shared_system_memory),
            if adapter.software { "是" } else { "否" }
        );
        if adapter.outputs.is_empty() {
            let _ = writeln!(report, "  无显示输出");
        }
        for output in &adapter.outputs {
            let (left, top, right, bottom) = output.desktop_rect;
            let refresh = output
                .refresh_hz
                .map(|hz| format!("{hz} Hz"))
                .unwrap_or_else(|| "刷新率未知".into());
            let _ = writeln!(
                report,
                "  {}：{}x{} @ {}，DPI {}（{}%），位置 ({left}, {top})-({right}, {bottom})，旋转 {}{}",
                output.device_name,
                output.width,
                output.height,
                refresh,
                output.dpi_x,
                output.scale_percent(),
                output.rotation,
                if output.attached {
                    ""
                } else {
                    "，未连接桌面"
                }
            );
        }
    }
    print!("{report}");
    Ok(())
}

fn vendor_label(vendor: AdapterVendor) -> String {
    match vendor {
        AdapterVendor::Amd => "AMD".into(),
        AdapterVendor::Nvidia => "NVIDIA".into(),
        AdapterVendor::Intel => "Intel".into(),
        AdapterVendor::Other(id) => format!("其他 0x{id:04x}"),
    }
}

fn format_memory(bytes: u64) -> String {
    let mib = bytes as f64 / (1024.0 * 1024.0);
    if mib >= 1024.0 {
        format!("{:.1} GiB", mib / 1024.0)
    } else {
        format!("{mib:.0} MiB")
    }
}
