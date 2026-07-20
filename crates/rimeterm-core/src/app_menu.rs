//! Application menu (top-left `≡ rimeterm`).
//!
//! §19.13 of the design doc. v0.1 contains only Settings + Acknowledgement +
//! Quit. Menu items only dispatch commands from [`crate::command::CommandRegistry`];
//! side effects live in command bodies, not here.

use crate::command::CommandId;

#[derive(Clone, Debug)]
pub struct AppMenuItem {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: Option<&'static str>,
    pub key_hint: Option<&'static str>,
    pub command: CommandId,
    pub separator_before: bool,
}

#[derive(Clone, Debug, Default)]
pub struct AppMenu {
    pub items: Vec<AppMenuItem>,
}

impl AppMenu {
    /// v0.1 default set. Kernel + config layer may append more later.
    pub fn v0_1_default() -> Self {
        Self {
            items: vec![
                AppMenuItem {
                    id: "app.settings",
                    title: "Settings",
                    icon: Some("⚙"),
                    key_hint: Some(","),
                    command: "app.settings",
                    separator_before: false,
                },
                AppMenuItem {
                    id: "app.acknowledgement",
                    title: "Acknowledgement",
                    icon: Some("ⓘ"),
                    key_hint: Some("?"),
                    command: "app.acknowledgement",
                    separator_before: false,
                },
                AppMenuItem {
                    id: "app.quit",
                    title: "Quit",
                    icon: Some("⏻"),
                    key_hint: Some("Ctrl+Q"),
                    command: "app.quit",
                    separator_before: true,
                },
            ],
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_menu_has_three_items_settings_ack_quit() {
        let menu = AppMenu::v0_1_default();
        assert_eq!(menu.items.len(), 3);
        assert_eq!(menu.items[0].id, "app.settings");
        assert_eq!(menu.items[1].id, "app.acknowledgement");
        assert_eq!(menu.items[2].id, "app.quit");
        assert!(
            menu.items[2].separator_before,
            "Quit sits under a separator"
        );
    }
}
