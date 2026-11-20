//! 外观主题：把颜色 / 圆角 / 描边这类"看得到"的参数抽成一套，运行时可切换。
//!
//! 之前这些值都是 `config.rs` 里的 `pub const`，换一种风格得改一堆常量再重编。
//! 现在集中到 `Theme`：绘制代码通过 `theme::cur()` 取当前主题，设置页换主题只
//! 需写入一个索引，下一帧所有窗口自然重绘成新样式。
//!
//! 颜色一律是 `0xAARRGGBB`（与 Win32 ARGB 一致，`render::premul_from_argb` 直读）。
//! 注意本项目是**逐像素 alpha 的分层窗口**，拿不到桌面像素，所以"毛玻璃"只能靠
//! 半透明底 + 内高光 + 细亮边模拟，没有真正的背景模糊。
//!
//! 内置三套风格，每套都有深色 + 浅色两个变体：
//! - **Calm UI**（平静极简）：低饱和柔和暖色、超大留白、弱阴影/装饰、干净字重层级。
//! - **Glassmorphism 2.0**（改良玻璃拟态）：克制毛玻璃——低模糊、高对比、细描边。
//! - **Material 3**（动态取色）：大圆角、卡片分层、强对比强调色、涟漪反馈。

use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy)]
pub struct Theme {
    /// 存进 `settings.txt` 的键（英文、稳定，不随显示名变）
    pub key: &'static str,
    /// 设置页里显示的名字
    pub name: &'static str,
    /// 深色底（决定 egui 的 Dark / Light 视觉、图标衬底等配套逻辑）
    pub dark: bool,
    /// 是否给形状描一圈外发光（霓虹风格用；当前主题都不用）
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

// =====================================================================
//  Calm UI —— 平静极简
//  低饱和柔和暖色（奶油/米色/陶土棕/鼠尾草绿），大圆角，弱阴影，干净字重。
// =====================================================================

/// Calm UI · 浅色：奶油白底、暖褐文字、陶土棕点缀。
pub const CALM_LIGHT: Theme = Theme {
    key: "calm_light",
    name: "平静 · 浅色",
    dark: false,
    glow: false,
    pill_bg: 0xF5_FA_F7_F2,
    text: 0xFF_3A_36_30,
    text_dim: 0x8C_3A_36_30,
    text_body: 0xE6_3A_36_30,
    icon_dim: 0xA0_3A_36_30,
    divider: 0x12_3A_36_30,
    hover: 0x0E_5A_4A_38,
    panel_bg: 0xFF_FB_F9_F5,
    panel_border: 0x14_5A_4A_38,
    accent: 0xFF_B0_7A_4E,
    dock_edge: 0x18_3A_36_30,
    dock_glow: 0x08_FF_FF_FF,
    dock_hover: 0x10_5A_4A_38,
    dock_hover_edge: 0x20_5A_4A_38,
    dock_desktop: 0x30_3A_36_30,
    dock_desktop_hover: 0xB8_3A_36_30,
    icon_backing: 0xF2_FF_FF_FF,
    tip_bg: 0xF8_FB_F9_F5,
    tip_text: 0xFF_3A_36_30,
    pin: 0xFF_B0_7A_4E,
    sun: 0xFF_D8_A8_5C,
    moon: 0xFF_8A_8C_A0,
    rain: 0xFF_7F_A0_B8,
    cloud: 0xFF_A8_A4_98,
    dock_radius: 24.0,
    panel_radius: 18.0,
    tip_radius: 12.0,
};

/// Calm UI · 深色：暖灰绿暗底、米白文字、柔和陶土棕点缀。
pub const CALM_DARK: Theme = Theme {
    key: "calm_dark",
    name: "平静 · 深色",
    dark: true,
    glow: false,
    pill_bg: 0xEB_28_2A_26,
    text: 0xFF_E8_E2_D6,
    text_dim: 0x9E_E8_E2_D6,
    text_body: 0xE8_E8_E2_D6,
    icon_dim: 0xB0_E8_E2_D6,
    divider: 0x14_E8_E2_D6,
    hover: 0x12_E8_E2_D6,
    panel_bg: 0xFF_22_24_20,
    panel_border: 0x1E_E8_E2_D6,
    accent: 0xFF_D0_9A_6E,
    dock_edge: 0x20_E8_E2_D6,
    dock_glow: 0x0C_FF_FF_FF,
    dock_hover: 0x16_E8_E2_D6,
    dock_hover_edge: 0x28_E8_E2_D6,
    dock_desktop: 0x3C_E8_E2_D6,
    dock_desktop_hover: 0xD0_E8_E2_D6,
    icon_backing: 0,
    tip_bg: 0xF2_26_28_24,
    tip_text: 0xFF_E8_E2_D6,
    pin: 0xFF_D0_9A_6E,
    sun: 0xFF_E2_BE_80,
    moon: 0xFF_B4_B8_CC,
    rain: 0xFF_94_B4_C8,
    cloud: 0xFF_B6_B0_A4,
    dock_radius: 24.0,
    panel_radius: 18.0,
    tip_radius: 12.0,
};

// =====================================================================
//  Glassmorphism 2.0 —— 改良玻璃拟态
//  克制版毛玻璃：低模糊、高对比、细描边，不再满屏糊。
// =====================================================================

/// Glassmorphism 2.0 · 深色：冷灰半透明底 + 高对比细亮边。
pub const GLASS2_DARK: Theme = Theme {
    key: "glass2_dark",
    name: "玻璃 2.0 · 深色",
    dark: true,
    glow: false,
    pill_bg: 0xC0_16_18_1E,
    text: 0xFF_F4_F6_F8,
    text_dim: 0xC8_F4_F6_F8,
    text_body: 0xEE_F4_F6_F8,
    icon_dim: 0xC4_F4_F6_F8,
    divider: 0x2C_FF_FF_FF,
    hover: 0x2E_FF_FF_FF,
    panel_bg: 0xF0_1C_1E_24,
    panel_border: 0x50_FF_FF_FF,
    accent: 0xFF_6E_A8_FF,
    dock_edge: 0x40_FF_FF_FF,
    dock_glow: 0x18_FF_FF_FF,
    dock_hover: 0x34_FF_FF_FF,
    dock_hover_edge: 0x40_FF_FF_FF,
    dock_desktop: 0x48_FF_FF_FF,
    dock_desktop_hover: 0xDC_FF_FF_FF,
    icon_backing: 0,
    tip_bg: 0xF2_20_22_2A,
    tip_text: 0xFF_F4_F6_F8,
    pin: 0xFF_FF_D5_4F,
    sun: 0xFF_FF_C9_60,
    moon: 0xFF_C0_D6_FF,
    rain: 0xFF_8C_BE_F4,
    cloud: 0xFF_C6_CD_D6,
    dock_radius: 20.0,
    panel_radius: 16.0,
    tip_radius: 10.0,
};

/// Glassmorphism 2.0 · 浅色：白半透明底 + 深字高对比细描边。
pub const GLASS2_LIGHT: Theme = Theme {
    key: "glass2_light",
    name: "玻璃 2.0 · 浅色",
    dark: false,
    glow: false,
    pill_bg: 0xC8_FF_FF_FF,
    text: 0xFF_1C_1E_22,
    text_dim: 0xA6_1C_1E_22,
    text_body: 0xEC_1C_1E_22,
    icon_dim: 0xB4_1C_1E_22,
    divider: 0x1C_00_00_00,
    hover: 0x16_00_00_00,
    panel_bg: 0xF0_FF_FF_FF,
    panel_border: 0x30_00_00_00,
    accent: 0xFF_2F_6B_E0,
    dock_edge: 0x26_00_00_00,
    dock_glow: 0x0C_FF_FF_FF,
    dock_hover: 0x14_00_00_00,
    dock_hover_edge: 0x22_00_00_00,
    dock_desktop: 0x38_00_00_00,
    dock_desktop_hover: 0xBE_00_00_00,
    icon_backing: 0xE8_FF_FF_FF,
    tip_bg: 0xF4_FF_FF_FF,
    tip_text: 0xFF_1C_1E_22,
    pin: 0xFF_C7_7A_00,
    sun: 0xFF_E8_A1_00,
    moon: 0xFF_5B_7F_B5,
    rain: 0xFF_2F_7D_C4,
    cloud: 0xFF_7A_84_8E,
    dock_radius: 20.0,
    panel_radius: 16.0,
    tip_radius: 10.0,
};

// =====================================================================
//  Material 3 —— 动态取色
//  大圆角、卡片分层、强对比强调色、涟漪反馈。
// =====================================================================

/// Material 3 · 深色：M3 dark surface + 高亮紫蓝主色。
pub const M3_DARK: Theme = Theme {
    key: "m3_dark",
    name: "Material 3 · 深色",
    dark: true,
    glow: false,
    pill_bg: 0xEA_1E_1B_24,
    text: 0xFF_E6_E1_E9,
    text_dim: 0xA8_E6_E1_E9,
    text_body: 0xEC_E6_E1_E9,
    icon_dim: 0xBC_E6_E1_E9,
    divider: 0x18_E6_E1_E9,
    hover: 0x14_E6_E1_E9,
    panel_bg: 0xFF_19_16_1F,
    panel_border: 0x1E_E6_E1_E9,
    accent: 0xFF_C8_BE_F0,
    dock_edge: 0x1C_E6_E1_E9,
    dock_glow: 0x10_FF_FF_FF,
    dock_hover: 0x1A_E6_E1_E9,
    dock_hover_edge: 0x24_E6_E1_E9,
    dock_desktop: 0x34_E6_E1_E9,
    dock_desktop_hover: 0xD0_E6_E1_E9,
    icon_backing: 0,
    tip_bg: 0xF4_21_1E_29,
    tip_text: 0xFF_E6_E1_E9,
    pin: 0xFF_EF_B8_C8,
    sun: 0xFF_FF_D9_80,
    moon: 0xFF_C8_D0_F0,
    rain: 0xFF_9C_C0_F0,
    cloud: 0xFF_C4_C4_D0,
    dock_radius: 26.0,
    panel_radius: 20.0,
    tip_radius: 12.0,
};

/// Material 3 · 浅色：M3 light surface + 紫罗兰主色。
pub const M3_LIGHT: Theme = Theme {
    key: "m3_light",
    name: "Material 3 · 浅色",
    dark: false,
    glow: false,
    pill_bg: 0xF0_FC_F8_FF,
    text: 0xFF_1D_1B_20,
    text_dim: 0x8C_1D_1B_20,
    text_body: 0xE6_1D_1B_20,
    icon_dim: 0x9E_1D_1B_20,
    divider: 0x14_1D_1B_20,
    hover: 0x0E_1D_1B_20,
    panel_bg: 0xFF_FE_FB_FF,
    panel_border: 0x18_1D_1B_20,
    accent: 0xFF_67_4F_A8,
    dock_edge: 0x16_1D_1B_20,
    dock_glow: 0x08_FF_FF_FF,
    dock_hover: 0x12_1D_1B_20,
    dock_hover_edge: 0x1E_1D_1B_20,
    dock_desktop: 0x2E_1D_1B_20,
    dock_desktop_hover: 0xB6_1D_1B_20,
    icon_backing: 0xF0_FF_FF_FF,
    tip_bg: 0xF8_FE_FB_FF,
    tip_text: 0xFF_1D_1B_20,
    pin: 0xFF_8F_4A_66,
    sun: 0xFF_E9_A6_2C,
    moon: 0xFF_6E_7A_A8,
    rain: 0xFF_3F_7C_C4,
    cloud: 0xFF_7C_84_96,
    dock_radius: 26.0,
    panel_radius: 20.0,
    tip_radius: 12.0,
};

/// 全部内置主题（顺序即设置页色块选择器的顺序，深浅成对排列）
pub const ALL: &[Theme] = &[
    CALM_LIGHT,
    CALM_DARK,
    GLASS2_LIGHT,
    GLASS2_DARK,
    M3_LIGHT,
    M3_DARK,
];

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
        assert!(n >= 6);
    }

    #[test]
    fn each_style_has_both_modes() {
        // 每套风格都有深浅两个变体
        for style in ["calm", "glass2", "m3"] {
            let light = ALL.iter().find(|t| t.key == format!("{style}_light"));
            let dark = ALL.iter().find(|t| t.key == format!("{style}_dark"));
            assert!(light.is_some(), "{style} 缺浅色变体");
            assert!(dark.is_some(), "{style} 缺深色变体");
            assert_eq!(light.unwrap().dark, false);
            assert_eq!(dark.unwrap().dark, true);
        }
    }

    #[test]
    fn set_index_clamps() {
        set_index(9999);
        assert_eq!(index(), 0);
        set_index(1);
        assert_eq!(index(), 1);
        set_index(0);
        assert_eq!(cur().key, "calm_light");
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
