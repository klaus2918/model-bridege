fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon = std::path::Path::new("../../assets/ModelGate.ico");
    if !icon.exists() {
        println!("cargo:warning=assets/ModelGate.ico not found; exe icon skipped");
        return;
    }
    println!("cargo:rerun-if-changed={}", icon.display());
    let mut res = winres::WindowsResource::new();
    res.set_icon(icon.to_str().unwrap());
    res.set("ProductName", "Model-Gate");
    res.set("FileDescription", "Model-Gate local model gateway");
    res.set("CompanyName", "Model-Gate");
    res.set("LegalCopyright", "Model-Gate");
    res.set("FileVersion", "2.19.0.0");
    res.set("ProductVersion", "2.19.0.0");
    if let Err(err) = res.compile() {
        println!("cargo:warning=Windows resource compile failed: {err}");
    }

    emit_tray_icon(icon);
}

/// 从 ICO 中取 32x32 的 PNG 条目解出 RGBA，生成托盘图标常量。
/// 解不出时退回纯色 16x16，保证托盘可用。
fn emit_tray_icon(icon: &std::path::Path) {
    let decoded = std::fs::read(icon).ok().and_then(|data| decode_ico_png(&data));
    let (size, rgba) = match decoded {
        Some(pair) => pair,
        None => {
            println!("cargo:warning=assets/ModelGate.ico 无法解析为 PNG，托盘图标使用纯色兜底");
            (16u32, vec![30u8, 64, 175, 255].repeat(16 * 16))
        }
    };
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo");
    let dest = std::path::Path::new(&out_dir).join("tray_icon.rs");
    let mut src = String::new();
    src.push_str(&format!("pub const TRAY_ICON_SIZE: u32 = {size};\n"));
    src.push_str("pub const TRAY_ICON_RGBA: &[u8] = &[");
    for (index, byte) in rgba.iter().enumerate() {
        if index % 32 == 0 {
            src.push('\n');
        }
        src.push_str(&format!("{byte},"));
    }
    src.push_str("\n];\n");
    std::fs::write(&dest, src).expect("write tray_icon.rs");
}

fn decode_ico_png(data: &[u8]) -> Option<(u32, Vec<u8>)> {
    if data.len() < 6 {
        return None;
    }
    let count = u16::from_le_bytes([data[4], data[5]]) as usize;
    let mut best: Option<(u32, u32, u32)> = None; // (width, offset, size)
    for index in 0..count {
        let entry = 6 + index * 16;
        if data.len() < entry + 16 {
            break;
        }
        let width = data[entry] as u32;
        let size = u32::from_le_bytes([
            data[entry + 8],
            data[entry + 9],
            data[entry + 10],
            data[entry + 11],
        ]);
        let offset = u32::from_le_bytes([
            data[entry + 12],
            data[entry + 13],
            data[entry + 14],
            data[entry + 15],
        ]);
        if width == 0 {
            continue;
        }
        let start = offset as usize;
        let end = start + 8;
        if data.len() < end || data[start..end] != [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
        {
            continue;
        }
        let score = match width {
            32 => 0,
            16 => 1,
            _ => 2,
        };
        let better = match best {
            None => true,
            Some((current, _, _)) => {
                let current_score = match current {
                    32 => 0,
                    16 => 1,
                    _ => 2,
                };
                score < current_score
            }
        };
        if better {
            best = Some((width, offset, size));
        }
    }
    let (_, offset, size) = best?;
    let start = offset as usize;
    let end = start.checked_add(size as usize)?;
    let slice = data.get(start..end)?;
    let mut decoder = png::Decoder::new(slice);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).ok()?;
    if info.width == 0 || info.height != info.width {
        return None;
    }
    buffer.truncate(info.buffer_size());
    match info.color_type {
        png::ColorType::Rgba => Some((info.width, buffer)),
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity(buffer.len() / 3 * 4);
            for chunk in buffer.chunks_exact(3) {
                rgba.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
            Some((info.width, rgba))
        }
        _ => None,
    }
}
