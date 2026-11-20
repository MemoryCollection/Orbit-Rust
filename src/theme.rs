//! 外观主题：把颜色 / 圆角 / 描边这类"看得到"的参数抽成一套，运行时可切换。
//!
//! 之前这些值都是 `config.rs` 里的 `pub const`，换一种风格得改一堆常量再重编。
//! 现在集中到 `Theme`：绘制代码通过 `theme::cur()` 取当前主题，设置页换主题只
//! 需写入一个索引，下一帧所有窗口自然重绘成新样式。
//!
//! 颜色一律是 `0xAARRGGBB`（与 Win32 ARGB 一致，`render::premul_from_argb` 直读）。
//! 注意本项目是**逐像素 alpha 的分层窗口**，拿不到桌面像素，所以"毛玻璃"只能靠
//! 半透明底 + 内高光 + 细亮边模拟，没有真正的背景模糊。

use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy)]
pub struct Theme {
    /// 存进 `settings.txt` 的键（英文、稳定，不随显示名变）
    pub key: &'static str,
    /// 设置页里显示的名字
    pub name: &'static str,
    /// 深色底（决定 egui 的 Dark / Light 视觉、图标衬底等配套逻辑）
    pub dark: bool,
    /// 是否给形状描一圈外发光（霓虹风格用）
    pub glow: bool,

    // ---- 胶囊 / 通用文字 ----
    pub pill_bg: u32,
    pub text: u32,
    pub text_dim: u32,
    pub text_body: u32,
    pub icon_dim: u32,
    pub divider: u32,
    pub hover: u32,

    // ---- 展开面板 ----
    pub panel_bg: u32,
    pub panel_border: u32,
    pub accent: u32,

    // ---- 底部 Dock ----
    pub dock_edge: u32,
    pub dock_glow: u32,
    pub dock_hover: u32,
    pub dock_hover_edge: u32,
    pub dock_desktop: u32,
    pub dock_desktop_hover: u32,
    /// Dock 图标的衬底（浅色主题下彩色图标直接压在浅底上会发脏，垫一层白圆角）
    /// `0` = 不画
    pub icon_backing: u32,

    // ---- 悬停提示条 ----
    pub tip_bg: u32,
    pub tip_text: u32,
    /// 固定标记星标
    pub pin: u32,

    // ---- 天气图标语义色 ----
    pub sun: u32,
    pub moon: u32,
    pub rain: u32,
    pub cloud: u32,

    // ---- 圆角（逻辑点）----
    pub dock_radius: f32,
    pub panel_radius: f32,
    pub tip_radius: f32,
}

/// 深色玻璃：近黑偏冷，两级亮边做出体积感。项目的基准风格。
pub const GLASS: Theme = Theme {
    key: "glass",
    name: "深色玻璃",
    dark: true,
    glow: false,
    pill_bg: 0xD2_19_1C_24,
    text: 0xFF_FF_FF_FF,
    text_dim: 0xC4_FF_FF_FF,
    text_body: 0xE8_FF_FF_FF,
    icon_dim: 0xC0_FF_FF_FF,
    divider: 0x33_FF_FF_FF,
    hover: 0x38_FF_FF_FF,
    panel_bg: 0xFF_1A_1D_24,
    panel_border: 0x38_FF_FF_FF,
    accent: 0xFF_5A_A9_FF,
    dock_edge: 0x30_FF_FF_FF,
    dock_glow: 0x1C_FF_FF_FF,
    dock_hover: 0x50_FF_FF_FF,
    dock_hover_edge: 0x46_FF_FF_FF,
    dock_desktop: 0x52_FF_FF_FF,
    dock_desktop_hover: 0xE0_FF_FF_FF,
    icon_backing: 0,
    tip_bg: 0xF2_1A_1D_24,
    tip_text: 0xFF_FF_FF_FF,
    pin: 0xFF_FF_D5_4F,
    sun: 0xFF_FF_C4_54,
    moon: 0xFF_BC_D4_FF,
    rain: 0xFF_86_B8_F0,
    cloud: 0xFF_C9_CF_D6,
    dock_radius: 28.0,
    panel_radius: 14.0,
    tip_radius: 8.0,
};

/// 液态玻璃：更透的冷灰蓝底，亮边更抢眼，圆角更大。
pub const LIQUID: Theme = Theme {
    key: "liquid",
    name: "液态玻璃",
    dark: true,
    glow: false,
    pill_bg: 0xA6_3D_48_58,
    text: 0xFF_FF_FF_FF,
    text_dim: 0xD4_FF_FF_FF,
    text_body: 0xF2_FF_FF_FF,
    icon_dim: 0xD0_FF_FF_FF,
    divider: 0x40_FF_FF_FF,
    hover: 0x48_FF_FF_FF,
    panel_bg: 0xF5_23_2A_34,
    panel_border: 0x4A_FF_FF_FF,
    accent: 0xFF_0A_84_FF,
    dock_edge: 0x5C_FF_FF_FF,
    dock_glow: 0x2E_FF_FF_FF,
    dock_hover: 0x60_FF_FF_FF,
    dock_hover_edge: 0x54_FF_FF_FF,
    dock_desktop: 0x60_FF_FF_FF,
    dock_desktop_hover: 0xE6_FF_FF_FF,
    icon_backing: 0,
    tip_bg: 0xF5_2C_35_42,
    tip_text: 0xFF_FF_FF_FF,
    pin: 0xFF_FF_D6_0A,
    sun: 0xFF_FF_D1_66,
    moon: 0xFF_C6_DA_FF,
    rain: 0xFF_9C_C7_FF,
    cloud: 0xFF_D5_DC_E6,
    dock_radius: 28.0,
    panel_radius: 18.0,
    tip_radius: 12.0,
};

/// 云母浅色：Win11 那套。浅半透明底 + 深色字 + 小圆角 + 彩色图标白衬底。
pub const MICA: Theme = Theme {
    key: "mica",
    name: "云母浅色",
    dark: false,
    glow: false,
    pill_bg: 0xE2_F4_F4_F4,
    text: 0xFF_1B_1B_1B,
    text_dim: 0xA8_1B_1B_1B,
    text_body: 0xE8_1B_1B_1B,
    icon_dim: 0xB8_1B_1B_1B,
    divider: 0x1E_00_00_00,
    hover: 0x14_00_00_00,
    panel_bg: 0xFF_F9_F9_F9,
    panel_border: 0x1E_00_00_00,
    accent: 0xFF_00_67_C0,
    dock_edge: 0x22_00_00_00,
    dock_glow: 0x0E_FF_FF_FF,
    dock_hover: 0x1C_00_00_00,
    dock_hover_edge: 0x26_00_00_00,
    dock_desktop: 0x40_00_00_00,
    dock_desktop_hover: 0xC4_00_00_00,
    icon_backing: 0xE0_FF_FF_FF,
    tip_bg: 0xF8_FA_FA_FA,
    tip_text: 0xFF_1B_1B_1B,
    pin: 0xFF_C7_7A_00,
    sun: 0xFF_E8_A1_00,
    moon: 0xFF_5B_7F_B5,
    rain: 0xFF_2F_7D_C4,
    cloud: 0xFF_7A_84_8E,
    dock_radius: 14.0,
    panel_radius: 10.0,
    tip_radius: 8.0,
};

/// 纸质极简：接近不透明的米白纸片，圆角小、几乎无描边。
pub const PAPER: Theme = Theme {
    key: "paper",
    name: "纸质极简",
    dark: false,
    glow: false,
    pill_bg: 0xF4_FB_FA_F7,
    text: 0xFF_2A_28_22,
    text_dim: 0x99_2A_28_22,
    text_body: 0xE6_2A_28_22,
    icon_dim: 0xA6_2A_28_22,
    divider: 0x16_00_00_00,
    hover: 0x0E_00_00_00,
    panel_bg: 0xFF_FD_FC_F9,
    panel_border: 0x14_00_00_00,
    accent: 0xFF_8C_6A_3F,
    dock_edge: 0x12_00_00_00,
    dock_glow: 0,
    dock_hover: 0x12_00_00_00,
    dock_hover_edge: 0x1C_00_00_00,
    dock_desktop: 0x34_00_00_00,
    dock_desktop_hover: 0xB0_00_00_00,
    icon_backing: 0xF0_FF_FF_FF,
    tip_bg: 0xFA_FD_FC_F9,
    tip_text: 0xFF_2A_28_22,
    pin: 0xFF_B8_86_2B,
    sun: 0xFF_D9_A2_3A,
    moon: 0xFF_6E_7E_9E,
    rain: 0xFF_5B_8B_B5,
    cloud: 0xFF_8D_8A_80,
    dock_radius: 6.0,
    panel_radius: 6.0,
    tip_radius: 4.0,
};

/// 霓虹赛博：近黑紫底 + 青色描边 + 品红点缀，配外发光。
pub const NEON: Theme = Theme {
    key: "neon",
    name: "霓虹赛博",
    dark: true,
    glow: true,
    pill_bg: 0xE0_0B_06_16,
    text: 0xFF_00_E5_FF,
    text_dim: 0xC0_00_E5_FF,
    text_body: 0xFF_E8_F6_FF,
    icon_dim: 0xD0_FF_2E_97,
    divider: 0x66_00_E5_FF,
    hover: 0x33_00_E5_FF,
    panel_bg: 0xFF_0B_06_16,
    panel_border: 0x88_00_E5_FF,
    accent: 0xFF_FF_2E_97,
    dock_edge: 0xAA_00_E5_FF,
    dock_glow: 0x22_FF_2E_97,
    dock_hover: 0x44_00_E5_FF,
    dock_hover_edge: 0xAA_00_E5_FF,
    dock_desktop: 0x80_00_E5_FF,
    dock_desktop_hover: 0xFF_00_E5_FF,
    icon_backing: 0,
    tip_bg: 0xF5_0B_06_16,
    tip_text: 0xFF_00_E5_FF,
    pin: 0xFF_FF_2E_97,
    sun: 0xFF_FF_D4_26,
    moon: 0xFF_7C_E8_FF,
    rain: 0xFF_00_B8_FF,
    cloud: 0xFF_9A_A7_C7,
    dock_radius: 6.0,
    panel_radius: 6.0,
    tip_radius: 4.0,
};

/// 硬边终端：纯黑直角 + 白硬边 + 绿色强调。
pub const TERMINAL: Theme = Theme {
    key: "terminal",
    name: "硬边终端",
    dark: true,
    glow: false,
    pill_bg: 0xF0_00_00_00,
    text: 0xFF_7E_E7_87,
    text_dim: 0xC0_7E_E7_87,
    text_body: 0xFF_E6_ED_E6,
    icon_dim: 0xC0_E6_ED_E6,
    divider: 0x66_7E_E7_87,
    hover: 0x22_7E_E7_87,
    panel_bg: 0xFF_0A_0F_0A,
    panel_border: 0x99_7E_E7_87,
    accent: 0xFF_7E_E7_87,
    dock_edge: 0xCC_FF_FF_FF,
    dock_glow: 0,
    dock_hover: 0x22_7E_E7_87,
    dock_hover_edge: 0x88_7E_E7_87,
    dock_desktop: 0x99_FF_FF_FF,
    dock_desktop_hover: 0xFF_FF_FF_FF,
    icon_backing: 0,
    tip_bg: 0xFF_00_00_00,
    tip_text: 0xFF_7E_E7_87,
    pin: 0xFF_7E_E7_87,
    sun: 0xFF_FF_C4_54,
    moon: 0xFF_BC_D4_FF,
    rain: 0xFF_86_B8_F0,
    cloud: 0xFF_C9_CF_D6,
    dock_radius: 0.0,
    panel_radius: 0.0,
    tip_radius: 0.0,
};

/// 全部内置主题（顺序即设置页下拉的顺序）
pub const ALL: &[Theme] = &[GLASS, LIQUID, MICA, PAPER, NEON, TERMINAL];

/// 当前主题下标。绘制路径每帧大量读取，用原子量做无锁读。
static CUR: AtomicUsize = AtomicUsize::new(0);

/// 取当前主题。返回 `'static` 引用，调用方可随意持有。
#[inline]
pub fn cur() -> &'static Theme {
    let i = CUR.load(Ordering::Relaxed);
    &ALL[if i < ALL.len() { i } else { 0 }]
}

/// 按下标切换（超界会被夹到 0）。只改内存，不落盘——落盘由 `settings` 负责。
pub fn set_index(i: usize) {
    CUR.store(if i < ALL.len() { i } else { 0 }, Ordering::Relaxed);
}

/// 当前主题下标
#[inline]
pub fn index() -> usize {
    let i = CUR.load(Ordering::Relaxed);
    if i < ALL.len() {
        i
    } else {
        0
    }
}

/// 按 key 找下标（读 `settings.txt` 时用）
pub fn index_of(key: &str) -> Option<usize> {
    let key = key.trim();
    ALL.iter().position(|t| t.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_findable() {
        for (i, t) in ALL.iter().enumerate() {
            assert_eq!(index_of(t.key), Some(i), "key 反查不到：{}", t.key);
        }
        let n = ALL.len();
        assert_eq!(ALL.iter().filter(|t| t.key == ALL[0].key).count(), 1);
        assert!(n >= 4);
    }

    #[test]
    fn set_index_clamps() {
        set_index(9999);
        assert_eq!(index(), 0);
        set_index(1);
        assert_eq!(index(), 1);
        set_index(0);
        assert_eq!(cur().key, "glass");
    }

    /// 圆角不能为负、透明色不能全透明（否则整个窗口看不见）
    #[test]
    fn theme_values_are_sane() {
        for t in ALL {
            assert!(t.dock_radius >= 0.0 && t.panel_radius >= 0.0 && t.tip_radius >= 0.0);
            assert_ne!(t.pill_bg >> 24, 0, "{} 的胶囊底色全透明了", t.key);
            assert_ne!(t.panel_bg >> 24, 0, "{} 的面板底全透明了", t.key);
            assert_ne!(t.text & 0xFF_FF_FF, 0, "{} 的文字全黑看不见", t.key);
        }
    }
}
