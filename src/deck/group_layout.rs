// Group layout pass — bakes direct-child positions from a group's flex
// properties and shrink-wraps the group box. Operates in group-local child
// coordinates (children are position:absolute within the group).

use crate::deck::element::{ElementNode, ElementStyle, ElementType};
use crate::deck::style::{GroupAlignment, GroupDirection, GroupDistribution, GroupStyle};

// axis_get / axis_set — read/write a child's main/cross coordinate + size by
// direction. Row → main = x, cross = y; Column → main = y, cross = x.
fn main_pos(g: &crate::deck::style::Geometry, dir: GroupDirection) -> f64 {
    match dir { GroupDirection::Row => g.x, GroupDirection::Column => g.y }
}
fn main_size(g: &crate::deck::style::Geometry, dir: GroupDirection) -> f64 {
    match dir { GroupDirection::Row => g.width, GroupDirection::Column => g.height }
}
fn cross_pos(g: &crate::deck::style::Geometry, dir: GroupDirection) -> f64 {
    match dir { GroupDirection::Row => g.y, GroupDirection::Column => g.x }
}
fn cross_size(g: &crate::deck::style::Geometry, dir: GroupDirection) -> f64 {
    match dir { GroupDirection::Row => g.height, GroupDirection::Column => g.width }
}
fn set_main(g: &mut crate::deck::style::Geometry, dir: GroupDirection, v: f64) {
    match dir { GroupDirection::Row => g.x = v, GroupDirection::Column => g.y = v }
}
fn set_cross(g: &mut crate::deck::style::Geometry, dir: GroupDirection, v: f64) {
    match dir { GroupDirection::Row => g.y = v, GroupDirection::Column => g.x = v }
}

// distribute_main — reposition children along the main axis per the mode,
// within the children's current main span. Order = ascending current main pos.
// Output: side-effect on child geometry.
fn distribute_main(children: &mut [ElementNode], dir: GroupDirection, dist: GroupDistribution) {
    let n: usize = children.len();
    assert!(n >= 1, "distribute_main: empty");
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        main_pos(&children[a].geometry, dir)
            .partial_cmp(&main_pos(&children[b].geometry, dir))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let min: f64 = order.iter().map(|&i| main_pos(&children[i].geometry, dir)).fold(f64::INFINITY, f64::min);
    let max_end: f64 = order.iter()
        .map(|&i| main_pos(&children[i].geometry, dir) + main_size(&children[i].geometry, dir))
        .fold(f64::NEG_INFINITY, f64::max);
    let span: f64 = (max_end - min).max(0.0);
    let content: f64 = order.iter().map(|&i| main_size(&children[i].geometry, dir)).sum();
    let free: f64 = (span - content).max(0.0);
    let (lead, gap): (f64, f64) = distribution_offsets(dist, free, n);
    let mut cursor: f64 = min + lead;
    for k in 0..n {
        assert!(k < 100_000, "distribute_main: bound");
        let i: usize = order[k];
        set_main(&mut children[i].geometry, dir, cursor);
        cursor += main_size(&children[i].geometry, dir) + gap;
    }
}

// distribution_offsets — (leading offset, inter-item gap) for a mode given the
// free space and item count.
fn distribution_offsets(dist: GroupDistribution, free: f64, n: usize) -> (f64, f64) {
    let n_f: f64 = n as f64;
    match dist {
        GroupDistribution::Start => (0.0, 0.0),
        GroupDistribution::Center => (free / 2.0, 0.0),
        GroupDistribution::End => (free, 0.0),
        GroupDistribution::SpaceBetween => {
            if n <= 1 { (0.0, 0.0) } else { (0.0, free / (n_f - 1.0)) }
        }
        GroupDistribution::SpaceAround => {
            let gap: f64 = free / n_f;
            (gap / 2.0, gap)
        }
        GroupDistribution::SpaceEvenly => {
            let gap: f64 = free / (n_f + 1.0);
            (gap, gap)
        }
        GroupDistribution::None => (0.0, 0.0),
    }
}

// align_cross — set each child's cross coordinate per the alignment mode within
// the children's current cross span.
fn align_cross(children: &mut [ElementNode], dir: GroupDirection, align: GroupAlignment) {
    let n: usize = children.len();
    assert!(n >= 1, "align_cross: empty");
    let min: f64 = children.iter().map(|c| cross_pos(&c.geometry, dir)).fold(f64::INFINITY, f64::min);
    let max_end: f64 = children.iter()
        .map(|c| cross_pos(&c.geometry, dir) + cross_size(&c.geometry, dir))
        .fold(f64::NEG_INFINITY, f64::max);
    let span: f64 = (max_end - min).max(0.0);
    for k in 0..n {
        assert!(k < 100_000, "align_cross: bound");
        let sz: f64 = cross_size(&children[k].geometry, dir);
        let v: f64 = match align {
            GroupAlignment::Start => min,
            GroupAlignment::Center => min + (span - sz) / 2.0,
            GroupAlignment::End => min + (span - sz),
            GroupAlignment::None => continue,
        };
        set_cross(&mut children[k].geometry, dir, v);
    }
}

// relayout_group
// Inputs: a group node (mutated in place).
// Output: true if any direct-child or group-box geometry changed.
// Errors: none (no-op for non-groups / <1 child). Distribution/alignment apply
// only for non-None modes; shrink-wrap always runs.
pub fn relayout_group(group: &mut ElementNode) -> bool {
    if group.element_type != ElementType::Group || group.children.is_empty() {
        return false;
    }
    let style: GroupStyle = match &group.style {
        ElementStyle::Group(s) => s.clone(),
        _ => return false,
    };
    let before: Vec<crate::deck::style::Geometry> =
        group.children.iter().map(|c| c.geometry.clone()).collect();
    let before_box: crate::deck::style::Geometry = group.geometry.clone();
    if style.distribution != GroupDistribution::None {
        distribute_main(&mut group.children, style.direction, style.distribution);
    }
    if style.alignment != GroupAlignment::None {
        align_cross(&mut group.children, style.direction, style.alignment);
    }
    shrink_wrap(group, style.scale);
    let changed_children: bool = group.children.iter().zip(before.iter())
        .any(|(c, b)| c.geometry != *b);
    changed_children || group.geometry != before_box
}

// shrink_wrap — set the group box to the children bbox, normalize children so
// the bbox starts at (0,0), and shift the group origin by the trimmed offset ×
// scale so the group stays visually anchored.
fn shrink_wrap(group: &mut ElementNode, scale: f64) {
    let n: usize = group.children.len();
    assert!(n >= 1, "shrink_wrap: empty");
    let min_x: f64 = group.children.iter().map(|c| c.geometry.x).fold(f64::INFINITY, f64::min);
    let min_y: f64 = group.children.iter().map(|c| c.geometry.y).fold(f64::INFINITY, f64::min);
    let max_x: f64 = group.children.iter().map(|c| c.geometry.x + c.geometry.width).fold(f64::NEG_INFINITY, f64::max);
    let max_y: f64 = group.children.iter().map(|c| c.geometry.y + c.geometry.height).fold(f64::NEG_INFINITY, f64::max);
    for k in 0..n {
        assert!(k < 100_000, "shrink_wrap: bound");
        group.children[k].geometry.x -= min_x;
        group.children[k].geometry.y -= min_y;
    }
    group.geometry.x += min_x * scale;
    group.geometry.y += min_y * scale;
    group.geometry.width = max_x - min_x;
    group.geometry.height = max_y - min_y;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::deck::builders::{group_element, text_element};

    // child — a text child at (x,y) sized w×h.
    fn child(id: &str, x: f64, y: f64, w: f64, h: f64) -> ElementNode {
        let mut n = text_element(id, "t");
        n.geometry.x = x;
        n.geometry.y = y;
        n.geometry.width = w;
        n.geometry.height = h;
        n
    }
    fn grp(style: GroupStyle, kids: Vec<ElementNode>) -> ElementNode {
        let mut g = group_element("g", kids);
        g.style = ElementStyle::Group(style);
        g
    }

    #[test]
    fn shrinkwrap_none_fits_box_and_normalizes_origin() {
        // Two children at x=10 and x=40 (w=20 each), y=5/y=0.
        let mut g = grp(GroupStyle::default(),
            vec![child("a", 10.0, 5.0, 20.0, 10.0), child("b", 40.0, 0.0, 20.0, 30.0)]);
        let changed = relayout_group(&mut g);
        assert!(changed);
        // bbox: x 10..60 -> w50 ; y 0..30 -> h30. Children normalized so min=0.
        assert_eq!(g.geometry.width, 50.0);
        assert_eq!(g.geometry.height, 30.0);
        assert_eq!(g.children[0].geometry.x, 0.0);  // 10-10
        assert_eq!(g.children[1].geometry.x, 30.0); // 40-10
        assert_eq!(g.children[0].geometry.y, 5.0);  // 5-0
        // group origin shifted by trimmed min (×scale 1.0): +10 x, +0 y.
        assert_eq!(g.geometry.x, 10.0);
        assert_eq!(g.geometry.y, 0.0);
    }

    #[test]
    fn row_space_between_pins_ends_holds_width() {
        // span 0..100 (a at 0 w20, b at 80 w20), third c in middle at 50 w20.
        let style = GroupStyle { distribution: GroupDistribution::SpaceBetween, ..Default::default() };
        let mut g = grp(style, vec![
            child("a", 0.0, 0.0, 20.0, 10.0),
            child("c", 50.0, 0.0, 20.0, 10.0),
            child("b", 80.0, 0.0, 20.0, 10.0)]);
        relayout_group(&mut g);
        // 3 items, content 60, span 100, free 40, gap 20. positions 0,40,80.
        assert_eq!(g.geometry.width, 100.0);
        assert_eq!(g.children[0].geometry.x, 0.0);
        assert_eq!(g.children[1].geometry.x, 40.0);
        assert_eq!(g.children[2].geometry.x, 80.0);
    }

    #[test]
    fn row_space_around_shrinks_box_after_trim() {
        let style = GroupStyle { distribution: GroupDistribution::SpaceAround, ..Default::default() };
        let mut g = grp(style, vec![
            child("a", 0.0, 0.0, 20.0, 10.0),
            child("b", 80.0, 0.0, 20.0, 10.0)]);
        relayout_group(&mut g);
        // 2 items content40 span100 free60 gap=free/n=30 -> half-gap 15 lead/trail.
        // pre-trim positions: a at 15, b at 15+20+30=65. bbox 15..85 width 70.
        // after shrink-wrap+normalize: width 70, a at 0, b at 50.
        assert_eq!(g.geometry.width, 70.0);
        assert_eq!(g.children[0].geometry.x, 0.0);
        assert_eq!(g.children[1].geometry.x, 50.0);
    }

    #[test]
    fn row_align_center_sets_cross_axis() {
        let style = GroupStyle { alignment: GroupAlignment::Center, ..Default::default() };
        let mut g = grp(style, vec![
            child("a", 0.0, 0.0, 20.0, 40.0),
            child("b", 30.0, 0.0, 20.0, 10.0)]);
        relayout_group(&mut g);
        // cross_span = 40 (tallest). b centered: (40-10)/2 = 15.
        assert_eq!(g.children[0].geometry.y, 0.0);
        assert_eq!(g.children[1].geometry.y, 15.0);
        assert_eq!(g.geometry.height, 40.0);
    }

    #[test]
    fn column_space_between_uses_y_axis() {
        let style = GroupStyle {
            direction: GroupDirection::Column,
            distribution: GroupDistribution::SpaceBetween, ..Default::default() };
        let mut g = grp(style, vec![
            child("a", 0.0, 0.0, 10.0, 20.0),
            child("b", 0.0, 80.0, 10.0, 20.0)]);
        relayout_group(&mut g);
        assert_eq!(g.children[0].geometry.y, 0.0);
        assert_eq!(g.children[1].geometry.y, 80.0);
        assert_eq!(g.geometry.height, 100.0);
    }

    #[test]
    fn single_child_and_empty_are_safe() {
        let mut one = grp(GroupStyle { distribution: GroupDistribution::SpaceAround, ..Default::default() },
            vec![child("a", 7.0, 9.0, 20.0, 10.0)]);
        relayout_group(&mut one);
        assert_eq!(one.geometry.width, 20.0);
        let mut empty = grp(GroupStyle::default(), vec![]);
        assert!(!relayout_group(&mut empty));
    }
}
