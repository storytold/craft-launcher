//! A retained document recipe. Both the columns and graph edit these same nodes.
//! No raster output is a substitute for the source or the operation parameters.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const FORMAT: &str = "artcraft-shell-1";
pub const MAX_NODES: usize = 256;
pub const MAX_REVISIONS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Program {
    PhotoCraft,
    VectorCraft,
    LightCraft,
}

impl Program {
    pub const ALL: [Self; 3] = [Self::PhotoCraft, Self::VectorCraft, Self::LightCraft];
    pub fn name(self) -> &'static str {
        match self {
            Self::PhotoCraft => "PhotoCraft",
            Self::VectorCraft => "VectorCraft",
            Self::LightCraft => "LightCraft",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::PhotoCraft => "photocraft",
            Self::VectorCraft => "vectorcraft",
            Self::LightCraft => "lightcraft",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Operation {
    HueSaturation { hue: f32, saturation: f32 },
    Levels { black: f32, white: f32, gamma: f32 },
    Rectangle { x: f32, y: f32, width: f32, height: f32, color: [u8; 3] },
    Exposure { stops: f32 },
}

impl Operation {
    pub fn program(&self) -> Program {
        match self {
            Self::HueSaturation { .. } | Self::Levels { .. } => Program::PhotoCraft,
            Self::Rectangle { .. } => Program::VectorCraft,
            Self::Exposure { .. } => Program::LightCraft,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::HueSaturation { .. } => "Hue / Saturation",
            Self::Levels { .. } => "Levels",
            Self::Rectangle { .. } => "Rectangle",
            Self::Exposure { .. } => "Exposure",
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        let in_range = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        let valid = match *self {
            Self::HueSaturation { hue, saturation } => in_range(hue, -180., 180.) && in_range(saturation, -100., 100.),
            Self::Levels { black, white, gamma } => {
                in_range(black, 0., 253.) && in_range(white, 2., 255.) && black < white && in_range(gamma, 0.01, 9.99)
            }
            Self::Rectangle { x, y, width, height, .. } => {
                in_range(x, -8192., 8192.) && in_range(y, -8192., 8192.) && in_range(width, 1., 8192.) && in_range(height, 1., 8192.)
            }
            Self::Exposure { stops } => in_range(stops, -5., 5.),
        };
        if valid { Ok(()) } else { Err("Invalid operation parameters".into()) }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: u64,
    /// None means the immutable source. A disabled node forwards this input.
    pub input: Option<u64>,
    pub enabled: bool,
    pub operation: Operation,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub output: Option<u64>,
}

impl Graph {
    pub fn append(&mut self, operation: Operation) -> Result<u64, String> {
        operation.validate()?;
        if self.nodes.len() >= MAX_NODES {
            return Err("Document operation limit reached".into());
        }
        let id = self.nodes.iter().map(|n| n.id).max().unwrap_or(0).checked_add(1).ok_or("Operation identifier exhausted")?;
        self.nodes.push(Node { id, input: self.output, enabled: true, operation });
        self.output = Some(id);
        Ok(id)
    }
    pub fn node(&self, id: u64) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }
    pub fn node_mut(&mut self, id: u64) -> Option<&mut Node> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.nodes.len() > MAX_NODES {
            return Err("Too many operations".into());
        }
        let ids: BTreeSet<_> = self.nodes.iter().map(|n| n.id).collect();
        if ids.len() != self.nodes.len() || ids.contains(&0) {
            return Err("Invalid or duplicate operation identifiers".into());
        }
        if self.output.is_some_and(|id| !ids.contains(&id)) {
            return Err("Missing output operation".into());
        }
        for node in &self.nodes {
            node.operation.validate()?;
            let mut visited = BTreeSet::new();
            let mut next = Some(node.id);
            while let Some(id) = next {
                if !visited.insert(id) {
                    return Err("Operation connections contain a cycle".into());
                }
                next = self.node(id).ok_or("An operation input is missing")?.input;
            }
        }
        Ok(())
    }
    /// Only ancestors of the selected output need evaluation, in dependency order.
    pub fn evaluation_order(&self) -> Result<Vec<u64>, String> {
        self.validate()?;
        let mut ids = Vec::new();
        let mut next = self.output;
        while let Some(id) = next {
            ids.push(id);
            next = self.node(id).ok_or("Missing operation")?.input;
        }
        ids.reverse();
        Ok(ids)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineStamp {
    pub version: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub format: String,
    pub name: String,
    /// The original bytes occur once, independently of undo history and preview caches.
    pub source: Vec<u8>,
    pub source_sha256: String,
    pub engines: BTreeMap<Program, EngineStamp>,
    pub revisions: Vec<Graph>,
    pub cursor: usize,
    pub generation: u64,
}

impl Document {
    pub fn new(name: String, source: Vec<u8>, source_sha256: String, engines: BTreeMap<Program, EngineStamp>) -> Self {
        Self { format: FORMAT.into(), name, source, source_sha256, engines, revisions: vec![Graph::default()], cursor: 0, generation: 0 }
    }
    pub fn graph(&self) -> Result<&Graph, String> {
        self.revisions.get(self.cursor).ok_or_else(|| "Invalid history position".into())
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.format != FORMAT {
            return Err("Unsupported document shell version".into());
        }
        if self.source.is_empty() || self.source.len() > 32 * 1024 * 1024 {
            return Err("Source must be between 1 byte and 32 MiB".into());
        }
        if self.revisions.is_empty() || self.revisions.len() > MAX_REVISIONS || self.cursor >= self.revisions.len() {
            return Err("Invalid document history".into());
        }
        for graph in &self.revisions {
            graph.validate()?;
        }
        Ok(())
    }
    /// The expected generation prevents stale engine/UI results overwriting newer edits.
    pub fn commit(&mut self, expected: u64, graph: Graph) -> Result<(), String> {
        if expected != self.generation {
            return Err("The document changed while this operation was running".into());
        }
        graph.validate()?;
        if &graph == self.graph()? {
            return Ok(());
        }
        if self.cursor + 1 >= MAX_REVISIONS {
            return Err("This preview supports at most 128 history revisions; save this document to preserve its history".into());
        }
        let next = self.generation.checked_add(1).ok_or("Revision identifier exhausted")?;
        self.revisions.truncate(self.cursor + 1);
        self.revisions.push(graph);
        self.cursor += 1;
        self.generation = next;
        Ok(())
    }
    pub fn undo(&mut self) -> Result<bool, String> {
        if self.cursor == 0 {
            return Ok(false);
        }
        self.generation = self.generation.checked_add(1).ok_or("Revision identifier exhausted")?;
        self.cursor -= 1;
        Ok(true)
    }
    pub fn redo(&mut self) -> Result<bool, String> {
        if self.cursor + 1 >= self.revisions.len() {
            return Ok(false);
        }
        self.generation = self.generation.checked_add(1).ok_or("Revision identifier exhausted")?;
        self.cursor += 1;
        Ok(true)
    }
}

/// Choice lifetime belongs to an open document/binder, never a global file extension.
#[derive(Default)]
pub struct Choices {
    opened: BTreeMap<String, Program>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Choice {
    Ask,
    Direct(Program),
    Unavailable,
}
impl Choices {
    pub fn route(&self, document_or_binder: &str, compatible: &[Program]) -> Choice {
        if let Some(program) = self.opened.get(document_or_binder).filter(|p| compatible.contains(p)) {
            return Choice::Direct(*program);
        }
        match compatible {
            [] => Choice::Unavailable,
            [one] => Choice::Direct(*one),
            _ => Choice::Ask,
        }
    }
    pub fn choose(&mut self, document_or_binder: String, program: Program) {
        self.opened.insert(document_or_binder, program);
    }
    pub fn close(&mut self, document_or_binder: &str) {
        self.opened.remove(document_or_binder);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn doc() -> Document {
        Document::new("test.png".into(), vec![1], "test".into(), BTreeMap::new())
    }
    #[test]
    fn replacement_ten_steps_back_keeps_shape_and_is_undoable() {
        let mut d = doc();
        let mut g = Graph::default();
        let hue = g.append(Operation::HueSaturation { hue: 30., saturation: 10. }).unwrap();
        let shape = Operation::Rectangle { x: 3., y: 4., width: 10., height: 20., color: [200, 100, 0] };
        g.append(shape.clone()).unwrap();
        for _ in 0..10 {
            g.append(Operation::Exposure { stops: 0.1 }).unwrap();
        }
        d.commit(0, g.clone()).unwrap();
        g.node_mut(hue).unwrap().operation = Operation::Levels { black: 10., white: 240., gamma: 1.2 };
        d.commit(1, g.clone()).unwrap();
        assert_eq!(g.node(2).unwrap().operation, shape);
        assert_eq!(g.evaluation_order().unwrap(), (1..=12).collect::<Vec<_>>());
        d.undo().unwrap();
        assert!(matches!(d.graph().unwrap().node(1).unwrap().operation, Operation::HueSaturation { .. }));
        d.redo().unwrap();
        assert_eq!(d.graph().unwrap(), &g);
        let restored: Document = serde_json::from_slice(&serde_json::to_vec(&d).unwrap()).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored.graph().unwrap(), &g);
    }
    #[test]
    fn graph_rejects_cycles_missing_inputs_and_invalid_levels() {
        let mut g = Graph::default();
        g.append(Operation::Exposure { stops: 1. }).unwrap();
        g.node_mut(1).unwrap().input = Some(1);
        assert!(g.validate().is_err());
        g.node_mut(1).unwrap().input = Some(999);
        assert!(g.validate().is_err());
        assert!(Operation::Levels { black: 230., white: 20., gamma: 1. }.validate().is_err());
    }
    #[test]
    fn stale_commit_does_not_modify_the_document() {
        let mut d = doc();
        let mut g = Graph::default();
        g.append(Operation::Exposure { stops: 1. }).unwrap();
        d.commit(0, g.clone()).unwrap();
        assert!(d.commit(0, Graph::default()).is_err());
        assert_eq!(d.graph().unwrap(), &g);
    }
    #[test]
    fn changing_output_keeps_other_branches_and_can_be_undone() {
        let mut d = doc();
        let mut g = Graph::default();
        g.append(Operation::Exposure { stops: 1. }).unwrap();
        g.append(Operation::Rectangle { x: 2., y: 3., width: 8., height: 8., color: [20, 30, 40] }).unwrap();
        g.append(Operation::HueSaturation { hue: 15., saturation: 5. }).unwrap();
        g.node_mut(3).unwrap().input = Some(1);
        d.commit(0, g.clone()).unwrap();
        assert_eq!(g.evaluation_order().unwrap(), vec![1, 3]);
        g.output = Some(2);
        d.commit(1, g.clone()).unwrap();
        assert_eq!(g.evaluation_order().unwrap(), vec![1, 2]);
        assert_eq!(g.nodes.len(), 3);
        d.undo().unwrap();
        assert_eq!(d.graph().unwrap().evaluation_order().unwrap(), vec![1, 3]);
    }

    #[test]
    fn separate_files_ask_separately_binder_pages_reuse_one_choice() {
        let mut choices = Choices::default();
        assert_eq!(choices.route("first", &Program::ALL), Choice::Ask);
        choices.choose("first".into(), Program::PhotoCraft);
        assert_eq!(choices.route("second", &Program::ALL), Choice::Ask);
        choices.choose("binder".into(), Program::VectorCraft);
        for _ in 0..300 {
            assert_eq!(choices.route("binder", &Program::ALL), Choice::Direct(Program::VectorCraft));
        }
        assert_eq!(choices.route("another binder", &Program::ALL), Choice::Ask);
        choices.close("first");
        assert_eq!(choices.route("first", &Program::ALL), Choice::Ask);
        assert_eq!(choices.route("only", &[Program::LightCraft]), Choice::Direct(Program::LightCraft));
        assert_eq!(choices.route("none", &[]), Choice::Unavailable);
    }
}
