// Table commands.
//
// All table edits — structural (insert/delete row/column, header counts) and
// content (cell text, multi-cell styling) — mutate the TableData carried by an
// Embed-less Table element. Each command snapshots the prior TableData and
// returns a SetTableData inverse that restores it verbatim, so undo is uniform
// regardless of which mutation ran. Every command re-serializes the whole
// element into a single ReplaceElement patch (tables are small; fine-grained
// cell patching is not worth the complexity).

use crate::commands::{Command, CommandError, CommandOutput, resolve_canvas_mut};
use crate::deck::element::{ElementContent, RichText, TableCell, TableData};
use crate::deck::{Canvas, CanvasTarget, ElementId};
use crate::html::serialize::serialize_element;
use crate::ipc::Patch;

// mutate_table
// Inputs: deck, the canvas target, the table element id, a command label, and
// a mutation closure over the element's TableData.
// Output: a CommandOutput whose patch is a ReplaceElement carrying the
// re-serialized element and whose inverse is a SetTableData restoring the prior
// grid.
// Errors: SlideNotFound/ElementNotFound (target/element absent) or
// InvalidOperation (element is not a Table).
fn mutate_table<F>(
    deck: &mut crate::deck::Deck,
    target: &CanvasTarget,
    element_id: &ElementId,
    f: F,
) -> Result<CommandOutput, CommandError>
where
    F: FnOnce(&mut TableData) -> Result<(), CommandError>,
{
    assert!(!target.id().is_empty(), "table command: target id is empty");
    assert!(!element_id.is_empty(), "table command: element_id is empty");
    let canvas = resolve_canvas_mut(deck, target)?;
    let element = canvas
        .find_element_mut(element_id)
        .ok_or_else(|| CommandError::ElementNotFound(element_id.clone()))?;
    let prior: TableData = match &element.content {
        ElementContent::Table(td) => td.clone(),
        _ => {
            return Err(CommandError::InvalidOperation(format!(
                "table command on non-table element {element_id}"
            )));
        }
    };
    if let ElementContent::Table(td) = &mut element.content {
        f(td)?;
    }
    let new_html: String = serialize_element(element);
    canvas.mark_dirty();
    canvas.invalidate_index();

    Ok(CommandOutput {
        patches: vec![Patch::ReplaceElement { element_id: element_id.clone(), new_html }],
        inverse: Box::new(SetTableData {
            target: target.clone(),
            element_id: element_id.clone(),
            data: prior,
        }),
        dirty_targets: vec![target.clone()],
        manifest_dirty: false,
        warnings: Vec::new(),
    })
}

// normalize_grid — force cells to exactly rows×columns (pad with defaults,
// truncate overflow). Keeps the TableData invariant after a structural edit.
fn normalize_grid(td: &mut TableData) {
    td.cells.truncate(td.rows);
    while td.cells.len() < td.rows {
        td.cells.push(Vec::new());
    }
    for row in td.cells.iter_mut() {
        row.truncate(td.columns);
        while row.len() < td.columns {
            row.push(default_cell());
        }
    }
}

fn default_cell() -> TableCell {
    TableCell { content: RichText::new(""), style_overrides: Default::default(), colspan: 1, rowspan: 1 }
}

// SetTableData — replace the whole grid (the universal inverse vehicle). Not
// surfaced to the UI directly; produced as the inverse of every table command.
#[derive(Debug, Clone)]
pub struct SetTableData {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub data: TableData,
}

impl Command for SetTableData {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        let data = self.data.clone();
        mutate_table(deck, &self.target, &self.element_id, move |td| {
            *td = data;
            Ok(())
        })
    }
    fn label(&self) -> &'static str {
        "Edit Table"
    }
    fn requires_remount(&self) -> bool {
        true
    }
}

// InsertTableRow — insert a blank row at `at` (clamped to rows). Header rows
// shift down when the insert lands inside the header band.
#[derive(Debug, Clone)]
pub struct InsertTableRow {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub at: usize,
}

impl Command for InsertTableRow {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        let at: usize = self.at;
        mutate_table(deck, &self.target, &self.element_id, move |td| {
            let pos: usize = at.min(td.rows);
            let row: Vec<TableCell> = (0..td.columns).map(|_| default_cell()).collect();
            td.cells.insert(pos.min(td.cells.len()), row);
            td.rows += 1;
            if pos < td.header_rows {
                td.header_rows += 1;
            }
            normalize_grid(td);
            Ok(())
        })
    }
    fn label(&self) -> &'static str {
        "Insert Row"
    }
    fn requires_remount(&self) -> bool {
        true
    }
}

// DeleteTableRow — remove the row at `at`. Refuses to delete the last row.
#[derive(Debug, Clone)]
pub struct DeleteTableRow {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub at: usize,
}

impl Command for DeleteTableRow {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        let at: usize = self.at;
        mutate_table(deck, &self.target, &self.element_id, move |td| {
            if td.rows <= 1 {
                return Err(CommandError::InvalidOperation("cannot delete the last row".into()));
            }
            let pos: usize = at.min(td.rows - 1);
            td.cells.remove(pos);
            td.rows -= 1;
            if pos < td.header_rows {
                td.header_rows -= 1;
            }
            normalize_grid(td);
            Ok(())
        })
    }
    fn label(&self) -> &'static str {
        "Delete Row"
    }
    fn requires_remount(&self) -> bool {
        true
    }
}

// InsertTableColumn — insert a blank column at `at` (clamped to columns).
#[derive(Debug, Clone)]
pub struct InsertTableColumn {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub at: usize,
}

impl Command for InsertTableColumn {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        let at: usize = self.at;
        mutate_table(deck, &self.target, &self.element_id, move |td| {
            let pos: usize = at.min(td.columns);
            for row in td.cells.iter_mut() {
                row.insert(pos.min(row.len()), default_cell());
            }
            td.columns += 1;
            if pos < td.header_columns {
                td.header_columns += 1;
            }
            normalize_grid(td);
            Ok(())
        })
    }
    fn label(&self) -> &'static str {
        "Insert Column"
    }
    fn requires_remount(&self) -> bool {
        true
    }
}

// DeleteTableColumn — remove the column at `at`. Refuses to delete the last.
#[derive(Debug, Clone)]
pub struct DeleteTableColumn {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub at: usize,
}

impl Command for DeleteTableColumn {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        let at: usize = self.at;
        mutate_table(deck, &self.target, &self.element_id, move |td| {
            if td.columns <= 1 {
                return Err(CommandError::InvalidOperation("cannot delete the last column".into()));
            }
            let pos: usize = at.min(td.columns - 1);
            for row in td.cells.iter_mut() {
                if pos < row.len() {
                    row.remove(pos);
                }
            }
            td.columns -= 1;
            if pos < td.header_columns {
                td.header_columns -= 1;
            }
            normalize_grid(td);
            Ok(())
        })
    }
    fn label(&self) -> &'static str {
        "Delete Column"
    }
    fn requires_remount(&self) -> bool {
        true
    }
}

// SetTableHeaderRows / SetTableHeaderColumns — set the header band size
// (clamped to the grid dimension).
#[derive(Debug, Clone)]
pub struct SetTableHeaderRows {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub count: usize,
}

impl Command for SetTableHeaderRows {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        let count: usize = self.count;
        mutate_table(deck, &self.target, &self.element_id, move |td| {
            td.header_rows = count.min(td.rows);
            Ok(())
        })
    }
    fn label(&self) -> &'static str {
        "Set Header Rows"
    }
    fn requires_remount(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone)]
pub struct SetTableHeaderColumns {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub count: usize,
}

impl Command for SetTableHeaderColumns {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        let count: usize = self.count;
        mutate_table(deck, &self.target, &self.element_id, move |td| {
            td.header_columns = count.min(td.columns);
            Ok(())
        })
    }
    fn label(&self) -> &'static str {
        "Set Header Columns"
    }
    fn requires_remount(&self) -> bool {
        true
    }
}

// SetCellText — replace one cell's plain text. Out-of-range coordinates are a
// no-op mutation (still undoable as a self-restore).
#[derive(Debug, Clone)]
pub struct SetCellText {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub row: usize,
    pub col: usize,
    pub text: String,
}

impl Command for SetCellText {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        let (row, col, text) = (self.row, self.col, self.text.clone());
        mutate_table(deck, &self.target, &self.element_id, move |td| {
            if let Some(cell) = td.cells.get_mut(row).and_then(|r| r.get_mut(col)) {
                cell.content = RichText::new(text);
            }
            Ok(())
        })
    }
    fn label(&self) -> &'static str {
        "Edit Cell"
    }
    fn requires_remount(&self) -> bool {
        true
    }
}

// SetCellStyles — write one CSS property into every listed cell's
// style_overrides (empty value removes it). This is the multi-cell styling
// path: the inspector routes a PropertyChanged here when a cell set is active.
#[derive(Debug, Clone)]
pub struct SetCellStyles {
    pub target: CanvasTarget,
    pub element_id: ElementId,
    pub cells: Vec<(usize, usize)>,
    pub property: String,
    pub value: String,
}

impl Command for SetCellStyles {
    fn apply(&self, deck: &mut crate::deck::Deck) -> Result<CommandOutput, CommandError> {
        assert!(!self.property.is_empty(), "SetCellStyles: empty property");
        let cells = self.cells.clone();
        let property = self.property.clone();
        let value = self.value.clone();
        mutate_table(deck, &self.target, &self.element_id, move |td| {
            for (r, c) in &cells {
                if let Some(cell) = td.cells.get_mut(*r).and_then(|row| row.get_mut(*c)) {
                    if value.is_empty() {
                        cell.style_overrides.remove(&property);
                    } else {
                        cell.style_overrides.insert(property.clone(), value.clone());
                    }
                }
            }
            Ok(())
        })
    }
    fn label(&self) -> &'static str {
        "Style Cells"
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
    use crate::deck::builders::{group_element, table_element};
    use crate::deck::SlideId;

    fn deck_with_table(rows: usize, columns: usize, header_rows: usize) -> (Deck, SlideId, ElementId) {
        let cells: Vec<Vec<TableCell>> =
            (0..rows).map(|_| (0..columns).map(|_| default_cell()).collect()).collect();
        let td = TableData { rows, columns, cells, header_rows, header_columns: 0 };
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        deck.slides.get_mut(&sid).unwrap().root = group_element("rt", vec![table_element("tb", td)]);
        (deck, sid, "tb".into())
    }

    fn grid(deck: &Deck, sid: &SlideId, eid: &str) -> TableData {
        match &deck.slides[sid].find_element(eid).unwrap().content {
            ElementContent::Table(td) => td.clone(),
            _ => panic!("not a table"),
        }
    }

    #[test]
    fn insert_row_grows_and_inverts() {
        let (mut deck, sid, eid) = deck_with_table(2, 3, 1);
        let cmd = InsertTableRow { target: CanvasTarget::Slide(sid.clone()), element_id: eid.clone(), at: 1 };
        let out = cmd.apply(&mut deck).unwrap();
        let g = grid(&deck, &sid, &eid);
        assert_eq!(g.rows, 3);
        assert_eq!(g.cells.len(), 3);
        assert!(g.cells.iter().all(|r| r.len() == 3));
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(grid(&deck, &sid, &eid).rows, 2);
    }

    #[test]
    fn insert_row_inside_header_band_grows_header() {
        let (mut deck, sid, eid) = deck_with_table(3, 2, 2);
        let cmd = InsertTableRow { target: CanvasTarget::Slide(sid.clone()), element_id: eid.clone(), at: 0 };
        cmd.apply(&mut deck).unwrap();
        assert_eq!(grid(&deck, &sid, &eid).header_rows, 3);
    }

    #[test]
    fn delete_last_row_errors() {
        let (mut deck, sid, eid) = deck_with_table(1, 3, 0);
        let err = DeleteTableRow { target: CanvasTarget::Slide(sid), element_id: eid, at: 0 }
            .apply(&mut deck)
            .unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
    }

    #[test]
    fn insert_and_delete_column() {
        let (mut deck, sid, eid) = deck_with_table(2, 2, 0);
        InsertTableColumn { target: CanvasTarget::Slide(sid.clone()), element_id: eid.clone(), at: 1 }
            .apply(&mut deck)
            .unwrap();
        assert_eq!(grid(&deck, &sid, &eid).columns, 3);
        assert!(grid(&deck, &sid, &eid).cells.iter().all(|r| r.len() == 3));
        DeleteTableColumn { target: CanvasTarget::Slide(sid.clone()), element_id: eid.clone(), at: 0 }
            .apply(&mut deck)
            .unwrap();
        assert_eq!(grid(&deck, &sid, &eid).columns, 2);
    }

    #[test]
    fn set_header_rows_clamps() {
        let (mut deck, sid, eid) = deck_with_table(3, 3, 0);
        SetTableHeaderRows { target: CanvasTarget::Slide(sid.clone()), element_id: eid.clone(), count: 9 }
            .apply(&mut deck)
            .unwrap();
        assert_eq!(grid(&deck, &sid, &eid).header_rows, 3);
    }

    #[test]
    fn set_cell_text_and_invert() {
        let (mut deck, sid, eid) = deck_with_table(2, 2, 0);
        let out = SetCellText {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            row: 1,
            col: 1,
            text: "hi".into(),
        }
        .apply(&mut deck)
        .unwrap();
        assert_eq!(grid(&deck, &sid, &eid).cells[1][1].content.plain, "hi");
        out.inverse.apply(&mut deck).unwrap();
        assert_eq!(grid(&deck, &sid, &eid).cells[1][1].content.plain, "");
    }

    #[test]
    fn set_cell_styles_applies_to_all_listed_and_inverts() {
        let (mut deck, sid, eid) = deck_with_table(2, 2, 0);
        let out = SetCellStyles {
            target: CanvasTarget::Slide(sid.clone()),
            element_id: eid.clone(),
            cells: vec![(0, 0), (1, 1)],
            property: "background-color".into(),
            value: "#eee".into(),
        }
        .apply(&mut deck)
        .unwrap();
        let g = grid(&deck, &sid, &eid);
        assert_eq!(g.cells[0][0].style_overrides.get("background-color").map(String::as_str), Some("#eee"));
        assert_eq!(g.cells[1][1].style_overrides.get("background-color").map(String::as_str), Some("#eee"));
        assert!(g.cells[0][1].style_overrides.is_empty());
        out.inverse.apply(&mut deck).unwrap();
        assert!(grid(&deck, &sid, &eid).cells[0][0].style_overrides.is_empty());
    }

    #[test]
    fn set_cell_styles_empty_value_removes_property() {
        let (mut deck, sid, eid) = deck_with_table(1, 1, 0);
        let t = CanvasTarget::Slide(sid.clone());
        SetCellStyles { target: t.clone(), element_id: eid.clone(), cells: vec![(0, 0)], property: "color".into(), value: "#111".into() }
            .apply(&mut deck)
            .unwrap();
        SetCellStyles { target: t, element_id: eid.clone(), cells: vec![(0, 0)], property: "color".into(), value: String::new() }
            .apply(&mut deck)
            .unwrap();
        assert!(grid(&deck, &sid, &eid).cells[0][0].style_overrides.is_empty());
    }

    #[test]
    fn table_command_errors_on_non_table() {
        let mut deck = Deck::sample();
        let sid: SlideId = deck.slide_order[0].clone();
        let eid: ElementId = deck.slides[&sid].root.children[0].id.clone();
        let err = InsertTableRow { target: CanvasTarget::Slide(sid), element_id: eid, at: 0 }
            .apply(&mut deck)
            .unwrap_err();
        assert!(matches!(err, CommandError::InvalidOperation(_)));
    }
}
