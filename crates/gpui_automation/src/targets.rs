//! Named automation targets: `.automation_target("sidebar/folder/<id>")` on
//! any element lets scripts address it by name (`click{target}`,
//! `bounds{target}`, `query{prefix}`) instead of by brittle coordinates.
//!
//! Cost model: when automation is off the extension returns the element
//! unchanged (as an [`AnyElement`]). When it is on, the wrapper records its
//! bounds during prepaint only while a capture is armed for its window, i.e.
//! during the single frame a target command asks for; otherwise prepaint
//! pays one relaxed atomic load.
//!
//! A capture refreshes the window (which bypasses view caching so every
//! target is prepainted), arms the registry at the start of the next frame
//! and collects at the start of the one after, so exactly one full draw is
//! recorded and targets that are no longer rendered simply don't appear.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, SharedString, Window,
};
use serde_json::{Value, json};

/// Extension for naming elements so automation scripts can find them.
pub trait AutomationTargetExt: IntoElement + Sized {
    /// Register this element under `name` while automation is active. Names
    /// are hierarchical by convention (`"sidebar/story/42"`) so `query`
    /// can list a whole group by prefix.
    fn automation_target(self, name: impl Into<SharedString>) -> AnyElement {
        if crate::is_active() {
            AutomationTarget {
                name: name.into(),
                child: self.into_any_element(),
            }
            .into_any_element()
        } else {
            self.into_any_element()
        }
    }

    /// Like [`Self::automation_target`], but only builds the name when
    /// automation is active, so a formatted name costs nothing otherwise.
    fn automation_target_with(self, name: impl FnOnce() -> SharedString) -> AnyElement {
        if crate::is_active() {
            AutomationTarget {
                name: name(),
                child: self.into_any_element(),
            }
            .into_any_element()
        } else {
            self.into_any_element()
        }
    }
}

impl<T: IntoElement> AutomationTargetExt for T {}

/// Layout-transparent wrapper: it reuses its child's layout node, so wrapping
/// never changes what is drawn.
struct AutomationTarget {
    name: SharedString,
    child: AnyElement,
}

impl IntoElement for AutomationTarget {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for AutomationTarget {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if ARMED.load(Ordering::Relaxed) > 0 {
            let window_id = window.window_handle().window_id().as_u64();
            let visible = bounds.intersect(&window.content_mask().bounds);
            record(
                window_id,
                TargetRecord {
                    name: self.name.clone(),
                    bounds,
                    visible: (visible.size.width > Pixels::ZERO
                        && visible.size.height > Pixels::ZERO)
                        .then_some(visible),
                },
            );
        }
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

/// One prepaint of a named element, in window coordinates (logical px).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TargetRecord {
    pub name: SharedString,
    pub bounds: Bounds<Pixels>,
    /// The part not clipped away by scroll containers or `overflow_hidden`
    /// ancestors, `None` when fully clipped.
    pub visible: Option<Bounds<Pixels>>,
}

/// Number of windows with an armed capture; the prepaint fast path.
static ARMED: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Armed captures by window id. Prepaint and the command loop both run on
    /// the main thread, so a thread-local needs no locking.
    static CAPTURES: RefCell<HashMap<u64, Vec<TargetRecord>>> = RefCell::new(HashMap::new());
}

/// Start recording targets for `window_id`, discarding any stale records.
pub(crate) fn arm(window_id: u64) {
    CAPTURES.with(|captures| {
        if captures
            .borrow_mut()
            .insert(window_id, Vec::new())
            .is_none()
        {
            ARMED.fetch_add(1, Ordering::Relaxed);
        }
    });
}

/// Stop recording for `window_id` and return what was recorded, `None` if
/// it was not armed.
pub(crate) fn take(window_id: u64) -> Option<Vec<TargetRecord>> {
    CAPTURES.with(|captures| {
        let records = captures.borrow_mut().remove(&window_id);
        if records.is_some() {
            ARMED.fetch_sub(1, Ordering::Relaxed);
        }
        records
    })
}

/// Whether a capture is armed for some window: the frame a target command asks for is being
/// drawn. Elements that name parts of themselves with [`record_target`] (characters of a line
/// of text, which have no element each) check this first, so they compute nothing otherwise.
pub fn is_capturing() -> bool {
    ARMED.load(Ordering::Relaxed) > 0
}

/// Records a target for a region no element of its own stands for, at `bounds` in window
/// coordinates, during prepaint (the clip in effect then decides what is visible). Does nothing
/// unless a capture is armed for `window` ([`is_capturing`]).
pub fn record_target(window: &Window, name: impl FnOnce() -> SharedString, bounds: Bounds<Pixels>) {
    if !is_capturing() {
        return;
    }
    let window_id = window.window_handle().window_id().as_u64();
    let visible = bounds.intersect(&window.content_mask().bounds);
    record(
        window_id,
        TargetRecord {
            name: name(),
            bounds,
            visible: (visible.size.width > Pixels::ZERO && visible.size.height > Pixels::ZERO)
                .then_some(visible),
        },
    );
}

fn record(window_id: u64, record: TargetRecord) {
    CAPTURES.with(|captures| {
        if let Some(records) = captures.borrow_mut().get_mut(&window_id) {
            records.push(record);
        }
    });
}

/// The targets of one captured frame.
#[derive(Debug, Clone, Default)]
pub(crate) struct TargetSet {
    /// Distinct names in first-prepaint order; each keeps its last record
    /// and how many times it was prepainted.
    entries: Vec<(TargetRecord, usize)>,
}

impl TargetSet {
    pub fn new(records: Vec<TargetRecord>) -> Self {
        let mut entries: Vec<(TargetRecord, usize)> = Vec::new();
        let mut index: HashMap<SharedString, usize> = HashMap::new();
        for record in records {
            match index.get(&record.name) {
                // A later prepaint wins: an element prepainted twice in a
                // frame (`window.transact` retries) ends where it was last put.
                Some(&ix) => {
                    entries[ix].0 = record;
                    entries[ix].1 += 1;
                }
                None => {
                    index.insert(record.name.clone(), entries.len());
                    entries.push((record, 1));
                }
            }
        }
        Self { entries }
    }

    pub fn get(&self, name: &str) -> Option<(&TargetRecord, usize)> {
        self.entries
            .iter()
            .find(|(record, _)| record.name.as_ref() == name)
            .map(|(record, count)| (record, *count))
    }

    pub fn with_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> impl Iterator<Item = (&'a TargetRecord, usize)> + 'a {
        self.entries
            .iter()
            .filter(move |(record, _)| record.name.starts_with(prefix))
            .map(|(record, count)| (record, *count))
    }

    /// Names close to `name`, for "did you mean" errors.
    pub fn suggestions(&self, name: &str) -> Vec<&str> {
        let stem = name.rsplit_once('/').map_or("", |(stem, _)| stem);
        let mut out: Vec<&str> = self
            .entries
            .iter()
            .map(|(record, _)| record.name.as_ref())
            .filter(|candidate| !stem.is_empty() && candidate.starts_with(stem))
            .take(8)
            .collect();
        if out.is_empty() {
            out = self
                .entries
                .iter()
                .map(|(record, _)| record.name.as_ref())
                .take(8)
                .collect();
        }
        out
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/**
 * A rect in logical pixels with each edge rounded to a whole pixel. gpui snaps layout to device
 * pixels, so at a fractional scale (Windows at 150 %, a device pixel is 2/3 of a logical one) a
 * box laid out at x = 293 reads back as 292.67. Rounding the edges, then taking the size from
 * them, gives back the integer layout that scenarios and the design boards use; rounding the
 * sizes instead would be off by one wherever both edges moved the same way.
 */
pub(crate) fn rect_json(bounds: Bounds<Pixels>) -> Value {
    let left = f32::from(bounds.origin.x).round();
    let top = f32::from(bounds.origin.y).round();
    let right = f32::from(bounds.origin.x + bounds.size.width).round();
    let bottom = f32::from(bounds.origin.y + bounds.size.height).round();
    json!({ "x": left, "y": top, "w": right - left, "h": bottom - top })
}

pub(crate) fn target_json(record: &TargetRecord, count: usize) -> Value {
    let mut value = rect_json(record.bounds);
    value["target"] = Value::String(record.name.to_string());
    value["visible"] = record.visible.map(rect_json).unwrap_or(Value::Null);
    value["count"] = json!(count);
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};
    use pretty_assertions::assert_eq;

    fn rec(name: &str, x: f32, visible: bool) -> TargetRecord {
        let bounds = Bounds::new(point(px(x), px(10.)), size(px(20.), px(5.)));
        TargetRecord {
            name: SharedString::from(name.to_string()),
            bounds,
            visible: visible.then_some(bounds),
        }
    }

    #[test]
    fn arm_record_take_round_trip() {
        let window = 0xfeed_0001;
        record(window, rec("ignored", 0., true));
        assert_eq!(take(window), None, "unarmed windows record nothing");

        arm(window);
        record(window, rec("a", 1., true));
        arm(window);
        record(window, rec("b", 2., true));
        record(window + 1, rec("other window", 3., true));
        let records = take(window).unwrap();
        assert_eq!(
            records,
            vec![rec("b", 2., true)],
            "re-arming discards stale records"
        );
        assert_eq!(take(window), None);
    }

    #[test]
    fn later_prepaints_win_and_are_counted() {
        let set = TargetSet::new(vec![
            rec("list/row/1", 1., true),
            rec("list/row/2", 2., false),
            rec("list/row/1", 3., true),
            rec("item", 4., true),
        ]);
        assert_eq!(set.len(), 3);
        let (row, count) = set.get("list/row/1").unwrap();
        assert_eq!(count, 2);
        assert_eq!(f32::from(row.bounds.origin.x), 3.);
        assert!(set.get("list/row/9").is_none());

        let rows: Vec<&str> = set
            .with_prefix("list/")
            .map(|(record, _)| record.name.as_ref())
            .collect();
        assert_eq!(rows, ["list/row/1", "list/row/2"]);
        assert_eq!(set.with_prefix("").count(), 3);
    }

    #[test]
    fn suggestions_prefer_siblings() {
        let set = TargetSet::new(vec![
            rec("list/row/1", 1., true),
            rec("list/row/2", 2., true),
            rec("item", 3., true),
        ]);
        assert_eq!(set.suggestions("list/row/7"), ["list/row/1", "list/row/2"]);
        assert_eq!(
            set.suggestions("nothing"),
            ["list/row/1", "list/row/2", "item"]
        );
    }

    #[test]
    fn json_shape() {
        let value = target_json(&rec("a", 1., false), 1);
        assert_eq!(
            value,
            json!({"target": "a", "x": 1.0, "y": 10.0, "w": 20.0, "h": 5.0, "visible": null, "count": 1})
        );
    }

    #[test]
    fn rects_round_their_edges_at_fractional_scales() {
        /* empty_window's drop zone at 150 %: laid out at 293, 81, 1119 × 750 on device pixels. */
        let bounds = Bounds::new(
            point(px(292.667), px(80.667)),
            size(px(1119.333), px(750.667)),
        );
        assert_eq!(
            rect_json(bounds),
            json!({"x": 293.0, "y": 81.0, "w": 1119.0, "h": 750.0})
        );
    }
}
