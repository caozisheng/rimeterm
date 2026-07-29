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
                #[cfg(windows)]
                AppMenuItem {
                    id: "app.upgrade",
                    title: "Upgrade",
                    icon: Some("⇧"),
                    key_hint: None,
                    command: "app.upgrade",
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
    fn default_menu_has_platform_items_in_order() {
        let menu = AppMenu::v0_1_default();

        #[cfg(windows)]
        assert_eq!(
            menu.items.iter().map(|item| item.id).collect::<Vec<_>>(),
            [
                "app.settings",
                "app.upgrade",
                "app.acknowledgement",
                "app.quit",
            ]
        );

        #[cfg(not(windows))]
        assert_eq!(
            menu.items.iter().map(|item| item.id).collect::<Vec<_>>(),
            ["app.settings", "app.acknowledgement", "app.quit"]
        );

        assert!(
            menu.items.last().is_some_and(|item| item.separator_before),
            "Quit sits under a separator"
        );
    }
}
