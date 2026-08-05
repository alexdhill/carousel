use crate::commands::{Command, CommandError, CommandOutput};
use crate::deck::SlideId;

#[derive(Debug, Clone)]
pub struct SetSlideTitle {
    pub slide_id: SlideId,
    pub new_title: String,
}

impl Command for SetSlideTitle {

    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.slide_id.is_empty(),
            "SetSlideTitle: slide_id is empty"
        );
        let entry = deck
            .manifest
            .slides
            .iter_mut()
            .find(|e| e.id == self.slide_id)
            .ok_or_else(|| CommandError::SlideNotFound(self.slide_id.clone()))?;
        let prior: String = entry.title.clone();
        entry.title = self.new_title.clone();
        deck.manifest_dirty = true;

        let inverse: SetSlideTitle = SetSlideTitle {
            slide_id: self.slide_id.clone(),
            new_title: prior,
        };

        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(inverse),
            dirty_targets: Vec::new(),
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Rename Slide"
    }

    fn affects_slide_list(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct SetDeckTitle {
    pub new_title: String,
}

impl Command for SetDeckTitle {

    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        let prior: String = deck.manifest.metadata.title.clone();
        deck.manifest.metadata.title = self.new_title.clone();
        deck.manifest_dirty = true;
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(SetDeckTitle { new_title: prior }),
            dirty_targets: Vec::new(),
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Rename Deck"
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;

    #[test]
    fn deck_title_sets_and_inverts() {
        let mut deck = Deck::sample();
        let original: String = deck.manifest.metadata.title.clone();
        let out = SetDeckTitle {
            new_title: "Q3 Review".into(),
        }
        .apply(&mut deck)
        .unwrap();
        assert_eq!(deck.manifest.metadata.title, "Q3 Review");
        assert!(deck.manifest_dirty);
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(deck.manifest.metadata.title, original);
    }

    #[test]
    fn sets_the_manifest_title() {
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        SetSlideTitle {
            slide_id: sid.clone(),
            new_title: "Agenda".into(),
        }
        .apply(&mut deck)
        .unwrap();
        let entry = deck.manifest.slides.iter().find(|e| e.id == sid).unwrap();
        assert_eq!(entry.title, "Agenda");
        assert!(deck.manifest_dirty);
    }

    #[test]
    fn rebroadcasts_slide_list() {
        let cmd = SetSlideTitle {
            slide_id: "s".into(),
            new_title: "x".into(),
        };
        assert!(cmd.affects_slide_list());
    }

    #[test]
    fn inverse_restores_prior_title() {
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let original: String = deck
            .manifest
            .slides
            .iter()
            .find(|e| e.id == sid)
            .unwrap()
            .title
            .clone();
        let out = SetSlideTitle {
            slide_id: sid.clone(),
            new_title: "Changed".into(),
        }
        .apply(&mut deck)
        .unwrap();
        out.inverse.apply(&mut deck).unwrap();
        let entry = deck.manifest.slides.iter().find(|e| e.id == sid).unwrap();
        assert_eq!(entry.title, original);
    }

    #[test]
    fn errors_on_missing_slide() {
        let mut deck = Deck::sample();
        let err = SetSlideTitle {
            slide_id: "ghost".into(),
            new_title: "x".into(),
        }
        .apply(&mut deck)
        .unwrap_err();
        assert!(matches!(err, CommandError::SlideNotFound(_)));
    }

    #[test]
    fn empty_title_is_valid() {
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        SetSlideTitle {
            slide_id: sid.clone(),
            new_title: String::new(),
        }
        .apply(&mut deck)
        .unwrap();
        let entry = deck.manifest.slides.iter().find(|e| e.id == sid).unwrap();
        assert!(entry.title.is_empty());
    }
}
