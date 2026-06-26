// Group layout commands: SetGroupLayout (direction/distribution/alignment) and
// SetGroupScale (uniform scale). Both patch the group's GroupStyle, relayout
// the group (and its ancestors), and emit the resulting geometry patches.

use crate::commands::{Command, CommandError, CommandOutput, relayout_patches, resolve_canvas_mut};
use crate::deck::element::ElementStyle;
use crate::deck::style::{GroupAlignment, GroupDirection, GroupDistribution};
use crate::deck::{CanvasTarget, ElementId};
use crate::ipc::Patch;

#[derive(Debug, Clone)]
pub struct SetGroupLayout {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub direction: Option<GroupDirection>,
    pub distribution: Option<GroupDistribution>,
    pub alignment: Option<GroupAlignment>,
}

impl Command for SetGroupLayout {
    // apply — patch the present fields on the group's GroupStyle, relayout, and
    // emit geometry patches. Inverse restores the prior props.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.element_id.is_empty(),
            "SetGroupLayout: empty element_id"
        );
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        let prior: crate::deck::style::GroupStyle = {
            let el = canvas
                .find_element_mut(&self.element_id)
                .ok_or_else(|| CommandError::ElementNotFound(self.element_id.clone()))?;
            let gs = match &mut el.style {
                ElementStyle::Group(gs) => gs,
                _ => return Err(CommandError::InvalidOperation("not a group".into())),
            };
            let before = gs.clone();
            if let Some(d) = self.direction {
                gs.direction = d;
            }
            if let Some(d) = self.distribution {
                gs.distribution = d;
            }
            if let Some(a) = self.alignment {
                gs.alignment = a;
            }
            before
        };
        canvas.mark_dirty();
        let patches: Vec<Patch> = relayout_patches(canvas.root_mut(), &self.element_id);
        let inverse = SetGroupLayout {
            target: self.target.clone(),
            element_id: self.element_id.clone(),
            direction: Some(prior.direction),
            distribution: Some(prior.distribution),
            alignment: Some(prior.alignment),
        };
        Ok(CommandOutput {
            patches,
            inverse: Box::new(inverse),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }
    fn label(&self) -> &'static str {
        "Set Group Layout"
    }
    fn requires_remount(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct SetGroupScale {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub scale: f64,
}

impl Command for SetGroupScale {
    // apply — set the group's uniform scale. Inverse restores the prior scale.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.element_id.is_empty(),
            "SetGroupScale: empty element_id"
        );
        assert!(self.scale > 0.0, "SetGroupScale: scale must be positive");
        let canvas = resolve_canvas_mut(deck, &self.target)?;
        let prior: f64 = {
            let el = canvas
                .find_element_mut(&self.element_id)
                .ok_or_else(|| CommandError::ElementNotFound(self.element_id.clone()))?;
            let gs = match &mut el.style {
                ElementStyle::Group(gs) => gs,
                _ => return Err(CommandError::InvalidOperation("not a group".into())),
            };
            let before = gs.scale;
            gs.scale = self.scale;
            before
        };
        canvas.mark_dirty();
        let inverse = SetGroupScale {
            target: self.target.clone(),
            element_id: self.element_id.clone(),
            scale: prior,
        };
        Ok(CommandOutput {
            patches: Vec::new(),
            inverse: Box::new(inverse),
            dirty_targets: vec![self.target.clone()],
            manifest_dirty: true,
            warnings: Vec::new(),
        })
    }
    fn label(&self) -> &'static str {
        "Scale Group"
    }
    fn requires_remount(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::deck::builders::{group_element, text_element};
    use crate::deck::element::ElementStyle;
    use crate::deck::style::{GroupDistribution, GroupStyle};
    use crate::deck::{CanvasTarget, Deck};

    fn deck_with_group() -> (Deck, crate::deck::SlideId, crate::deck::ElementId) {
        let mut deck = Deck::sample();
        let sid = deck.slide_order[0].clone();
        let mut a = text_element("ga", "t");
        a.geometry.x = 0.0;
        a.geometry.width = 20.0;
        a.geometry.height = 10.0;
        let mut b = text_element("gb", "t");
        b.geometry.x = 80.0;
        b.geometry.width = 20.0;
        b.geometry.height = 10.0;
        let mut g = group_element("grp", vec![a, b]);
        g.style = ElementStyle::Group(GroupStyle::default());
        deck.slides.get_mut(&sid).unwrap().root.children.push(g);
        (deck, sid.clone(), "grp".into())
    }

    #[test]
    fn set_group_layout_sets_props_and_relayouts() {
        let (mut deck, sid, gid) = deck_with_group();
        let cmd = SetGroupLayout {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: gid.clone(),
            direction: None,
            distribution: Some(GroupDistribution::SpaceBetween),
            alignment: None,
        };
        let out = cmd.apply(&mut deck).unwrap();
        let g = deck.slides[&sid].find_element(&gid).unwrap();
        match &g.style {
            ElementStyle::Group(gs) => assert_eq!(gs.distribution, GroupDistribution::SpaceBetween),
            _ => panic!("not a group"),
        }
        assert!(
            out.patches
                .iter()
                .any(|p| matches!(p, crate::ipc::Patch::SetStyle { .. }))
        );
        out.inverse.apply(&mut deck).unwrap();
        let g2 = deck.slides[&sid].find_element(&gid).unwrap();
        match &g2.style {
            ElementStyle::Group(gs) => assert_eq!(gs.distribution, GroupDistribution::None),
            _ => panic!("not a group"),
        }
    }

    #[test]
    fn set_group_scale_sets_scale_and_inverts() {
        let (mut deck, sid, gid) = deck_with_group();
        let out = SetGroupScale {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: gid.clone(),
            scale: 2.0,
        }
        .apply(&mut deck)
        .unwrap();
        let g = deck.slides[&sid].find_element(&gid).unwrap();
        match &g.style {
            ElementStyle::Group(gs) => assert_eq!(gs.scale, 2.0),
            _ => panic!(),
        }
        out.inverse.apply(&mut deck).unwrap();
        let g2 = deck.slides[&sid].find_element(&gid).unwrap();
        match &g2.style {
            ElementStyle::Group(gs) => assert_eq!(gs.scale, 1.0),
            _ => panic!(),
        }
    }
}
