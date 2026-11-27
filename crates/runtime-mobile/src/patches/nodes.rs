//! Raw node access for `phoenix.ui`: node snapshots and `onNode` watchers.

use std::sync::atomic::Ordering;

use phoenix_ui_mobile::Props;

use crate::patches::{
   ACTIVE,
   NodeWatcher,
   Patches,
   patches,
   worker::{
      Job,
      queue,
   },
};

/// Matches a Phoenix node's `accessibilityId`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeMatch {
   Exact(String),
   Prefix(String),
}

impl NodeMatch {
   pub fn matches(&self, aid: &str) -> bool {
      match self {
         Self::Exact(expected) => aid == expected,
         Self::Prefix(prefix) => aid.starts_with(prefix.as_str()),
      }
   }
}

/// Snapshot of a live Phoenix node from [`Mirror`], exposed to plugins.
#[derive(Clone, Debug)]
pub struct NodeInfo {
   pub id:               i32,
   pub kind:             Option<String>,
   pub accessibility_id: Option<String>,
   pub parent:           Option<i32>,
   pub children:         Vec<i32>,
   pub sheet:            Option<i32>,
   pub list:             Option<i32>,
   pub on:               Vec<String>,
}

/// Registers a raw node creation watcher (`phoenix.ui.onNode`).
pub fn add_node_watcher(id: u64, plugin: String, matcher: NodeMatch) {
   ACTIVE.store(true, Ordering::Release);
   patches().node_watchers.push(NodeWatcher {
      id,
      plugin,
      matcher,
   });
}

pub fn remove_node_watcher(id: u64) {
   patches().node_watchers.retain(|watcher| watcher.id != id);
}

/// Sends a raw `Op::Update` (`phoenix.ui.update`) on the patches worker thread.
pub fn update_node(node: i32, props: Props) {
   ACTIVE.store(true, Ordering::Release);
   queue(Job::UpdateNode { node, props });
}

/// Finds all live nodes matching `matcher` (`phoenix.ui.find`).
pub fn find_nodes(matcher: &NodeMatch) -> Vec<i32> {
   patches().mirror.nodes_where(&|aid| matcher.matches(aid))
}

/// Finds the first node at or under `root` matching `matcher`
/// (`phoenix.ui.findDown`).
pub fn find_down(root: i32, matcher: &NodeMatch, max_depth: usize) -> Option<i32> {
   patches()
      .mirror
      .find_down(root, &|aid| matcher.matches(aid), max_depth)
}

/// Snapshot of a live node (`phoenix.ui.node`).
pub fn node_info(id: i32) -> Option<NodeInfo> {
   let state = patches();
   state.mirror.exists(id).then(|| state.build_node_info(id))
}

impl Patches {
   pub fn notify_watchers(&self, named: &[i32]) {
      // Notify raw node watchers (`phoenix.ui.onNode`) for matching newly
      // created nodes.
      if !self.node_watchers.is_empty() {
         for id in named {
            let Some(aid) = self.mirror.accessibility_id(*id) else {
               continue;
            };
            let watchers = self
               .node_watchers
               .iter()
               .filter(|watcher| watcher.matcher.matches(aid))
               .map(|watcher| watcher.id)
               .collect::<Vec<_>>();
            if !watchers.is_empty() {
               queue(Job::NodeCreated {
                  watchers,
                  info: self.build_node_info(*id),
               });
            }
         }
      }
   }

   pub fn build_node_info(&self, id: i32) -> NodeInfo {
      let on = self
         .mirror
         .list_on(id)
         .iter()
         .filter_map(|v| v.as_str().map(str::to_owned))
         .collect();
      NodeInfo {
         id,
         kind: self.mirror.kind(id).map(|k| k.as_str().to_owned()),
         accessibility_id: self.mirror.accessibility_id(id).map(str::to_owned),
         parent: self.mirror.parent(id),
         children: self.mirror.kids(id).to_vec(),
         sheet: self.mirror.sheet_of(id),
         list: self.mirror.enclosing_list(id),
         on,
      }
   }
}
