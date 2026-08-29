use ratatui::style::Color;

#[derive(Clone)]
pub(crate) struct Theme {
    pub(crate) accent: Color,
    pub(crate) panel_bg: Color,
    pub(crate) surface0: Color,
    pub(crate) surface1: Color,
    pub(crate) overlay0: Color,
    pub(crate) text: Color,
    pub(crate) subtext0: Color,
    pub(crate) green: Color,
    pub(crate) yellow: Color,
    pub(crate) red: Color,
    pub(crate) blue: Color,
    pub(crate) teal: Color,
    pub(crate) mauve: Color,
    pub(crate) peach: Color,
}

impl Theme {
    pub(crate) fn terminal() -> Self {
        Self {
            accent: Color::Blue,
            panel_bg: Color::Reset,
            surface0: Color::DarkGray,
            surface1: Color::DarkGray,
            overlay0: Color::Gray,
            text: Color::Reset,
            subtext0: Color::Gray,
            mauve: Color::Gray,
            green: Color::Green,
            yellow: Color::Yellow,
            red: Color::LightRed,
            blue: Color::Blue,
            teal: Color::Cyan,
            peach: Color::Yellow,
        }
    }

    pub(crate) fn selection_background(&self, strong: bool) -> Color {
        if strong {
            self.surface1
        } else {
            self.surface0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_palette_uses_terminal_colors() {
        let theme = Theme::terminal();

        assert_eq!(theme.panel_bg, Color::Reset);
        assert_eq!(theme.accent, Color::Blue);
        assert_eq!(theme.green, Color::Green);
    }
}
