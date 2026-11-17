//! A custom `gio::ListModel` over a Rust row array (PLAN §5.3).
//!
//! Rows are node ids into a node table. `item(i)` creates a tiny `RowObject`
//! holding only the node id, so items stay valid when rows are spliced.

use std::cell::RefCell;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

/// Display data of one synthetic node.
pub struct Node {
    pub name: String,
    pub depth: u16,
    pub is_dir: bool,
    pub size: u64,
    /// Age in seconds relative to "now".
    pub age: u32,
}

#[derive(Default)]
pub struct Data {
    pub nodes: Vec<Node>,
    pub rows: Vec<u32>,
}

mod row_imp {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    pub struct RowObject {
        pub node: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowObject {
        const NAME: &'static str = "SpikeRowObject";
        type Type = super::RowObject;
    }

    impl ObjectImpl for RowObject {}
}

glib::wrapper! {
    pub struct RowObject(ObjectSubclass<row_imp::RowObject>);
}

impl RowObject {
    fn new(node: u32) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().node.set(node);
        obj
    }

    pub fn node(&self) -> u32 {
        self.imp().node.get()
    }
}

mod model_imp {
    use super::*;

    #[derive(Default)]
    pub struct RowModel {
        pub data: RefCell<Data>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RowModel {
        const NAME: &'static str = "SpikeRowModel";
        type Type = super::RowModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for RowModel {}

    impl ListModelImpl for RowModel {
        fn item_type(&self) -> glib::Type {
            RowObject::static_type()
        }

        fn n_items(&self) -> u32 {
            self.data.borrow().rows.len() as u32
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            let id = *self.data.borrow().rows.get(position as usize)?;
            Some(RowObject::new(id).upcast())
        }
    }
}

glib::wrapper! {
    pub struct RowModel(ObjectSubclass<model_imp::RowModel>)
        @implements gio::ListModel;
}

const EN: &[&str] = &[
    "report", "contract", "invoice", "budget", "meeting", "notes", "draft", "final", "summary",
    "plan", "design", "spec", "review", "slides", "data", "export", "backup", "photo", "scan",
];
const CJK: &[&str] = &[
    "合同", "核对", "季报", "年报", "发票", "会议", "纪要", "报告", "总结", "计划", "方案",
];
const EXT: &[&str] = &[
    "pdf", "docx", "xlsx", "txt", "md", "jpg", "png", "rs", "py", "zip",
];

/// Tiny deterministic PRNG (xorshift64*), enough for synthetic rows.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn synthetic_node(rng: &mut Rng, i: usize, depth: u16) -> Node {
    let is_dir = rng.below(8) == 0;
    let stem = if rng.below(5) == 0 {
        format!("{}{}", CJK[rng.below(CJK.len())], CJK[rng.below(CJK.len())])
    } else {
        format!("{}-{}", EN[rng.below(EN.len())], EN[rng.below(EN.len())])
    };
    let name = if is_dir {
        format!("{stem}{i}")
    } else {
        format!("{stem}{i}.{}", EXT[rng.below(EXT.len())])
    };
    Node {
        name,
        depth,
        is_dir,
        size: if is_dir { 0 } else { rng.next() % 50_000_000 },
        age: (rng.next() % (3 * 365 * 86_400)) as u32,
    }
}

impl RowModel {
    /// A model with `n` rows forming a plausible expanded tree (depth 0..=5).
    pub fn synthetic(n: usize) -> Self {
        let model: Self = glib::Object::new();
        {
            let mut d = model.imp().data.borrow_mut();
            let mut rng = Rng::new(20_261_118);
            let mut depth = 0u16;
            for i in 0..n {
                let node = synthetic_node(&mut rng, i, depth);
                if node.is_dir && depth < 5 {
                    depth += 1;
                } else if depth > 0 && rng.below(40) == 0 {
                    depth -= 1;
                }
                d.nodes.push(node);
                d.rows.push(i as u32);
            }
        }
        model
    }

    pub fn data(&self) -> std::cell::Ref<'_, Data> {
        self.imp().data.borrow()
    }

    /// Simulated expand: splice `count` new child rows after `position`.
    pub fn insert_children(&self, position: usize, count: usize, seed: u64) {
        {
            let mut d = self.imp().data.borrow_mut();
            let depth = d.nodes[d.rows[position] as usize].depth + 1;
            let mut rng = Rng::new(seed);
            let first = d.nodes.len();
            for k in 0..count {
                let node = synthetic_node(&mut rng, first + k, depth);
                d.nodes.push(node);
            }
            let ids = (first..first + count).map(|i| i as u32);
            d.rows.splice(position + 1..position + 1, ids);
        }
        self.items_changed(position as u32 + 1, 0, count as u32);
    }

    /// Simulated collapse: remove `count` rows after `position`.
    pub fn remove_children(&self, position: usize, count: usize) {
        {
            let mut d = self.imp().data.borrow_mut();
            d.rows.drain(position + 1..position + 1 + count);
        }
        self.items_changed(position as u32 + 1, count as u32, 0);
    }
}
