use crate::commands::scale_elements::{ElementTransform, SetElementsTransform};
use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::canvas::find_parent;
use crate::deck::element::{ElementNode, ElementStyle};
use crate::deck::style::{GroupAlignment, GroupDistribution};
use crate::deck::{Canvas, CanvasTarget, ElementId};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignAxis {
    Left,
    HCenter,
    Right,
    Top,
    VCenter,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistributeAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignOp {
    Align(AlignAxis),
    Distribute(DistributeAxis),
}

/// Aligns or evenly distributes a free multi-selection on one canvas.
///
/// Input: a canvas target, two or more element ids (three or more to
/// distribute), and the operation. Output: the `CommandOutput` of the
/// `SetElementsTransform` it delegates to, so the inverse, the dirty marking
/// and the minimum-size clamping are inherited rather than duplicated.
///
/// Errors: `InvalidOperation` when the selection is too small for the
/// operation, contains a duplicate id, spans more than one parent, or lives
/// inside a group whose flex layout owns child positions; `ElementNotFound`
/// when an id is missing — validated for every id before anything moves, so a
/// bad id never leaves a partial mutation behind.
#[derive(Debug, Clone)]
pub struct AlignElements {
    pub target: CanvasTarget,
    pub element_ids: Vec<ElementId>,
    pub op: AlignOp,
}

impl Command for AlignElements {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(
            !self.target.id().is_empty(),
            "AlignElements: target id empty"
        );
        let minimum: usize = match self.op {
            AlignOp::Align(_) => 2,
            AlignOp::Distribute(_) => 3,
        };
        if self.element_ids.len() < minimum {
            return Err(CommandError::InvalidOperation(format!(
                "operation needs at least {minimum} selected elements"
            )));
        }
        let unique: BTreeSet<&ElementId> = self.element_ids.iter().collect();
        if unique.len() != self.element_ids.len() {
            return Err(CommandError::InvalidOperation(
                "selection contains a duplicate element id".into(),
            ));
        }
        let canvas: &mut dyn Canvas = resolve_canvas_mut(deck, &self.target)?;
        let boxes: Vec<AlignBox> = collect_boxes(canvas, &self.element_ids)?;
        let items: Vec<ElementTransform> = plan_transforms(&boxes, self.op);
        SetElementsTransform {
            target: self.target.clone(),
            items,
        }
        .apply(deck)
    }

    fn label(&self) -> &'static str {
        match self.op {
            AlignOp::Align(_) => "Align Elements",
            AlignOp::Distribute(_) => "Distribute Elements",
        }
    }

    fn requires_remount(&self) -> bool {
        true
    }
}

/// Geometry of one selected element, in its parent's coordinate space.
#[derive(Debug, Clone)]
struct AlignBox {
    id: ElementId,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// Reads the geometry of every selected element and rejects selections whose
/// coordinates are not comparable.
///
/// Input: the canvas and the selected ids. Output: one `AlignBox` per id, in
/// selection order. Errors: `ElementNotFound` for a missing id,
/// `InvalidOperation` when the ids do not share one parent or that parent is a
/// group whose flex layout would immediately overwrite the new positions.
/// Control flow: one pass over the ids, comparing each parent against the
/// first one seen.
fn collect_boxes(canvas: &dyn Canvas, ids: &[ElementId]) -> Result<Vec<AlignBox>, CommandError> {
    assert!(!ids.is_empty(), "collect_boxes: no ids");
    let mut parent_id: Option<ElementId> = None;
    let mut managed: bool = false;
    let mut out: Vec<AlignBox> = Vec::with_capacity(ids.len());
    for id in ids {
        let element: &ElementNode = canvas
            .find_element(id)
            .ok_or_else(|| CommandError::ElementNotFound(id.clone()))?;
        let parent: &ElementNode = find_parent(canvas.root(), id)
            .ok_or_else(|| CommandError::ElementNotFound(id.clone()))?;
        match &parent_id {
            None => {
                parent_id = Some(parent.id.clone());
                managed = is_managed_group(parent);
            }
            Some(previous) if *previous == parent.id => {}
            Some(_) => {
                return Err(CommandError::InvalidOperation(
                    "aligned elements must share one parent".into(),
                ));
            }
        }

        // ponytail: alignment uses the unrotated box; a rotated element aligns
        // by its geometry, not by its visual corners. Upgrade path: rotate the
        // four corners here and align on that hull.
        out.push(AlignBox {
            id: id.clone(),
            x: element.geometry.x,
            y: element.geometry.y,
            width: element.geometry.width,
            height: element.geometry.height,
        });
    }
    if managed {
        return Err(CommandError::InvalidOperation(
            "cannot align inside a group whose flex layout positions its children".into(),
        ));
    }
    Ok(out)
}

/// True when a group's own layout writes its children's positions, which would
/// undo any alignment applied to them.
fn is_managed_group(node: &ElementNode) -> bool {
    match &node.style {
        ElementStyle::Group(style) => {
            style.distribution != GroupDistribution::None || style.alignment != GroupAlignment::None
        }
        _ => false,
    }
}

/// Turns the selection geometry into the absolute transforms the mutation
/// needs.
///
/// Input: the selection boxes and the operation. Output: one
/// `ElementTransform` per box, in selection order, with sizes unchanged.
/// Control flow: alignment works off the selection's union bounding box;
/// distribution sorts by leading edge, pins the extremes, and equalizes the
/// gaps between boxes.
fn plan_transforms(boxes: &[AlignBox], op: AlignOp) -> Vec<ElementTransform> {
    assert!(boxes.len() >= 2, "plan_transforms: selection too small");
    let left: f64 = fold_min(boxes, |b| b.x);
    let right: f64 = fold_max(boxes, |b| b.x + b.width);
    let top: f64 = fold_min(boxes, |b| b.y);
    let bottom: f64 = fold_max(boxes, |b| b.y + b.height);
    let mut xs: Vec<f64> = boxes.iter().map(|b| b.x).collect();
    let mut ys: Vec<f64> = boxes.iter().map(|b| b.y).collect();
    match op {
        AlignOp::Align(AlignAxis::Left) => set_all(&mut xs, |_| left),
        AlignOp::Align(AlignAxis::Right) => set_all(&mut xs, |i| right - boxes[i].width),
        AlignOp::Align(AlignAxis::HCenter) => {
            set_all(&mut xs, |i| (left + right - boxes[i].width) / 2.0);
        }
        AlignOp::Align(AlignAxis::Top) => set_all(&mut ys, |_| top),
        AlignOp::Align(AlignAxis::Bottom) => set_all(&mut ys, |i| bottom - boxes[i].height),
        AlignOp::Align(AlignAxis::VCenter) => {
            set_all(&mut ys, |i| (top + bottom - boxes[i].height) / 2.0);
        }
        AlignOp::Distribute(DistributeAxis::Horizontal) => {
            spread(&mut xs, boxes, (left, right), |b| (b.x, b.width));
        }
        AlignOp::Distribute(DistributeAxis::Vertical) => {
            spread(&mut ys, boxes, (top, bottom), |b| (b.y, b.height));
        }
    }
    let mut items: Vec<ElementTransform> = Vec::with_capacity(boxes.len());
    for (i, b) in boxes.iter().enumerate() {
        items.push(ElementTransform {
            id: b.id.clone(),
            x: xs[i],
            y: ys[i],
            width: b.width,
            height: b.height,
            font_size_px: None,
            group_scale: None,
        });
    }
    items
}

/// Writes the distributed leading edges for one axis back into `axis_out`,
/// which is indexed by selection order.
///
/// Input: the current positions, the selection boxes, the span to fill, and an
/// accessor giving each box's leading edge and size on this axis. Output: none
/// — `axis_out` is updated in place. Control flow: sort indices by leading
/// edge, distribute, then scatter the results back to selection order.
fn spread(
    axis_out: &mut [f64],
    boxes: &[AlignBox],
    span: (f64, f64),
    axis_of: fn(&AlignBox) -> (f64, f64),
) {
    assert_eq!(axis_out.len(), boxes.len(), "spread: length mismatch");
    let mut order: Vec<usize> = (0..boxes.len()).collect();
    order.sort_by(|a, b| {
        let (pa, _) = axis_of(&boxes[*a]);
        let (pb, _) = axis_of(&boxes[*b]);
        pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
    });
    let sorted: Vec<(f64, f64)> = order.iter().map(|i| axis_of(&boxes[*i])).collect();
    let placed: Vec<f64> = distribute_positions(&sorted, span);
    for (slot, index) in order.iter().enumerate() {
        axis_out[*index] = placed[slot];
    }
}

/// Places items so the gaps between their boxes are equal across a span.
///
/// Input: `(leading_edge, size)` per item, already sorted by leading edge, and
/// the `(start, end)` of the span to fill. Output: the new leading edge of
/// every item, in the same order. Control flow: the first item keeps the
/// start of the span, each following item is placed one shared gap after the
/// previous item's trailing edge; with fewer than two items the input
/// positions are returned unchanged, so no gap is ever divided by zero.
fn distribute_positions(items: &[(f64, f64)], span: (f64, f64)) -> Vec<f64> {
    if items.len() < 2 {
        return items.iter().map(|it| it.0).collect();
    }
    let total_size: f64 = items.iter().map(|it| it.1).sum();
    let gap: f64 = (span.1 - span.0 - total_size) / ((items.len() - 1) as f64);
    let mut out: Vec<f64> = Vec::with_capacity(items.len());
    let mut cursor: f64 = span.0;
    for it in items {
        out.push(cursor);
        cursor += it.1 + gap;
    }
    out
}

fn set_all(values: &mut [f64], f: impl Fn(usize) -> f64) {
    for (i, v) in values.iter_mut().enumerate() {
        *v = f(i);
    }
}

fn fold_min(boxes: &[AlignBox], f: impl Fn(&AlignBox) -> f64) -> f64 {
    boxes.iter().map(f).fold(f64::INFINITY, f64::min)
}

fn fold_max(boxes: &[AlignBox], f: impl Fn(&AlignBox) -> f64) -> f64 {
    boxes.iter().map(f).fold(f64::NEG_INFINITY, f64::max)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::deck::builders::text_element;
    use crate::deck::style::{GroupDirection, GroupStyle};
    use crate::deck::{Deck, SlideId};

    fn deck_with(boxes: &[(&str, f64, f64, f64, f64)]) -> (Deck, SlideId) {
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let slide = deck.slides.get_mut(&sid).unwrap();
        slide.root.children.clear();
        for (id, x, y, w, h) in boxes {
            let mut node = text_element(*id, "x");
            node.geometry.x = *x;
            node.geometry.y = *y;
            node.geometry.width = *w;
            node.geometry.height = *h;
            slide.root.children.push(node);
        }
        (deck, sid)
    }

    fn geom(deck: &Deck, sid: &SlideId, id: &str) -> (f64, f64, f64, f64) {
        let g = &deck.slides[sid].find_element(id).unwrap().geometry;
        (g.x, g.y, g.width, g.height)
    }

    fn run(deck: &mut Deck, sid: &SlideId, ids: &[&str], op: AlignOp) -> CommandOutput {
        AlignElements {
            target: CanvasTarget::Slide(sid.clone()),
            element_ids: ids.iter().map(|i| ElementId::from(*i)).collect(),
            op,
        }
        .apply(deck)
        .unwrap()
    }

    fn three() -> (Deck, SlideId) {
        deck_with(&[
            ("a", 10.0, 5.0, 40.0, 20.0),
            ("b", 100.0, 50.0, 20.0, 40.0),
            ("c", 200.0, 90.0, 60.0, 10.0),
        ])
    }

    #[test]
    fn each_align_axis_moves_only_its_own_coordinate() {
        let cases: [(AlignAxis, (f64, f64, f64)); 6] = [
            (AlignAxis::Left, (10.0, 10.0, 10.0)),
            (AlignAxis::Right, (220.0, 240.0, 200.0)),
            (AlignAxis::HCenter, (115.0, 125.0, 105.0)),
            (AlignAxis::Top, (5.0, 5.0, 5.0)),
            (AlignAxis::Bottom, (80.0, 60.0, 90.0)),
            (AlignAxis::VCenter, (42.5, 32.5, 47.5)),
        ];
        for (axis, expected) in cases {
            let (mut deck, sid) = three();
            let vertical: bool = matches!(
                axis,
                AlignAxis::Top | AlignAxis::VCenter | AlignAxis::Bottom
            );
            run(&mut deck, &sid, &["a", "b", "c"], AlignOp::Align(axis));
            let got = [
                geom(&deck, &sid, "a"),
                geom(&deck, &sid, "b"),
                geom(&deck, &sid, "c"),
            ];
            let expect = [expected.0, expected.1, expected.2];
            let untouched = [10.0, 100.0, 200.0];
            let untouched_y = [5.0, 50.0, 90.0];
            for i in 0..3 {
                if vertical {
                    assert_eq!(got[i].1, expect[i], "{axis:?} y[{i}]");
                    assert_eq!(got[i].0, untouched[i], "{axis:?} moved x[{i}]");
                } else {
                    assert_eq!(got[i].0, expect[i], "{axis:?} x[{i}]");
                    assert_eq!(got[i].1, untouched_y[i], "{axis:?} moved y[{i}]");
                }
            }
        }
    }

    #[test]
    fn distribute_equalizes_gaps_and_pins_the_extremes() {
        let (mut deck, sid) = deck_with(&[
            ("a", 0.0, 0.0, 10.0, 10.0),
            ("b", 20.0, 0.0, 10.0, 10.0),
            ("c", 200.0, 0.0, 10.0, 10.0),
            ("d", 300.0, 0.0, 10.0, 10.0),
        ]);
        run(
            &mut deck,
            &sid,
            &["a", "b", "c", "d"],
            AlignOp::Distribute(DistributeAxis::Horizontal),
        );
        assert_eq!(geom(&deck, &sid, "a").0, 0.0);
        assert_eq!(geom(&deck, &sid, "d").0, 300.0);
        let xs = [
            geom(&deck, &sid, "a").0,
            geom(&deck, &sid, "b").0,
            geom(&deck, &sid, "c").0,
            geom(&deck, &sid, "d").0,
        ];
        let gaps = [
            xs[1] - xs[0] - 10.0,
            xs[2] - xs[1] - 10.0,
            xs[3] - xs[2] - 10.0,
        ];
        assert_eq!(gaps[0], gaps[1]);
        assert_eq!(gaps[1], gaps[2]);
    }

    #[test]
    fn distribute_positions_handles_all_same_position() {
        let items: [(f64, f64); 3] = [(0.0, 10.0), (0.0, 10.0), (0.0, 10.0)];
        let out: Vec<f64> = distribute_positions(&items, (0.0, 10.0));
        assert_eq!(out.len(), 3);
        for v in out {
            assert!(v.is_finite(), "degenerate distribute produced {v}");
        }
        let single: [(f64, f64); 1] = [(7.0, 3.0)];
        assert_eq!(distribute_positions(&single, (0.0, 100.0)), vec![7.0]);
    }

    #[test]
    fn too_few_elements_is_invalid() {
        let (mut deck, sid) = three();
        let one = AlignElements {
            target: CanvasTarget::Slide(sid.clone()),
            element_ids: vec!["a".into()],
            op: AlignOp::Align(AlignAxis::Left),
        };
        assert!(matches!(
            one.apply(&mut deck),
            Err(CommandError::InvalidOperation(_))
        ));
        let two = AlignElements {
            target: CanvasTarget::Slide(sid),
            element_ids: vec!["a".into(), "b".into()],
            op: AlignOp::Distribute(DistributeAxis::Horizontal),
        };
        assert!(matches!(
            two.apply(&mut deck),
            Err(CommandError::InvalidOperation(_))
        ));
    }

    #[test]
    fn unknown_id_errors_without_moving_anything() {
        let (mut deck, sid) = three();
        let cmd = AlignElements {
            target: CanvasTarget::Slide(sid.clone()),
            element_ids: vec!["a".into(), "b".into(), "ghost".into()],
            op: AlignOp::Align(AlignAxis::Left),
        };
        assert!(matches!(
            cmd.apply(&mut deck),
            Err(CommandError::ElementNotFound(_))
        ));
        assert_eq!(geom(&deck, &sid, "a").0, 10.0);
        assert_eq!(geom(&deck, &sid, "b").0, 100.0);
    }

    #[test]
    fn inverse_restores_every_original_geometry() {
        let (mut deck, sid) = three();
        let before = [
            geom(&deck, &sid, "a"),
            geom(&deck, &sid, "b"),
            geom(&deck, &sid, "c"),
        ];
        let out = run(
            &mut deck,
            &sid,
            &["a", "b", "c"],
            AlignOp::Align(AlignAxis::Right),
        );
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(geom(&deck, &sid, "a"), before[0]);
        assert_eq!(geom(&deck, &sid, "b"), before[1]);
        assert_eq!(geom(&deck, &sid, "c"), before[2]);
    }

    #[test]
    fn label_differs_between_align_and_distribute() {
        let sid: SlideId = "s".into();
        let align = AlignElements {
            target: CanvasTarget::Slide(sid.clone()),
            element_ids: vec!["a".into(), "b".into()],
            op: AlignOp::Align(AlignAxis::Left),
        };
        let dist = AlignElements {
            target: CanvasTarget::Slide(sid),
            element_ids: vec!["a".into(), "b".into(), "c".into()],
            op: AlignOp::Distribute(DistributeAxis::Vertical),
        };
        assert_eq!(align.label(), "Align Elements");
        assert_eq!(dist.label(), "Distribute Elements");
    }

    #[test]
    fn mixed_parents_and_flex_groups_are_rejected() {
        let (mut deck, sid) = three();
        {
            let slide = deck.slides.get_mut(&sid).unwrap();
            let mut inner_a = text_element("ga", "x");
            inner_a.geometry.width = 10.0;
            let mut inner_b = text_element("gb", "x");
            inner_b.geometry.x = 50.0;
            inner_b.geometry.width = 10.0;
            let mut group = crate::deck::builders::group_element("g", vec![inner_a, inner_b]);
            group.style = ElementStyle::Group(GroupStyle {
                direction: GroupDirection::Row,
                distribution: GroupDistribution::SpaceBetween,
                alignment: GroupAlignment::None,
                scale: 1.0,
            });
            slide.root.children.push(group);
        }
        let mixed = AlignElements {
            target: CanvasTarget::Slide(sid.clone()),
            element_ids: vec!["a".into(), "ga".into()],
            op: AlignOp::Align(AlignAxis::Left),
        };
        assert!(matches!(
            mixed.apply(&mut deck),
            Err(CommandError::InvalidOperation(_))
        ));
        let inside = AlignElements {
            target: CanvasTarget::Slide(sid),
            element_ids: vec!["ga".into(), "gb".into()],
            op: AlignOp::Align(AlignAxis::Left),
        };
        assert!(matches!(
            inside.apply(&mut deck),
            Err(CommandError::InvalidOperation(_))
        ));
    }
}
