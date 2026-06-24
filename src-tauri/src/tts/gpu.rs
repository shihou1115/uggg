//! Irodori-TTS の GPU 判定 (architecture §8.6, M4c Phase B)。
//!
//! Windows DXGI で物理 GPU を列挙し、NVIDIA 製 (VendorId=0x10DE) が見つかれば
//! Irodori 用 CUDA 推論が「動く可能性が高い」と扱う (available=true)。
//! 実際の CUDA 利用可否は Phase D のサイドカー起動時に `torch.cuda.is_available()` で
//! 最終確認するため、ここは事前フィルタにとどめる。
//!
//! 設計判断:
//! - nvml / cudart を呼ばない: 追加 DLL 同梱が不要、AV 検出リスクなし
//! - DXGI は VendorId と Description が確実に取れる軽量 API
//! - ソフトウェアアダプタ (Microsoft Basic Render Driver 等) は除外

/// NVIDIA の DXGI VendorId。
const VENDOR_ID_NVIDIA: u32 = 0x10DE;

/// DXGI で列挙された 1 GPU 分のメタ (フロントへ返すための中間表現)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DxgiAdapter {
    pub vendor_id: u32,
    pub description: String,
    /// `DXGI_ADAPTER_FLAG_SOFTWARE` が立っているか (Microsoft Basic Render Driver 等)。
    pub software: bool,
}

/// 与えられたアダプタ一覧から Irodori 利用可否を判定する pure 関数。
/// テストできるようにこの判定だけ切り出す。
pub fn pick_irodori_gpu(adapters: &[DxgiAdapter]) -> IrodoriPick {
    for a in adapters {
        if a.software {
            continue;
        }
        if a.vendor_id == VENDOR_ID_NVIDIA {
            return IrodoriPick::Found {
                name: a.description.clone(),
            };
        }
    }
    // NVIDIA が見つからない場合は理由を組み立てる
    let hw = adapters.iter().find(|a| !a.software);
    match hw {
        Some(a) => IrodoriPick::NotNvidia {
            name: a.description.clone(),
        },
        None => IrodoriPick::NoHardwareGpu,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrodoriPick {
    /// NVIDIA GPU が見つかった (Irodori 用 CUDA 推論が利用可能と推定)。
    Found { name: String },
    /// ハードウェア GPU はあるが NVIDIA 以外 (AMD / Intel 内蔵 等)。
    NotNvidia { name: String },
    /// 物理 GPU が一切見つからない (ソフトウェアアダプタのみ)。
    NoHardwareGpu,
}

/// DXGI で実際に GPU を列挙する。Windows でのみコンパイル。
#[cfg(windows)]
pub fn list_adapters() -> Result<Vec<DxgiAdapter>, String> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_DESC1,
        DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND,
    };

    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("DXGI factory 作成失敗: {e}"))?;
        let mut out = Vec::new();
        let mut i: u32 = 0;
        loop {
            let res = factory.EnumAdapters1(i);
            let adapter: IDXGIAdapter1 = match res {
                Ok(a) => a,
                Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(err) => return Err(format!("EnumAdapters1({i}) 失敗: {err}")),
            };
            i += 1;

            let desc: DXGI_ADAPTER_DESC1 = match adapter.GetDesc1() {
                Ok(d) => d,
                Err(_) => continue,
            };
            let description = String::from_utf16_lossy(
                &desc
                    .Description
                    .iter()
                    .take_while(|c| **c != 0)
                    .copied()
                    .collect::<Vec<u16>>(),
            );
            let software = (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0;
            out.push(DxgiAdapter {
                vendor_id: desc.VendorId,
                description,
                software,
            });
        }
        Ok(out)
    }
}

/// Windows 以外は GPU 列挙 API がないので常に空。Phase A の互換性のため。
#[cfg(not(windows))]
pub fn list_adapters() -> Result<Vec<DxgiAdapter>, String> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ad(vendor: u32, name: &str, software: bool) -> DxgiAdapter {
        DxgiAdapter {
            vendor_id: vendor,
            description: name.to_string(),
            software,
        }
    }

    #[test]
    fn picks_nvidia_over_other_hardware() {
        let list = vec![
            ad(0x8086, "Intel(R) UHD Graphics", false),
            ad(VENDOR_ID_NVIDIA, "NVIDIA GeForce RTX 4070", false),
        ];
        match pick_irodori_gpu(&list) {
            IrodoriPick::Found { name } => assert!(name.contains("NVIDIA")),
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn reports_not_nvidia_when_only_other_hw() {
        let list = vec![ad(0x8086, "Intel(R) UHD Graphics", false)];
        assert_eq!(
            pick_irodori_gpu(&list),
            IrodoriPick::NotNvidia {
                name: "Intel(R) UHD Graphics".to_string()
            }
        );
    }

    #[test]
    fn reports_no_hardware_when_only_software() {
        let list = vec![ad(0x1414, "Microsoft Basic Render Driver", true)];
        assert_eq!(pick_irodori_gpu(&list), IrodoriPick::NoHardwareGpu);
    }

    #[test]
    fn empty_list_means_no_hardware() {
        assert_eq!(pick_irodori_gpu(&[]), IrodoriPick::NoHardwareGpu);
    }
}
