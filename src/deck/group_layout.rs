use crate::deck::element::{ElementNode, ElementStyle, ElementType};
use crate::deck::style::{GroupAlignment, GroupDirection, GroupDistribution, GroupStyle};

fn main_pos(g: &crate::deck::style::Geometry, dir: GroupDirection) -> f64 {
    match dir {
        GroupDirection::Row => g.x,
        GroupDirection::Column => g.y,
    }
}
fn main_size(g: &crate::deck::style::Geometry, dir: GroupDirection) -> f64 {
    match dir {
        GroupDirection::Row => g.width,
        GroupDirection::Column => g.height,
    }
}
fn cross_pos(g: &crate::deck::style::Geometry, dir: GroupDirection) -> f64 {
    match dir {
        GroupDirection::Row => g.y,
        GroupDirection::Column => g.x,
    }
}
fn cross_size(g: &crate::deck::style::Geometry, dir: GroupDirection) -> f64 {
    match dir {
        GroupDirection::Row => g.height,
        GroupDirection::Column => g.width,
    }
}
fn set_main(g: &mut crate::deck::style::Geometry, dir: GroupDirection, v: f64) {
    match dir {
        GroupDirection::Row => g.x = v,
        GroupDirection::Column => g.y = v,
    }
}
fn set_cross(g: &mut crate::deck::style::Geometry, dir: GroupDirection, v: f64) {
    match dir {
        GroupDirection::Row => g.y = v,
        GroupDirection::Column => g.x = v,
    }
}

fn distribute_main(children: &mut [ElementNode], dir: GroupDirection, dist: GroupDistribution) {
    let n: usize = children.len();
    assert!(n >= 1, "distribute_main: empty");
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| {
        main_pos(&children[a].geometry, dir)
            .partial_cmp(&main_pos(&children[b].geometry, dir))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let min: f64 = order
        .iter()
        .map(|&i| main_pos(&children[i].geometry, dir))
        .fold(f64::INFINITY, f64::min);
    let max_end: f64 = order
        .iter()
        .map(|&i| main_pos(&children[i].geometry, dir) + main_size(&children[i].geometry, dir))
        .fold(f64::NEG_INFINITY, f64::max);
    let span: f64 = (max_end - min).max(0.0);
    let content: f64 = order
        .iter()
        .map(|&i| main_size(&children[i].geometry, dir))
        .sum();
    let free: f64 = (span - content).max(0.0);
    let (lead, gap): (f64, f64) = distribution_offsets(dist, free, n);
    let mut cursor: f64 = min + lead;
    for (k, i) in order.iter().enumerate().take(n) {
        assert!(k < 100_000, "distribute_main: bound");
        set_main(&mut children[*i].geometry, dir, cursor);
        cursor += main_size(&children[*i].geometry, dir) + gap;
    }
}

fn distribution_offsets(dist: GroupDistribution, free: f64, n: usize) -> (f64, f64) {
    let n_f: f64 = n as f64;
    match dist {
        GroupDistribution::Start => (0.0, 0.0),
        GroupDistribution::Center => (free / 2.0, 0.0),
        GroupDistribution::End => (free, 0.0),
        GroupDistribution::SpaceBetween => {
            if n <= 1 {
                (0.0, 0.0)
            } else {
                (0.0, free / (n_f - 1.0))
            }
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

fn align_cross(children: &mut [ElementNode], dir: GroupDirection, align: GroupAlignment) {
    let n: usize = children.len();
    assert!(n >= 1, "align_cross: empty");
    let min: f64 = children
        .iter()
        .map(|c| cross_pos(&c.geometry, dir))
        .fold(f64::INFINITY, f64::min);
    let max_end: f64 = children
        .iter()
        .map(|c| cross_pos(&c.geometry, dir) + cross_size(&c.geometry, dir))
        .fold(f64::NEG_INFINITY, f64::max);
    let span: f64 = (max_end - min).max(0.0);
    for (k, i) in children.iter_mut().enumerate().take(n) {
        assert!(k < 100_000, "align_cross: bound");
        let sz: f64 = cross_size(&i.geometry, dir);
        let v: f64 = match align {
            GroupAlignment::Start => min,
            GroupAlignment::Center => min + (span - sz) / 2.0,
            GroupAlignment::End => min + (span - sz),
            GroupAlignment::None => continue,
        };
        set_cross(&mut i.geometry, dir, v);
    }
}

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
    let changed_children: bool = group
        .children
        .iter()
        .zip(before.iter())
        .any(|(c, b)| c.geometry != *b);
    changed_children || group.geometry != before_box
}

fn shrink_wrap(group: &mut ElementNode, scale: f64) {
    let n: usize = group.children.len();
    assert!(n >= 1, "shrink_wrap: empty");
    let min_x: f64 = group
        .children
        .iter()
        .map(|c| c.geometry.x)
        .fold(f64::INFINITY, f64::min);
    let min_y: f64 = group
        .children
        .iter()
        .map(|c| c.geometry.y)
        .fold(f64::INFINITY, f64::min);
    let max_x: f64 = group
        .children
        .iter()
        .map(|c| c.geometry.x + c.geometry.width)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_y: f64 = group
        .children
        .iter()
        .map(|c| c.geometry.y + c.geometry.height)
        .fold(f64::NEG_INFINITY, f64::max);
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

fn ancestor_group_ids(root: &ElementNode, element_id: &str) -> Vec<String> {
    const MAX_NODES: usize = 1_000_000;
    let root_id: &str = &root.id;
    let mut stack: Vec<(&ElementNode, Vec<String>)> = vec![(root, Vec::new())];
    let mut seen: usize = 0;
    while let Some((node, path)) = stack.pop() {
        seen += 1;
        assert!(seen <= MAX_NODES, "ancestor_group_ids: node ceiling");
        if node.id == element_id {
            let mut chain: Vec<String> = path;
            chain.reverse();

            if node.element_type == ElementType::Group && node.id != root_id {
                chain.insert(0, node.id.clone());
            }
            return chain;
        }
        for child in &node.children {
            let mut next: Vec<String> = path.clone();
            if node.element_type == ElementType::Group && node.id != root_id {
                next.push(node.id.clone());
            }
            stack.push((child, next));
        }
    }
    Vec::new()
}

fn own_scale(n: &ElementNode) -> f64 {
    match &n.style {
        ElementStyle::Group(g) => g.scale,
        _ => 1.0,
    }
}

pub fn element_frame(root: &ElementNode, id: &str) -> Option<(f64, f64, f64, f64)> {
    const MAX_NODES: usize = 1_000_000;
    let mut stack: Vec<(&ElementNode, f64, f64, f64)> = vec![(root, 0.0, 0.0, 1.0)];
    let mut seen: usize = 0;
    while let Some((n, ox, oy, s)) = stack.pop() {
        seen += 1;
        assert!(seen <= MAX_NODES, "element_frame: node ceiling");
        let abs_x: f64 = ox + n.geometry.x * s;
        let abs_y: f64 = oy + n.geometry.y * s;
        let own: f64 = own_scale(n);
        if n.id == id {
            return Some((abs_x, abs_y, s, own));
        }
        let child_s: f64 = s * own;
        for c in &n.children {
            stack.push((c, abs_x, abs_y, child_s));
        }
    }
    None
}

pub fn relayout_ancestors(root: &mut ElementNode, element_id: &str) -> bool {
    let chain: Vec<String> = ancestor_group_ids(root, element_id);
    let mut changed: bool = false;
    for (i, gid) in chain.iter().enumerate() {
        assert!(i < 100_000, "relayout_ancestors: bound");
        if let Some(g) = crate::deck::canvas::find_element_mut(root, gid) {
            changed = relayout_group(g) || changed;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::deck::builders::{group_element, text_element};

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
        let mut g = grp(
            GroupStyle::default(),
            vec![
                child("a", 10.0, 5.0, 20.0, 10.0),
                child("b", 40.0, 0.0, 20.0, 30.0),
            ],
        );
        let changed = relayout_group(&mut g);
        assert!(changed);

        assert_eq!(g.geometry.width, 50.0);
        assert_eq!(g.geometry.height, 30.0);
        assert_eq!(g.children[0].geometry.x, 0.0);
        assert_eq!(g.children[1].geometry.x, 30.0);
        assert_eq!(g.children[0].geometry.y, 5.0);

        assert_eq!(g.geometry.x, 10.0);
        assert_eq!(g.geometry.y, 0.0);
    }

    #[test]
    fn row_space_between_pins_ends_holds_width() {
        let style = GroupStyle {
            distribution: GroupDistribution::SpaceBetween,
            ..Default::default()
        };
        let mut g = grp(
            style,
            vec![
                child("a", 0.0, 0.0, 20.0, 10.0),
                child("c", 50.0, 0.0, 20.0, 10.0),
                child("b", 80.0, 0.0, 20.0, 10.0),
            ],
        );
        relayout_group(&mut g);

        assert_eq!(g.geometry.width, 100.0);
        assert_eq!(g.children[0].geometry.x, 0.0);
        assert_eq!(g.children[1].geometry.x, 40.0);
        assert_eq!(g.children[2].geometry.x, 80.0);
    }

    #[test]
    fn row_space_around_shrinks_box_after_trim() {
        let style = GroupStyle {
            distribution: GroupDistribution::SpaceAround,
            ..Default::default()
        };
        let mut g = grp(
            style,
            vec![
                child("a", 0.0, 0.0, 20.0, 10.0),
                child("b", 80.0, 0.0, 20.0, 10.0),
            ],
        );
        relayout_group(&mut g);

        assert_eq!(g.geometry.width, 70.0);
        assert_eq!(g.children[0].geometry.x, 0.0);
        assert_eq!(g.children[1].geometry.x, 50.0);
    }

    #[test]
    fn row_align_center_sets_cross_axis() {
        let style = GroupStyle {
            alignment: GroupAlignment::Center,
            ..Default::default()
        };
        let mut g = grp(
            style,
            vec![
                child("a", 0.0, 0.0, 20.0, 40.0),
                child("b", 30.0, 0.0, 20.0, 10.0),
            ],
        );
        relayout_group(&mut g);

        assert_eq!(g.children[0].geometry.y, 0.0);
        assert_eq!(g.children[1].geometry.y, 15.0);
        assert_eq!(g.geometry.height, 40.0);
    }

    #[test]
    fn column_space_between_uses_y_axis() {
        let style = GroupStyle {
            direction: GroupDirection::Column,
            distribution: GroupDistribution::SpaceBetween,
            ..Default::default()
        };
        let mut g = grp(
            style,
            vec![
                child("a", 0.0, 0.0, 10.0, 20.0),
                child("b", 0.0, 80.0, 10.0, 20.0),
            ],
        );
        relayout_group(&mut g);
        assert_eq!(g.children[0].geometry.y, 0.0);
        assert_eq!(g.children[1].geometry.y, 80.0);
        assert_eq!(g.geometry.height, 100.0);
    }

    #[test]
    fn single_child_and_empty_are_safe() {
        let mut one = grp(
            GroupStyle {
                distribution: GroupDistribution::SpaceAround,
                ..Default::default()
            },
            vec![child("a", 7.0, 9.0, 20.0, 10.0)],
        );
        relayout_group(&mut one);
        assert_eq!(one.geometry.width, 20.0);
        let mut empty = grp(GroupStyle::default(), vec![]);
        assert!(!relayout_group(&mut empty));
    }

    #[test]
    fn relayout_ancestors_runs_bottom_up_for_nested_groups() {
        let inner = {
            let mut g = grp(
                GroupStyle {
                    distribution: GroupDistribution::SpaceBetween,
                    ..Default::default()
                },
                vec![
                    child("a", 0.0, 0.0, 20.0, 10.0),
                    child("b", 80.0, 0.0, 20.0, 10.0),
                ],
            );
            g.id = "inner".into();
            g
        };
        let mut outer = group_element("outer", vec![inner, child("c", 0.0, 40.0, 10.0, 10.0)]);
        outer.style = ElementStyle::Group(GroupStyle::default());
        let changed = relayout_ancestors(&mut outer, "a");
        assert!(changed);

        let inner_node = outer.children.iter().find(|n| n.id == "inner").unwrap();
        assert_eq!(inner_node.geometry.width, 100.0);
    }
}
