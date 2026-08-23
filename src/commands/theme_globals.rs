use crate::commands::{Command, CommandError, CommandOutput};

#[derive(Debug, Clone)]
pub struct SetGlobalsCss {
    pub new_css: String,
}

impl Command for SetGlobalsCss {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        let prior: String = deck.theme.globals_css.clone();
        deck.theme.globals_css = self.new_css.clone();
        deck.manifest_dirty = true;

        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(SetGlobalsCss { new_css: prior }),
            dirty_targets: Vec::new(),
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Edit Globals CSS"
    }

    fn affects_globals(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;

    #[test]
    fn set_globals_replaces_blob_and_flags_globals() {
        let mut deck = Deck::default();
        assert!(deck.theme.globals_css.is_empty());
        let cmd = SetGlobalsCss {
            new_css: "@keyframes spin { to { rotate: 360deg; } }".into(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert!(deck.theme.globals_css.contains("@keyframes spin"));
        assert!(out.patches.is_empty());
        assert!(out.manifest_dirty);
        assert!(cmd.affects_globals());
    }

    #[test]
    fn set_globals_inverse_restores_prior_blob() {
        let mut deck = Deck::default();
        deck.theme.globals_css = ":root { --x: 1; }".into();
        let cmd = SetGlobalsCss {
            new_css: ":root { --x: 2; }".into(),
        };
        let out = cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.theme.globals_css, ":root { --x: 2; }");
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.theme.globals_css, ":root { --x: 1; }");
    }

    #[test]
    fn set_globals_accepts_empty_string() {
        let mut deck = Deck::default();
        deck.theme.globals_css = "body{}".into();
        let cmd = SetGlobalsCss {
            new_css: String::new(),
        };
        cmd.apply(&mut deck).unwrap();
        assert!(deck.theme.globals_css.is_empty());
    }
}
