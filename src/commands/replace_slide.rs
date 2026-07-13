// ReplaceSlideContent command.
//
// Swaps a slide root group's children with new content from an agent write.
// Used by the agent panel workflow to apply whole-slide edits in one undoable step.
//
// Effects: replaces the slide's root element's children, emits a SetInnerHtml
// patch for the webview to apply, and marks the slide dirty and requiring remount.

use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::element::ElementNode;
use crate::deck::{Canvas, CanvasTarget, SlideId};
use crate::html::serialize::serialize_element;
use crate::ipc::Patch;

#[derive(Debug, Clone)]
pub struct ReplaceSlideContent {
    pub slide_id: SlideId,
    pub new_children: Vec<ElementNode>,
}

impl Command for ReplaceSlideContent {
    // apply
    // Inputs: &self, &mut Deck.
    // Output: CommandOutput with a SetInnerHtml patch, an inverse
    // ReplaceSlideContent carrying the pre-swap children, the slide marked
    // dirty, and requires_remount = true.
    // Errors:
    //   SlideNotFound    — slide_id absent.
    // Dataflow: locate slide -> snapshot current children -> replace them
    // with new_children -> serialize to HTML -> emit patch -> build inverse.
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.slide_id.is_empty(),
            "ReplaceSlideContent: slide_id is empty"
        );
        assert!(
            !self.new_children.is_empty(),
            "ReplaceSlideContent: new_children is empty"
        );

        let canvas: &mut dyn Canvas = resolve_canvas_mut(deck, &CanvasTarget::Slide(self.slide_id.clone()))?;

        let old_children: Vec<ElementNode> = canvas.root().children.clone();
        let root_id: String = canvas.root().id.clone();

        canvas.root_mut().children = self.new_children.clone();
        canvas.mark_dirty();
        canvas.invalidate_index();

        let mut inner_html: String = String::new();
        for child in &self.new_children {
            inner_html.push_str(&serialize_element(child));
        }

        let inverse: ReplaceSlideContent = ReplaceSlideContent {
            slide_id: self.slide_id.clone(),
            new_children: old_children,
        };

        Ok(CommandOutput {
            patches: vec![Patch::SetInnerHtml {
                element_id: root_id,
                html: inner_html,
            }],
            inverse: Box::new(inverse),
            dirty_targets: vec![CanvasTarget::Slide(self.slide_id.clone())],
            manifest_dirty: false,
            warnings: Vec::new(),
        })
    }

    fn label(&self) -> &'static str {
        "Agent Edit"
    }

    fn affects_object_tree(&self) -> bool {
        true
    }

    fn requires_remount(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::Deck;
    use crate::deck::builders::text_element;

    #[test]
    fn replaces_slide_children_and_inverse_round_trips() {
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();

        let original_children: Vec<ElementNode> = deck.slides[&sid].root.children.clone();

        let new_children: Vec<ElementNode> = vec![
            text_element("new_1", "replaced"),
            text_element("new_2", "content"),
        ];

        let cmd = ReplaceSlideContent {
            slide_id: sid.clone(),
            new_children: new_children.clone(),
        };

        let output = cmd.apply(&mut deck).unwrap();
        assert_eq!(deck.slides[&sid].root.children.len(), 2);
        assert_eq!(deck.slides[&sid].root.children[0].id, "new_1");
        assert_eq!(deck.slides[&sid].root.children[1].id, "new_2");

        output.inverse.apply(&mut deck).unwrap();
        assert_eq!(
            deck.slides[&sid].root.children.len(),
            original_children.len()
        );
        assert_eq!(deck.slides[&sid].root.children, original_children);
    }
}
