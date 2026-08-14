use ratatui::style::{Color, Style};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThemeKind {
    Auto,
    #[default]
    IndusNight,
    IndusDay,
    IndusMidnight,
    IndusWarm,
}

impl ThemeKind {
    pub const ALL: [Self; 5] = [
        Self::Auto,
        Self::IndusNight,
        Self::IndusDay,
        Self::IndusMidnight,
        Self::IndusWarm,
    ];

    pub fn from_name(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" | "system" => Some(Self::Auto),
            "indus-night" | "indusnight" | "night" | "dark" => Some(Self::IndusNight),
            "indusday" | "indus-day" | "day" | "light" => Some(Self::IndusDay),
            "indus-midnight" | "indusmidnight" | "midnight" => Some(Self::IndusMidnight),
            "indus-warm" | "induswarm" | "warm" => Some(Self::IndusWarm),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::IndusNight => "indus-night",
            Self::IndusDay => "indusday",
            Self::IndusMidnight => "indus-midnight",
            Self::IndusWarm => "indus-warm",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Auto => "Follow the terminal appearance",
            Self::IndusNight => "Neutral dark interface",
            Self::IndusDay => "Neutral light interface",
            Self::IndusMidnight => "Deep midnight interface",
            Self::IndusWarm => "Amber and terracotta interface",
        }
    }

    pub fn resolved(self) -> Self {
        if self != Self::Auto {
            return self;
        }

        let light_terminal = std::env::var("COLORFGBG")
            .ok()
            .and_then(|value| value.rsplit(';').next()?.parse::<u8>().ok())
            .is_some_and(|background| background >= 7);
        if light_terminal {
            Self::IndusDay
        } else {
            Self::IndusNight
        }
    }
}

/// Complete semantic palette for conversation, execution, and diff rendering.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub bg_base: Color,
    pub bg_light: Color,
    pub bg_dark: Color,
    pub bg_hover: Color,
    pub bg_visual: Color,
    pub text_primary: Color,
    pub text_secondary: Color,
    pub gray_dim: Color,
    pub gray: Color,
    pub gray_bright: Color,
    pub accent_user: Color,
    pub accent_assistant: Color,
    pub accent_thinking: Color,
    pub accent_system: Color,
    pub accent_success: Color,
    pub accent_error: Color,
    pub diff_insert_fg: Color,
    pub diff_insert_bg: Color,
    pub diff_delete_fg: Color,
    pub diff_delete_bg: Color,
    pub diff_equal_fg: Color,
    pub diff_gutter_fg: Color,
    pub accent_skill: Color,
    pub command: Color,
    pub warning: Color,
    pub fuzzy_accent: Color,
    pub prompt_border: Color,
    pub prompt_border_active: Color,
    pub scrollbar_bg: Color,
    pub scrollbar_fg: Color,
}

impl Theme {
    pub const fn from_kind(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Auto | ThemeKind::IndusNight => Self::indus_night(),
            ThemeKind::IndusDay => Self::indus_day(),
            ThemeKind::IndusMidnight => Self::indus_midnight(),
            ThemeKind::IndusWarm => Self::indus_warm(),
        }
    }

    pub fn for_preference(kind: ThemeKind) -> Self {
        Self::from_kind(kind.resolved())
    }

    pub fn base(self) -> Style {
        Style::default().fg(self.text_primary).bg(self.bg_base)
    }

    const fn indus_night() -> Self {
        Self {
            bg_base: rgb(20, 20, 20),
            bg_light: rgb(36, 36, 36),
            bg_dark: rgb(28, 28, 28),
            bg_hover: rgb(44, 44, 44),
            bg_visual: rgb(54, 54, 54),
            text_primary: rgb(225, 225, 225),
            text_secondary: rgb(200, 200, 200),
            gray_dim: rgb(88, 88, 88),
            gray: rgb(108, 108, 108),
            gray_bright: rgb(120, 120, 120),
            accent_user: rgb(200, 200, 200),
            accent_assistant: rgb(187, 154, 247),
            accent_thinking: rgb(187, 154, 247),
            accent_system: rgb(122, 162, 247),
            accent_success: rgb(158, 206, 106),
            accent_error: rgb(247, 118, 142),
            diff_insert_fg: rgb(158, 206, 106),
            diff_insert_bg: rgb(28, 54, 34),
            diff_delete_fg: rgb(247, 118, 142),
            diff_delete_bg: rgb(62, 30, 38),
            diff_equal_fg: rgb(170, 170, 170),
            diff_gutter_fg: rgb(88, 88, 88),
            accent_skill: rgb(122, 162, 247),
            command: rgb(224, 175, 104),
            warning: rgb(224, 175, 104),
            fuzzy_accent: rgb(122, 162, 247),
            prompt_border: rgb(50, 50, 55),
            prompt_border_active: rgb(80, 80, 88),
            scrollbar_bg: rgb(17, 17, 17),
            scrollbar_fg: rgb(36, 36, 36),
        }
    }

    const fn indus_day() -> Self {
        Self {
            bg_base: rgb(238, 238, 238),
            bg_light: rgb(222, 222, 222),
            bg_dark: rgb(228, 228, 228),
            bg_hover: rgb(208, 208, 208),
            bg_visual: rgb(198, 198, 198),
            text_primary: rgb(38, 38, 38),
            text_secondary: rgb(68, 68, 68),
            gray_dim: rgb(165, 165, 165),
            gray: rgb(118, 118, 118),
            gray_bright: rgb(98, 98, 98),
            accent_user: rgb(68, 68, 68),
            accent_assistant: rgb(125, 75, 198),
            accent_thinking: rgb(125, 75, 198),
            accent_system: rgb(47, 100, 210),
            accent_success: rgb(55, 142, 35),
            accent_error: rgb(205, 48, 72),
            diff_insert_fg: rgb(42, 130, 32),
            diff_insert_bg: rgb(210, 232, 207),
            diff_delete_fg: rgb(190, 42, 62),
            diff_delete_bg: rgb(242, 211, 216),
            diff_equal_fg: rgb(68, 68, 68),
            diff_gutter_fg: rgb(135, 135, 135),
            accent_skill: rgb(47, 100, 210),
            command: rgb(162, 118, 18),
            warning: rgb(162, 118, 18),
            fuzzy_accent: rgb(47, 100, 210),
            prompt_border: rgb(200, 200, 205),
            prompt_border_active: rgb(165, 165, 175),
            scrollbar_bg: rgb(234, 234, 234),
            scrollbar_fg: rgb(198, 198, 198),
        }
    }

    const fn indus_midnight() -> Self {
        Self {
            bg_base: rgb(3, 3, 4),
            bg_light: rgb(15, 18, 22),
            bg_dark: rgb(4, 5, 7),
            bg_hover: rgb(36, 32, 52),
            bg_visual: rgb(36, 32, 52),
            text_primary: rgb(228, 228, 228),
            text_secondary: rgb(190, 190, 190),
            gray_dim: rgb(94, 100, 108),
            gray: rgb(129, 134, 143),
            gray_bright: rgb(190, 190, 190),
            accent_user: rgb(196, 167, 231),
            accent_assistant: rgb(155, 126, 206),
            accent_thinking: rgb(129, 134, 143),
            accent_system: rgb(125, 207, 223),
            accent_success: rgb(80, 180, 140),
            accent_error: rgb(220, 90, 100),
            diff_insert_fg: rgb(80, 180, 140),
            diff_insert_bg: rgb(13, 43, 35),
            diff_delete_fg: rgb(220, 90, 100),
            diff_delete_bg: rgb(51, 21, 28),
            diff_equal_fg: rgb(170, 174, 180),
            diff_gutter_fg: rgb(94, 100, 108),
            accent_skill: rgb(155, 126, 206),
            command: rgb(235, 217, 110),
            warning: rgb(235, 217, 110),
            fuzzy_accent: rgb(196, 167, 231),
            prompt_border: rgb(36, 32, 52),
            prompt_border_active: rgb(52, 48, 72),
            scrollbar_bg: rgb(18, 16, 28),
            scrollbar_fg: rgb(52, 48, 72),
        }
    }

    const fn indus_warm() -> Self {
        Self {
            bg_base: rgb(214, 165, 78),
            bg_light: rgb(196, 112, 72),
            bg_dark: rgb(202, 146, 72),
            bg_hover: rgb(187, 101, 65),
            bg_visual: rgb(184, 94, 61),
            text_primary: Color::Black,
            text_secondary: Color::Black,
            gray_dim: rgb(86, 55, 36),
            gray: rgb(72, 46, 31),
            gray_bright: rgb(46, 31, 22),
            accent_user: Color::Black,
            accent_assistant: Color::Black,
            accent_thinking: Color::Black,
            accent_system: Color::Black,
            accent_success: Color::Black,
            accent_error: rgb(92, 25, 20),
            diff_insert_fg: rgb(25, 78, 35),
            diff_insert_bg: rgb(185, 139, 67),
            diff_delete_fg: rgb(112, 27, 22),
            diff_delete_bg: rgb(196, 112, 72),
            diff_equal_fg: Color::Black,
            diff_gutter_fg: rgb(86, 55, 36),
            accent_skill: Color::Black,
            command: Color::Black,
            warning: Color::Black,
            fuzzy_accent: Color::Black,
            prompt_border: rgb(121, 69, 43),
            prompt_border_active: rgb(74, 42, 28),
            scrollbar_bg: rgb(184, 128, 62),
            scrollbar_fg: rgb(121, 69, 43),
        }
    }
}

const fn rgb(red: u8, green: u8, blue: u8) -> Color {
    Color::Rgb(red, green, blue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_aliases_resolve_to_indus_names() {
        assert_eq!(ThemeKind::from_name("dark"), Some(ThemeKind::IndusNight));
        assert_eq!(ThemeKind::from_name("light"), Some(ThemeKind::IndusDay));
        assert_eq!(ThemeKind::from_name("warm"), Some(ThemeKind::IndusWarm));
    }

    #[test]
    fn warm_theme_uses_black_primary_text() {
        let theme = Theme::from_kind(ThemeKind::IndusWarm);
        assert_eq!(theme.text_primary, Color::Black);
        assert_eq!(theme.accent_user, Color::Black);
    }
}
